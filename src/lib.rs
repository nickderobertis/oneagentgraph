//! `oneagentgraph` composes agents into a graph, constructs onejudge/oneharness
//! invocations for each member, and merges their outputs into one NDJSON event
//! stream.
//!
//! Every public item below is the surface named by
//! [`docs/contract.md`](../../../docs/contract.md) — the approved contract,
//! committed verbatim. `tests/contract.rs` drives the fenced blocks out of that
//! document through these types, so the document and this surface cannot drift.
//!
//! This crate owns **no** harness/model/fallback logic. A graph names an
//! oneharness config per role and side; oneharness owns identity chains,
//! fallback, model pins, and quota classification, and onejudge owns the
//! two-party conversation. What lives here is the composition: resolving a
//! graph, constructing each member's invocation, supervising it, and merging
//! every member's output into one NDJSON event stream.

#![warn(missing_docs)]

mod clock;

pub mod cli;
pub mod config;
pub mod error;
pub mod event;
pub mod invoke;
pub mod liveness;
pub mod persona;
pub mod resolve;
pub mod scratch;
