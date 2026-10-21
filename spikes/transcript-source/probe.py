#!/usr/bin/env python3
"""Spike: can a journal turn record point at oneharness's history file instead
of carrying a bounded copy of its content?

Two parts, both driven by `probe.sh`:

* ``live`` runs one real two-party turn (`kind: onejudge`, judged by the free
  command judge beside this file) and one real single-sided turn
  (`kind: oneharness`) through the installed `oneagentgraph`, with history on and
  a pointer file of the probe's own. While each runs it timestamps every journal
  line as it arrives on stdout and every history line as it first appears on
  disk. Afterwards it resolves each `turn-started`, `turn-message` and
  `turn-activity` record from the history file by the candidate reference —
  the pointer line written before the record (history file + history id) plus
  the record's own `index` / `tool_call_id`, and for the instruction the closing
  run line's `prompt` — and prints whether the resolved text equals the text the
  probe knows the turn really carried, plus the append lag.

* ``recorded`` reads this host's recorded runs read-only
  (`<runs>/*/events.jsonl` and `oneharness-sessions.jsonl`) for which member
  kinds have history and a journal pointer, whether the files outlived their
  worktrees and scratch, and whether a change-request drafter whose
  `turn-started` says `instruction_truncated: true` was handed the whole prompt.

Nothing here writes outside the probe's scratch directory, except what a real
oneharness run always writes: its history file in the default store.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import secrets
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
JOURNAL_BOUND = 4096
DETAIL_BOUND = 160
TURN_KINDS = ("turn-started", "turn-message", "turn-activity")


# --------------------------------------------------------------------- live


def prompt_for(nonce: str) -> tuple[str, str, str, str]:
    """The prompt, the exact command it asks for, and what that command prints
    and what the reply should be — all longer than the journal's bounds."""
    command = (
        "python3 -c \"[print('probe-%s-%04d ' % ('" + nonce + "', i) + 'abcdefghij' * 4)"
        " for i in range(120)]\"  # transcript-source probe: this trailing comment makes the"
        " tool call's input longer than the 160-character activity bound"
    )
    output = subprocess.run(
        ["bash", "-c", command], capture_output=True, text=True, check=True
    ).stdout
    reply_lines = 75
    reply = "\n".join(output.splitlines()[:reply_lines])
    padding = "\n".join(
        f"padding line {i:03d}: this block only makes the prompt longer than the bound."
        for i in range(70)
    )
    prompt = (
        "This is a measurement probe. Do exactly two things and nothing else.\n\n"
        "1. Run this one shell command with your shell tool, exactly as written, once:\n\n"
        f"{command}\n\n"
        f"2. Then reply with exactly the first {reply_lines} lines that command printed, "
        "verbatim, one per line, and nothing else: no preamble, no code fence, no "
        "closing remark.\n\n"
        "The block below is padding that makes this prompt longer than the journal's "
        "4096-byte bound. Ignore it.\n\n"
        f"<padding>\n{padding}\n</padding>\n"
        f"END-OF-PROMPT-{nonce}\n"
    )
    assert len(prompt.encode()) > JOURNAL_BOUND, len(prompt.encode())
    assert len(output.encode()) > JOURNAL_BOUND
    assert len(reply.encode()) > JOURNAL_BOUND
    assert len(command) > DETAIL_BOUND
    return prompt, command, output, reply


def write_fixture(root: Path, identities: str) -> dict[str, Path]:
    (root / "project").mkdir(parents=True, exist_ok=True)
    (root / "project" / "README.md").write_text("transcript-source probe project\n")
    (root / "oneharness.toml").write_text(
        f'extends = "{identities}"\n'
        "history = true\n"
        'history_labels = { role = "agent", probe = "transcript-source" }\n'
        'mode = "bypass"\n'
        "timeout = 600\n"
        "harnesses = [\n"
        '    "claude-code:alternate",\n'
        '    "claude-code:alternate2",\n'
        '    "codex:primary",\n'
        '    "codex:alternate",\n'
        '    "claude-code:primary-backup",\n'
        '    "claude-code:primary",\n'
        "]\n"
    )
    (root / "base.yaml").write_text(
        "provider:\n  kind: oneharness\n  bin: oneharness\n"
    )
    (root / "two-party.yaml").write_text(
        "version: 10\n"
        "name: probe-two-party\n"
        "members:\n"
        "  worker:\n"
        "    kind: onejudge\n"
        "    base_config: ./base.yaml\n"
        "    mode: bypass\n"
        "    max_turns: 1\n"
        "    agent:\n"
        "      oneharness_config: ./oneharness.toml\n"
        "    judge:\n"
        f"      command: [python3, {HERE / 'judge.py'}]\n"
    )
    (root / "single.yaml").write_text(
        "version: 10\n"
        "name: probe-single\n"
        "members:\n"
        "  worker:\n"
        "    kind: oneharness\n"
        "    oneharness_config: ./oneharness.toml\n"
    )
    return {
        "two-party": root / "two-party.yaml",
        "single-sided": root / "single.yaml",
        "project": root / "project",
    }


class HistoryWatcher(threading.Thread):
    """Poll the probe's pointer file and every history file it names, stamping
    the wall-clock instant each line is first seen complete on disk."""

    def __init__(self, pointer_file: Path, interval: float = 0.005):
        super().__init__(daemon=True)
        self.pointer_file = pointer_file
        self.interval = interval
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self.pointers: list[tuple[float, dict]] = []
        self.lines: dict[str, list[tuple[float, dict]]] = {}
        self._offsets: dict[str, int] = {}
        self._pointer_offset = 0

    def _drain(self, path: str, offset: int) -> tuple[int, list[dict]]:
        try:
            with open(path, "rb") as handle:
                handle.seek(offset)
                data = handle.read()
        except FileNotFoundError:
            return offset, []
        complete = data.rfind(b"\n") + 1
        out = []
        for raw in data[:complete].splitlines():
            try:
                out.append(json.loads(raw))
            except json.JSONDecodeError:
                pass
        return offset + complete, out

    def poll(self) -> None:
        with self.lock:
            self._poll()

    def _poll(self) -> None:
        now = time.time()
        self._pointer_offset, new = self._drain(str(self.pointer_file), self._pointer_offset)
        for pointer in new:
            self.pointers.append((now, pointer))
            self.lines.setdefault(pointer["history_file"], [])
        for path in list(self.lines):
            offset, new = self._drain(path, self._offsets.get(path, 0))
            self._offsets[path] = offset
            self.lines[path].extend((now, line) for line in new)

    def run(self) -> None:
        while not self.stop.is_set():
            self.poll()
            time.sleep(self.interval)
        self.poll()


def run_live(kind: str, graph: Path, project: Path, scratch: Path, prompt: str) -> dict:
    out = scratch / kind
    out.mkdir(parents=True, exist_ok=True)
    pointer_file = out / "pointers.jsonl"
    task_file = out / "task.txt"
    task_file.write_text(prompt)
    env = dict(os.environ)
    # The probe's own pointer file and labels, never the enclosing dispatch's:
    # an inherited pointer file would put these turns into somebody's run.
    env["ONEHARNESS_HISTORY"] = "1"
    env["ONEHARNESS_HISTORY_POINTER_FILE"] = str(pointer_file)
    env["ONEHARNESS_HISTORY_LABELS"] = f"probe.kind={kind}"
    watcher = HistoryWatcher(pointer_file)
    watcher.start()
    journal: list[tuple[float, dict]] = []
    with open(out / "stderr.log", "w") as stderr:
        proc = subprocess.Popen(
            ["oneagentgraph", "run", str(graph), "--task-file", str(task_file), "--dir", str(project)],
            stdout=subprocess.PIPE,
            stderr=stderr,
            env=env,
            text=True,
        )
        assert proc.stdout is not None
        for line in proc.stdout:
            stamp = time.time()
            # Read the history store at the instant the record arrives, so
            # "was its content already on disk" is answered exactly rather than
            # to the poll interval.
            watcher.poll()
            try:
                journal.append((stamp, json.loads(line)))
            except json.JSONDecodeError:
                pass
        code = proc.wait()
    watcher.stop.set()
    watcher.join()
    with open(out / "events.jsonl", "w") as handle:
        for _, envelope in journal:
            handle.write(json.dumps(envelope) + "\n")
    capture = {
        "kind": kind,
        "exit": code,
        "journal": journal,
        "pointers": watcher.pointers,
        "lines": watcher.lines,
    }
    # Everything the resolution reads, with its arrival stamps, so `replay`
    # re-reads a probe without spending another turn.
    with open(out / "capture.json", "w") as handle:
        json.dump(capture, handle)
    return capture


def show(text: str | None, width: int = 48) -> str:
    if text is None:
        return "-"
    flat = text.replace("\n", "\\n")
    return flat if len(flat) <= width else flat[: width - 3] + "..."


def resolve_live(result: dict, truth: dict) -> list[dict]:
    """Resolve every turn record by the candidate reference and compare it with
    what the probe knows the turn carried.

    The reference is taken two ways. *At emit*: the newest pointer line on disk
    when the record arrived — oneharness writes one per harness run, before it
    spawns the harness, so this is what a producer reading the pointer file could
    have stamped. *Final*: the run oneharness closed `ok`, which is what the
    journal's own `oneharness-session` record names once the call returns.
    """
    journal = result["journal"]
    sessions = [e["payload"] for _, e in journal if e.get("kind") == "oneharness-session"]
    pointers = [p for _, p in result["pointers"]]
    all_lines = [(t, l) for lines in result["lines"].values() for t, l in lines]
    ok_runs = [l["history_id"] for _, l in all_lines if l.get("type") == "run" and l.get("status") == "ok"]
    final_id = sessions[-1]["history_id"] if sessions else (ok_runs[-1] if ok_runs else None)

    def file_of(history_id: str | None) -> str | None:
        return next((p["history_file"] for p in pointers if p["history_id"] == history_id), None)

    def run_line(history_id: str | None):
        hits = [(t, l) for t, l in all_lines if l.get("type") == "run" and l.get("history_id") == history_id]
        return hits[-1] if hits else None

    rows = []
    for t_journal, envelope in journal:
        kind = envelope.get("kind")
        if kind not in TURN_KINDS:
            continue
        payload = envelope["payload"]
        known = [p for t, p in result["pointers"] if t <= t_journal]
        at_emit = known[-1]["history_id"] if known else None
        row = {
            "record": kind if kind != "turn-activity" else f"turn-activity/{payload.get('kind')}",
            "role": payload.get("role"),
            "index": payload.get("index"),
            "tool_call_id": payload.get("tool_call_id"),
            "ref_at_emit": at_emit,
        }
        target = None
        expected = None
        if kind == "turn-started":
            row["journal"] = payload.get("instruction")
            row["journal_truncated"] = payload.get("instruction_truncated")
            ref = at_emit or final_id
            target = run_line(final_id if at_emit is None else at_emit)
            # The run line of the candidate that was live at emit may be a
            # fallen-through one; every candidate is handed the same prompt.
            row["resolved"] = target[1].get("prompt") if target else None
            expected = truth["prompt"]
            row["via"] = "run line .prompt"
        elif kind == "turn-message":
            row["journal"] = payload.get("text")
            row["journal_truncated"] = payload.get("truncated")
            if payload.get("role") == "assistant":
                ref = final_id
                target = run_line(final_id)
                row["resolved"] = target[1].get("text") if target else None
                expected = truth["reply"]
                row["via"] = "run line .text"
            else:
                ref = None
                row["resolved"] = None
                row["via"] = "none: the supervising side's words are not in the agent's history"
        else:
            sub = payload.get("kind")
            index = payload.get("index")
            ref = at_emit
            hits = [
                (t, l) for t, l in all_lines
                if l.get("type") == "event" and l.get("run_id") == ref and l["event"].get("index") == index
            ]
            row["index_only_matches"] = sum(
                1 for _, l in all_lines if l.get("type") == "event" and l["event"].get("index") == index
            )
            target = hits[-1] if hits else None
            event = target[1]["event"] if target else None
            row["via"] = "event line (history_id, index)"
            if event and payload.get("tool_call_id"):
                row["tool_call_id_agrees"] = event.get("tool_call_id") == payload.get("tool_call_id")
            served = ref == final_id
            if not served:
                row["via"] += " [fallen-through candidate]"
            if sub == "tool_call":
                row["journal"] = payload.get("detail")
                row["journal_truncated"] = payload.get("truncated")
                row["resolved"] = (event.get("input") or {}).get("command") if event else None
                expected = truth["command"] if served else None
                row["via"] += " .input.command"
            elif sub == "tool_result":
                row["journal"] = payload.get("output")
                row["journal_truncated"] = payload.get("output_truncated")
                row["resolved"] = event.get("output") if event else None
                expected = truth["output"] if served else None
                row["via"] += " .output"
            else:
                row["journal"] = payload.get("output") or payload.get("detail")
                row["journal_truncated"] = payload.get("output_truncated") or payload.get("truncated")
                row["resolved"] = event.get("output") if event else None
                expected = truth["reply"] if served and sub == "message" else None
                row["via"] += " .output"
        row["ref"] = ref
        row["history_file"] = file_of(ref)
        row["expected"] = expected
        # How long after the record reached a journal reader its content was on
        # disk: <= 0 means it was already there, so the reference resolved live.
        row["history_after_journal_ms"] = round((target[0] - t_journal) * 1000, 1) if target else None
        row["exact"] = row["resolved"] is not None and expected is not None and row["resolved"] == expected
        # A harness trims the trailing newline of a tool result; that is the
        # harness's normalization, made before oneharness ever sees the text.
        row["exact_modulo_trailing_ws"] = (
            row["resolved"] is not None and expected is not None and row["resolved"].rstrip() == expected.rstrip()
        )
        journal_text = (row.get("journal") or "").rstrip("…")
        row["journal_is_prefix"] = row["resolved"] is not None and row["resolved"].startswith(journal_text)
        rows.append(row)
    return rows


def print_live(result: dict, rows: list[dict]) -> None:
    print(f"\n=== live: {result['kind']} member (oneagentgraph exit {result['exit']}) ===")
    for t, pointer in result["pointers"]:
        print(
            f"    pointer line {datetime.fromtimestamp(t, timezone.utc).strftime('%H:%M:%S.%f')[:-3]}"
            f"  {pointer['harness_id']:<24} history_id={pointer['history_id']}"
        )
    files = sorted({p["history_file"] for _, p in result["pointers"]})
    for f in files:
        print(f"    history file {f}")
    for _, envelope in result["journal"]:
        if envelope.get("kind") == "oneharness-session":
            print(
                f"    journal oneharness-session at {envelope['ts']} history_id={envelope['payload']['history_id']}"
                " (published after the call returned)"
            )
    print(
        "    record                     idx  journal B,cut   resolved B  ==full  ==full~ws  ref@emit  hist-after-journal  via"
    )
    for row in rows:
        jb = len((row.get("journal") or "").encode())
        rb = "-" if row["resolved"] is None else len(row["resolved"].encode())
        lag = "-" if row["history_after_journal_ms"] is None else f"{row['history_after_journal_ms']:+.1f}ms"
        exact = "n/a" if row["expected"] is None else ("YES" if row["exact"] else "no")
        exws = "n/a" if row["expected"] is None else ("YES" if row["exact_modulo_trailing_ws"] else "no")
        idx = "-" if row["index"] is None else str(row["index"])
        cut = "cut" if row.get("journal_truncated") else "whole"
        print(
            f"    {row['record']:<26} {idx:>3}  {jb:>6},{cut:<5}  {str(rb):>10}  {exact:<6}  {exws:<9}"
            f"  {'yes' if row['ref_at_emit'] else 'NO':<8}  {lag:>18}  {row['via']}"
        )
        if row.get("index_only_matches", 1) > 1:
            print(f"        note: (file, index) alone matches {row['index_only_matches']} event lines; history_id disambiguates")
        if row.get("tool_call_id_agrees") is False:
            print("        note: tool_call_id DISAGREES with the history event")
        if row["resolved"] is not None and not row["journal_is_prefix"] and row.get("journal_truncated"):
            print("        note: the journal copy is a reshaped summary, not a prefix of the resolved text")
        if row["expected"] is not None and row["resolved"] is not None and not row["exact_modulo_trailing_ws"]:
            print(f"        note: resolved {show(row['resolved'])!r} vs full {show(row['expected'])!r}")
    full = [r for r in rows if r["expected"] is not None]
    print(
        f"    summary: {sum(r['exact_modulo_trailing_ws'] for r in full)}/{len(full)} records with known full text"
        f" resolved to it exactly (byte-exact: {sum(r['exact'] for r in full)});"
        f" {sum(1 for r in rows if r['ref_at_emit'])}/{len(rows)} had a pointer line on disk at emit;"
        f" {sum(1 for r in rows if r['history_after_journal_ms'] is not None and r['history_after_journal_ms'] <= 0)}"
        f"/{len(rows)} had their content on disk when the record arrived"
    )
    print("    ==full~ws ignores trailing whitespace only (the harness trims a tool result's last newline).")
    print("    hist-after-journal: when the resolving history line was first on disk relative to the journal")
    print("    record reaching this reader; <= 0 means already there, > 0 means not resolvable live yet.")


def live(args: argparse.Namespace) -> int:
    base = Path(os.environ.get("ONEPIPELINE_NODE_SCRATCH_DIR") or "/tmp")
    scratch = Path(args.scratch) if args.scratch else base / f"transcript-source-{int(time.time())}-{os.getpid()}"
    scratch.mkdir(parents=True, exist_ok=True)
    print(f"scratch: {scratch}")
    paths = write_fixture(scratch, args.identities)
    status = 0
    for kind in args.kinds:
        nonce = secrets.token_hex(3)
        prompt, command, output, reply = prompt_for(nonce)
        truth = {"prompt": prompt, "command": command, "output": output, "reply": reply}
        print(
            f"\n[{kind}] prompt {len(prompt.encode())} bytes, command {len(command)} chars, "
            f"output {len(output.encode())} bytes, expected reply {len(reply.encode())} bytes"
        )
        (scratch / kind).mkdir(parents=True, exist_ok=True)
        with open(scratch / kind / "truth.json", "w") as handle:
            json.dump(truth, handle)
        run_live(kind, paths[kind], paths["project"], scratch, prompt)
        status |= report(scratch / kind)
    return status


def report(out: Path) -> int:
    """Resolve and print one captured probe; non-zero when a record whose full
    text the probe knows did not resolve to it."""
    with open(out / "capture.json") as handle:
        result = json.load(handle)
    with open(out / "truth.json") as handle:
        truth = json.load(handle)
    rows = resolve_live(result, truth)
    print(f"\n    capture: {out / 'capture.json'}")
    print_live(result, rows)
    with open(out / "resolution.json", "w") as handle:
        json.dump(rows, handle, indent=1)
    return int(any(r["expected"] is not None and not r["exact_modulo_trailing_ws"] for r in rows))


def replay(args: argparse.Namespace) -> int:
    if not args.scratch:
        sys.exit("replay needs --scratch <a previous probe's scratch directory>")
    status = 0
    for kind in args.kinds:
        status |= report(Path(args.scratch) / kind)
    return status


# ----------------------------------------------------------------- recorded


def parse_ts(value: str | None) -> float | None:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def read_jsonl(path: str) -> list[dict]:
    out = []
    try:
        with open(path, errors="replace") as handle:
            for line in handle:
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
    except FileNotFoundError:
        pass
    return out


def members_of(events: list[dict]) -> dict[tuple[str, str], dict]:
    """One entry per (graph stream, member): its graph, engine, worktree, window."""
    graphs: dict[str, str] = {}
    members: dict[tuple[str, str], dict] = {}
    for e in events:
        stream, kind = e.get("stream"), e.get("kind")
        labels, payload = e.get("labels") or {}, e.get("payload") or {}
        if kind == "graph-started":
            graphs[stream] = payload.get("name")
        member = labels.get("member")
        if not member or e.get("source") != "agentgraph":
            continue
        key = (stream, member)
        entry = members.setdefault(
            key,
            {"graph": graphs.get(stream), "node": labels.get("node"), "engine": None,
             "worktree": None, "scratch": None, "first": None, "last": None,
             "sessions": [], "turn_started": [], "activity": 0},
        )
        ts = parse_ts(e.get("ts"))
        entry["first"] = entry["first"] or ts
        entry["last"] = ts or entry["last"]
        if kind == "member-started":
            entry["engine"] = payload.get("engine")
            entry["worktree"] = payload.get("worktree")
            config = payload.get("config")
            entry["scratch"] = str(Path(config).parent) if config else None
        elif kind == "oneharness-session":
            entry["sessions"].append(payload)
        elif kind == "turn-started":
            entry["turn_started"].append(e)
        elif kind == "turn-activity":
            entry["activity"] += 1
    return members


def pointers_for(entry: dict, stream: str, member: str, pointers: list[dict]) -> list[dict]:
    """The pointer lines this member's own harness runs wrote. A pointer file
    also collects every nested run a worker's tests start under the inherited
    environment, so a line counts only when it ran in the member's worktree,
    inside its window, and — for a two-party member — under the session name
    onejudge gives its agent and judge sides."""
    if entry["first"] is None or entry["last"] is None:
        return []
    out = []
    for p in pointers:
        started = parse_ts(p.get("started"))
        if started is None or not (entry["first"] - 2 <= started <= entry["last"] + 2):
            continue
        if entry["worktree"] and p.get("project") != entry["worktree"]:
            continue
        if entry["engine"] == "onejudge":
            if not str(p.get("name", "")).startswith(f"{stream}-{member}"):
                continue
        elif (p.get("labels") or {}).get("role") not in ("agent",):
            continue
        out.append(p)
    return out


def recorded(args: argparse.Namespace) -> int:
    runs = sorted(glob.glob(os.path.join(args.runs, "*", "events.jsonl")))
    print(f"\n=== recorded runs: {len(runs)} journals under {args.runs} (read-only) ===")
    by_kind: dict[tuple, dict] = {}
    drafter_cases = []
    history_cache: dict[str, list[dict]] = {}
    for journal in runs:
        run_dir = os.path.dirname(journal)
        events = read_jsonl(journal)
        pointers = read_jsonl(os.path.join(run_dir, "oneharness-sessions.jsonl"))
        for (stream, member), entry in members_of(events).items():
            if entry["engine"] is None:
                continue
            mine = pointers_for(entry, stream, member, pointers)
            key = (entry["graph"], member, entry["engine"])
            agg = by_kind.setdefault(
                key,
                {"members": 0, "with_pointer_lines": 0, "with_journal_session": 0,
                 "journal_session_live": 0, "history_files": set(), "files_present": 0,
                 "worktree_gone": 0, "worktree_gone_file_present": 0, "scratch_gone": 0,
                 "scratch_gone_file_present": 0, "with_activity": 0, "default_store": 0},
            )
            agg["members"] += 1
            agg["with_activity"] += bool(entry["activity"])
            agg["with_pointer_lines"] += bool(mine)
            agg["with_journal_session"] += bool(entry["sessions"])
            files = {p["history_file"] for p in mine}
            files |= {
                os.path.join(s["history_dir"], s["history_project"], s["history_session"] + ".jsonl")
                for s in entry["sessions"] if s.get("history_dir")
            }
            for f in files:
                if f in agg["history_files"]:
                    continue
                agg["history_files"].add(f)
                present = os.path.exists(f)
                agg["files_present"] += present
                agg["default_store"] += f.startswith(os.path.expanduser("~/.local/state/oneharness/history/"))
            if entry["worktree"] and not os.path.exists(entry["worktree"]) and files:
                agg["worktree_gone"] += 1
                agg["worktree_gone_file_present"] += all(os.path.exists(f) for f in files)
            if entry["scratch"] and not os.path.exists(entry["scratch"]) and files:
                agg["scratch_gone"] += 1
                agg["scratch_gone_file_present"] += all(os.path.exists(f) for f in files)
            if member == "pr-author":
                for started in entry["turn_started"]:
                    if started["payload"].get("instruction_truncated"):
                        drafter_cases.append((journal, stream, started, mine))
    print("\nQ1/Q3 per member kind: history written? journal pointer? file outlived worktree/scratch?")
    print(
        "    graph/member (engine)                 members  activity  ptr-lines  journal-ohs"
        "  files  present  default-store  wt-gone(file-kept)  scratch-gone(file-kept)"
    )
    for (graph, member, engine), agg in sorted(by_kind.items(), key=lambda kv: -kv[1]["members"]):
        print(
            f"    {f'{graph}/{member} ({engine})':<38} {agg['members']:>7}  {agg['with_activity']:>8}"
            f"  {agg['with_pointer_lines']:>9}  {agg['with_journal_session']:>11}  {len(agg['history_files']):>5}"
            f"  {agg['files_present']:>7}  {agg['default_store']:>13}"
            f"  {agg['worktree_gone']:>7}({agg['worktree_gone_file_present']})"
            f"  {agg['scratch_gone']:>14}({agg['scratch_gone_file_present']})"
        )
    print("    ptr-lines: members with a line in the run's oneharness-sessions.jsonl (ONEHARNESS_HISTORY_POINTER_FILE)")
    print("    journal-ohs: members whose journal carries an `oneharness-session` record")

    print(f"\nQ6 change-request drafter turn-started with instruction_truncated: true: {len(drafter_cases)}")
    resolved = whole = prefix = 0
    sizes = []
    examples = []
    for journal, stream, started, mine in drafter_cases:
        instruction = started["payload"].get("instruction") or ""
        prompts = []
        for p in mine:
            lines = history_cache.setdefault(p["history_file"], read_jsonl(p["history_file"]))
            prompts += [l for l in lines if l.get("type") == "run" and l.get("history_id") == p["history_id"]]
        if not prompts:
            continue
        resolved += 1
        run = next((r for r in prompts if r.get("status") == "ok"), prompts[-1])
        prompt = run.get("prompt") or ""
        is_prefix = prompt.startswith(instruction)
        prefix += is_prefix
        whole += is_prefix and len(prompt.encode()) > len(instruction.encode())
        sizes.append((len(instruction.encode()), len(prompt.encode())))
        if len(examples) < 3:
            examples.append((journal, stream, run, instruction, prompt))
    print(f"    resolved to a history run line: {resolved}")
    print(f"    journal instruction is a prefix of the history prompt: {prefix}")
    print(f"    history prompt strictly longer than the journal copy: {whole}")
    if sizes:
        longest = max(b for _, b in sizes)
        print(
            f"    journal copy bytes: max {max(a for a, _ in sizes)}; history prompt bytes: "
            f"min {min(b for _, b in sizes)}, max {longest}"
        )
    for journal, stream, run, instruction, prompt in examples:
        print(
            f"    e.g. {stream} ({os.path.basename(os.path.dirname(journal))[:60]})\n"
            f"         history_id={run.get('history_id')} harness={run.get('harness_id')} status={run.get('status')}\n"
            f"         journal {len(instruction.encode())} bytes, ends {show(instruction[-40:], 44)!r}\n"
            f"         history {len(prompt.encode())} bytes, ends {show(prompt[-60:], 64)!r}"
        )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("part", choices=["live", "recorded", "replay", "all"], nargs="?", default="all")
    parser.add_argument("--kinds", nargs="+", default=["two-party", "single-sided"],
                        choices=["two-party", "single-sided"])
    parser.add_argument("--scratch")
    parser.add_argument("--runs", default=os.path.expanduser("~/projects/ai-orchestrator/runs"))
    parser.add_argument("--identities",
                        default=os.path.expanduser("~/projects/ai-orchestrator/oneharness.identities.toml"))
    args = parser.parse_args()
    status = 0
    if args.part in ("live", "all"):
        status |= live(args)
    if args.part == "replay":
        status |= replay(args)
    if args.part in ("recorded", "all"):
        status |= recorded(args)
    return status


if __name__ == "__main__":
    sys.exit(main())
