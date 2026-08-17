# The persona format

A **persona** is the delta a member layers over the onejudge base config it names
as its `base_config`: what makes this role different, with the shared wiring left
in the base. A graph reaches one by built-in name (`persona: engineer`), by a
name in its own catalog (`personas: ./personas`), or by path or URL
(`persona: ./roles/lead.yaml`).

A persona is a **onejudge config fragment**. It is written in onejudge's own
field names and validated against onejudge's own config schema, so oneagentgraph
keeps no second definition of that schema to drift from it — including no
required fields of its own. onejudge requires nothing but a task, and a task
never comes from a persona, so **a persona carrying only a `system_prompt` is
complete.** That matters for a member whose judge is an external `command`
provider: there is no simulated user at all there, and a persona for one carries
no `user:` block.

`tests/persona_format.rs` drives every example below through the real types, so
this document cannot drift from what ships.

## What a persona may say

```yaml
name: lead                 # ours: the label stamped on this member's events
system_prompt: |           # onejudge: the role, appended after the base's preamble
  You lead the implementation. Say what you changed and why.
user:                      # onejudge: the simulated supervisor; omit it entirely
  persona: |               #   for a member that has none
    You are a tech lead. Push on correctness and on tests that prove behaviour.
  done_when: "the change is proven end to end"
  done_when_replaces_base: false   # ours: see "How it merges" below
  max_turns: 8
evals:                     # onejudge: extra checks over the finished transcript
  - criterion: "the change is well-scoped"
    kind: numeric
    scale: [1, 5]
assessment: "Name the follow-up work this run left out of scope."
```

Every key is onejudge's except two, which are oneagentgraph's own and are
consumed by the merge — neither ever reaches onejudge:

| key | what it does |
| --- | --- |
| `name` | the `persona` label on every event this member publishes. Absent, the label is the persona ref's own file name. |
| `user.done_when_replaces_base` | `true` when this role's bar must stand in for the base's instead of adding to it. Needs a `user.done_when` to stand in with, or the file is refused. |

Four onejudge fields are **refused** in a persona, because the member's own launch
decides each and would overwrite it: `provider` (the graph's `agent:` / `judge:`),
`session` (the run's), `task` (the member's `task:` or the run's `--task`), and
`skill` (resolved against the directory of the config naming it, so it belongs in
the base config). Anything else onejudge accepts, a persona may carry.

## How it merges

The persona is layered over the base config, with three fields composed rather
than replaced:

- **`system_prompt`** — the base's shared preamble, then the persona's role,
  separated by a blank line. Both are kept.
- **`user.done_when`** — the base's bar *and* the persona's, numbered under
  `Both of these must hold:`. A base's bar is where an operator centralizes the
  review bar for every dispatch, so a role bringing its own is asking for a
  second bar, not for the shared one to go away. `done_when_replaces_base: true`
  is how a role says it must genuinely stand in for the shared one.
- **`task`** — dropped from the base. It reaches onejudge over the CLI, so a base
  that happens to carry one cannot leak into every member.

Everything else the persona brings — `user`'s other keys, `evals`, `assessment` —
replaces the base's. A base with no `user:` block merged with a persona that
brings none keeps having none: no empty `user:` is invented, because to onejudge
an empty one is a supervisor with an empty persona rather than the single-turn
run the base asked for.

## The previous spelling is refused

Before this format, oneagentgraph defined a persona schema of its own whose role
went in a top-level `agent:` block, and translated it to `system_prompt` at the
merge. **That spelling no longer loads, anywhere** — not through
`oneagentgraph persona validate`, and not when a member resolves a persona as a
graph runs. There is no alias, no flag, no environment variable, and no
deprecation period; a file written in it is refused with the field to write
instead, and produces no member.

```yaml
# Refused. `agent` is not a persona key.
agent:
  name: lead
  instructions: |
    You lead the implementation.
user:
  persona: |
    You are a tech lead.
```

The rewrite is key-by-key, and nothing else moves:

| previous | now |
| --- | --- |
| `agent.instructions` | `system_prompt` (top level) |
| `agent.name` | `name` (top level) |
| `user.persona` | unchanged |
| `user.done_when` | unchanged |
| `user.done_when_replaces_base` | unchanged |
| `user.max_turns` | unchanged |
| `evals` | unchanged |

A **base config** is a onejudge config, so the same rule holds there: a base
carrying an `agent:` block is refused, and its shared preamble is written as the
top-level `system_prompt`.
