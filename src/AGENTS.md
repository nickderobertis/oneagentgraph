# The crate

Every public item here exists because `docs/contract.md` names it, and the crate
is at the **interface-only** stage: the surface compiles, and nothing behind it
does anything.

Rules while that holds:

- **No method bodies** beyond derives, trivial field constructors, and serde
  `default` helpers. A version check, an RFC 3339 parse, a truncation, a
  `resolve()` — all of it belongs to the implementation change, not here.
- **Add no public item the contract does not name.** A helpful-looking
  convenience method, a `Result` alias, a builder, an extra enum variant: each is
  interface drift, and a consumer that pins to it gets a breaking change when the
  implementation lands.
- **`main` parses and refuses.** Exit code 3 is scaffolding distinct from the
  contract's own `0` / `1` / `2`; it goes away with the implementation.
- **Optionality is a decision, not a default.** Where the contract states a
  default (`stream: true`) or shows a field as `null` / `[]`, that reading is
  encoded. Where it neither states a default nor marks a field optional, the
  field is *required* — do not quietly relax one to make a config parse.
- **`#![warn(missing_docs)]` with `clippy -D warnings` means undocumented public
  items fail the gate.** That is deliberate: at this stage the docs are most of
  what the crate delivers.

The llmlint directives in these files name which rule above forbids their fix.
They are exemptions for this stage, not permanent ones: revisit each with the
implementation rather than widening its directive.

`tests/contract.rs` reads `docs/contract.md` itself, so a type added here without
a matching assertion there leaves the document unproven. Extend it in the same
change.
