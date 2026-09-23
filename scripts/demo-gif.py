#!/usr/bin/env python3
"""Render the animated hero GIF: one run's event stream filling line by line.

Like `scripts/screenshots.sh`, this drives the **real release `oneagentgraph`
binary** against this crate's own paid-harness double at oneharness's own
`ONEHARNESS_BIN_<ID>` seam — so every line it draws is a line the binary really
wrote, and there is no model call, no paid turn, no network, and no credential.
Unlike the stills it drives the graph's **two-party** member, and that member is
the only thing in this repository that still spawns `oneharness run`: its worker
side is an onejudge `OneharnessProvider`, and `src/judge.rs` installs a `SpawnHook`
on every two-party member so both sides land in the process group liveness and
`cancel` reap through — which is onejudge's documented opt-in to its spawning seam
(`Execution::Process`), since an in-process turn has no process to offer a hook.
So this renderer, and nothing else in the capture, needs the `oneharness` CLI on
`PATH` at the release the justfile pins. The hash-gated stills need none.

`run --output text` is append-only — nothing redraws, nothing is cleared — so
rendering it faithfully is simply replaying the lines in the order they arrived,
at the pace they arrived. There is no view to reconstruct, which is what makes
this simpler than `llmlint`'s equivalent (its live view redraws in place). Each
frame's duration comes from the gap between the two event timestamps it sits
between, clamped, so the animation carries the real rhythm of a run — a tool call
landing, then a wait, then the reply — rather than a uniform tick.

The agent's first turn is deliberately held in flight for [`HOLD_SECONDS`], with
the double's own `fake:hold` sentinel, and released by this process. Without it the
double answers in microseconds and the whole run settles inside one millisecond:
true, and a picture of nothing, because the liveness supervision a reader most
needs to see — the member heartbeat that says a turn is still alive — only has
something to say while a turn is actually running. A held turn is not a staged one;
it is the ordinary case, since a real turn takes minutes.

The output is monochrome because the output **is** monochrome: this tool emits no
ANSI escape anywhere, and colouring the frames here would be inventing a feature
rather than documenting one.

The GIF is informational, like the stills — but it is NOT hash-gated (a GIF is not
byte-reproducible across Pillow versions), so it is regenerated on demand with
`just screenshots-gif` and committed to `docs/screenshots/demo.gif`. Regenerate it
when the text rendering (`src/render.rs`) or the set of events a run publishes
changes.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import threading
from datetime import datetime
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# The window, matching the stills' `freeze` chrome (background #0d1117).
BG = (13, 17, 23)
BAR = (22, 27, 34)
FG = (201, 209, 217)
DOTS = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]  # traffic-light window dots

COLS = 118           # clears the widest line a settling run writes (a
                     # `turn-completed` carrying its usage, or a session id)
FONT_SIZE = 18
PAD = 24
BAR_H = 40
LEAD_MS = 700        # the empty window, before the first line lands
MIN_MS = 160         # floor on a gap, so back-to-back events stay readable
MAX_MS = 1100        # ceiling on a gap, so a real wait does not stall the loop
HOLD_MS = 3000       # hold on the settled stream before looping

# How long the agent's first turn is held in flight. Past the 15-second heartbeat
# bound the contract gives a member, so exactly one `member-heartbeat` lands inside
# the turn — enough to show that liveness is supervised, without a wall of them.
HOLD_SECONDS = 18

# The sentinel steering the double, and the task the run is given. `fake:hold=`
# runs to the end of the string on purpose: its value is a path, and anything after
# it — even the `:` that separates the other sentinels — is read as part of that
# path, so the turn would wait on a file nobody ever writes.
TASK = "fake:complete-now: add the retry fake:hold={release}"


def pin(name: str) -> str:
    """One value from `screenshots/tools.env`, the file that declares them."""
    root = Path(__file__).resolve().parent.parent
    for line in (root / "screenshots/tools.env").read_text().splitlines():
        if line.startswith(f"{name}="):
            return line.split("=", 1)[1].strip()
    raise SystemExit(f"demo-gif: screenshots/tools.env declares no {name}")


def oneharness_version() -> str:
    """The `oneharness` release the justfile pins, read from it rather than copied."""
    root = Path(__file__).resolve().parent.parent
    text = (root / "justfile").read_text()
    marker = '\noneharness-version := "'
    return text.split(marker, 1)[1].split('"', 1)[0]


def oneharness_bin() -> str:
    """The `oneharness` CLI onejudge spawns per side, resolved as the e2e suite does.

    `PATH` first, then the directory `just bootstrap` installs into, which `PATH`
    need not reach first.
    """
    named = os.environ.get("ONEAGENTGRAPH_TEST_ONEHARNESS")
    if named:
        return named
    found = shutil.which("oneharness")
    if found:
        return found
    cargo_home = os.environ.get("CARGO_HOME") or str(Path.home() / ".cargo")
    return str(Path(cargo_home) / "bin/oneharness")


def stream(root: Path, binary: str, fake: str) -> list[str]:
    """Drive one real run of the fixture graph and return its rendered lines.

    The world the run gets is built here and named in full: `env -i` in spirit, so
    an ambient `ONEHARNESS_MODEL` cannot put this run on an identity nobody chose
    and an ambient `ONEHARNESS_HISTORY_POINTER_FILE` cannot write its throwaway
    sessions into the caller's own history store.
    """
    work = Path(tempfile.mkdtemp(prefix="oneagentgraph-gif-"))
    release = work / "release"
    fixture = work / "fixture"
    shutil.copytree(root / "screenshots/fixture", fixture)
    binaries = work / "bin"
    binaries.mkdir()
    shutil.copy(fake, binaries / "oneagentgraph-fake-harness")
    for name in ("home", "tmp", "dir"):
        (work / name).mkdir()

    env = {
        "HOME": str(work / "home"),
        "TMPDIR": str(work / "tmp"),
        "PATH": f"{binaries}:/usr/bin:/bin",
        "ONEAGENTGRAPH_ONEHARNESS_BIN": oneharness_bin(),
    }
    running = subprocess.Popen(
        [binary, "run", "./graph.yaml", "--task", TASK.format(release=release),
         "--dir", str(work / "dir"), "--output", "text"],
        cwd=fixture, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True,
    )
    # Release the held turn from here rather than from the double, which cannot
    # know how long a demonstration wants to watch.
    freeing = threading.Timer(HOLD_SECONDS, release.touch)
    freeing.start()
    try:
        out, err = running.communicate(timeout=HOLD_SECONDS + 120)
    finally:
        freeing.cancel()
    lines = [line for line in out.splitlines() if line.strip()]
    if not lines:
        print("demo-gif: the run published nothing. This hero drives the graph's "
              "two-party member, whose sides onejudge spawns `oneharness run` for, "
              f"so it needs `oneharness {oneharness_version()}` (the justfile's pin) "
              "on PATH or in the cargo bin directory — `just bootstrap` installs it.",
              file=sys.stderr)
        print(err, file=sys.stderr)
        raise SystemExit(1)
    shutil.rmtree(work, ignore_errors=True)
    return lines


def gaps(lines: list[str]) -> list[int]:
    """How long each line stays alone on screen, from the stream's own timestamps.

    Every rendered line opens with its RFC 3339 stamp, so the pace is read off the
    run rather than invented. Clamped at both ends: a burst of events in the same
    millisecond would flash past, and a genuine thirty-second wait would stall the
    loop.
    """
    stamps = []
    for line in lines:
        head = line.split(" ", 1)[0].replace("Z", "+00:00")
        stamps.append(datetime.fromisoformat(head))
    out = []
    for i in range(len(lines)):
        if i + 1 == len(lines):
            out.append(HOLD_MS)
            continue
        delta = (stamps[i + 1] - stamps[i]).total_seconds() * 1000
        out.append(int(min(MAX_MS, max(MIN_MS, delta))))
    return out


def wrap(line: str) -> list[str]:
    """Fold one over-wide line at the window's width, with a hanging indent."""
    if len(line) <= COLS:
        return [line]
    folded = [line[:COLS]]
    rest = line[COLS:]
    while rest:
        folded.append("  " + rest[: COLS - 2])
        rest = rest[COLS - 2 :]
    return folded


def render(lines: list[str], durations: list[int], font_path: str, out: str) -> int:
    font = ImageFont.truetype(font_path, FONT_SIZE)
    char = font.getlength("M")
    ascent, descent = font.getmetrics()
    line_h = ascent + descent + 4

    frames = [([], LEAD_MS)]
    drawn: list[str] = []
    for line, ms in zip(lines, durations):
        drawn = drawn + wrap(line)
        frames.append((list(drawn), ms))

    rows = max(len(body) for body, _ in frames)
    width = int(PAD * 2 + COLS * char)
    height = int(BAR_H + PAD + rows * line_h + PAD)

    def draw(body: list[str]) -> Image.Image:
        image = Image.new("RGB", (width, height), BG)
        pen = ImageDraw.Draw(image)
        # Window chrome: a title bar with three traffic-light dots.
        pen.rectangle([0, 0, width, BAR_H], fill=BAR)
        for i, colour in enumerate(DOTS):
            cx, cy = PAD + i * 22, BAR_H // 2
            pen.ellipse([cx - 6, cy - 6, cx + 6, cy + 6], fill=colour)
        y = BAR_H + PAD
        for text in body:
            pen.text((PAD, y), text, font=font, fill=FG)
            y += line_h
        return image

    images = [draw(body) for body, _ in frames]
    images[0].save(
        out, save_all=True, append_images=images[1:],
        duration=[ms for _, ms in frames], loop=0, optimize=True, disposal=2,
    )
    return len(images)


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    binary = root / "target/release/oneagentgraph"
    fake = root / "target/release/oneagentgraph-fake-harness"
    font = root / "screenshots" / pin("FONT_FILE")
    out = os.environ.get("DEMO_GIF_OUT", str(root / "docs/screenshots/demo.gif"))

    for path in (binary, fake, font):
        if not path.exists():
            print(f"demo-gif: missing {path}", file=sys.stderr)
            return 1

    lines = stream(root, str(binary), str(fake))
    frames = render(lines, gaps(lines), str(font), out)
    Path(out).parent.mkdir(parents=True, exist_ok=True)
    print(f"demo-gif: wrote {out} ({frames} frames from {len(lines)} events)",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
