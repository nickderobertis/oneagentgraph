# Event filter notes

<!-- llmlint: ignore-file[contracts_have_one_source_or_a_drift_gate] This document
is a claim-to-symbol implementation map plus a cross-repo alignment record, not a
second normative contract; docs/contract.md remains authoritative and
tests/contract.rs drives its fenced examples through the public types. -->

The filter grammar in `docs/contract.md` is **shared** with `onevcs` and
`onepipeline`, and — like the envelope beside it — is implemented once per
repository with no shared crate. This note maps that grammar to the code here,
and records the corners each of the three implementations has to resolve the
same way. The contract itself remains `docs/contract.md`.

## Where each claim lives

| the contract says | the code |
| --- | --- |
| the grammar, and what one matcher means | `event::EventFilter`, `event::Matcher` |
| `include` admits, `exclude` rejects and wins | `event::EventFilter::allows` |
| a kind glob over the wire string | `event::glob`, `event::EventKind::as_str` |
| a spec that could match nothing is refused | `event::EventFilter::validate` |
| the graph's own say, gated on a schema version | `config::Events`, `config::FIRST_EVENT_FILTER_VERSION` |
| the flag, and its precedence over the graph | `cli::RunArgs::event_filter`, `run::Request::filter` |
| filtering decides what is emitted, not what the run acts on | `event::Emitter::emit_with` |

## Departures from the approved grammar

**None.** Everything below conforms to the approved text; each is a corner that
text does not settle, resolved here and written down so the other two producers
can resolve it identically. A departure, if one is ever needed, is a proposal to
the planner who owns `docs/contract.md` — never a unilateral edit to it.

## Corners the grammar leaves open, and how this producer resolves them

**The glob dialect is `*` and nothing else.** `*` stands for any run of
characters including none; every other character is itself, so `?` and `[a-z]`
are literals. The grammar says "glob" and shows only `member-*`. A dialect
supported by one producer and not another is one spec that filters differently
depending on who read it, so this is stated in `docs/contract.md` rather than
left to each implementation's convenience. Kebab-case wire strings need nothing
wider.

**`seq` numbers what the stream carries, not what was produced.** A suppressed
envelope takes no number with it, so a filtered stream is `0..n` with no gaps.
The envelope contract has a consumer detect loss through per-stream `seq` gaps;
numbering filtered-out events would make every deliberate omission read as a
dropped one, which defeats the point of filtering for a consumer whose loss
detection then fires on its own request. Wire-visible, so the three producers
must agree.

**A run's own `events.jsonl` is the merged stream, so the filter reaches it.**
The file and the caller's sink are one stream teed two ways (`run::Tee`), and
`tests/e2e/library.rs` holds them byte-identical. A producer that kept a separate
durable log could reasonably resolve this the other way; this one cannot, because
the two are the same stream by construction. What is *never* filtered is what the
run reads internally — liveness, scheduling, and settle detection consume the
value `Emitter::emit` returns, which it returns whether or not the envelope was
admitted.

**A label matcher reads the key as the envelope carries it.** `Labels` flattens
its free-form extras beside the reserved fields, and `run::parse_label` reserves
only `run_id`, `member`, and `persona` — so `--label node=service` is accepted,
lands among the extras, and reaches the wire under the reserved name `node`. A
matcher naming `node` therefore consults the typed slot *and* the extras
(`event::stamped`); consulting only the typed slot would refuse to see a label the
same consumer can plainly read. Reconciling `parse_label`'s three reserved keys
with the contract's six is a separate question, and belongs to whoever owns the
label surface.

## What is deliberately not here

`stream` and payload fields are outside the grammar by decision, not by omission:
`stream` identifies a producing process rather than anything a consumer means, and
a turn's `role` lives in a payload. `round` is a reserved label that the approved
matcher list does not name, and is not matchable here either — the list is
implemented exactly as approved.
