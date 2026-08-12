//! `oneagentgraph` composes agents into a graph, drives onejudge in-process for
//! each two-party member and `oneharness run` for each single-sided one, and
//! merges their outputs into one NDJSON event stream.
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
//! graph, preparing each member's launch, supervising it, and merging every
//! member's output into one NDJSON event stream.

#![warn(missing_docs)]

mod clock;
mod harness_process;

pub mod cli;
pub mod config;
pub mod control;
pub mod error;
pub mod event;
pub mod health;
pub mod history;
pub mod invoke;
pub mod judge;
pub mod liveness;
pub mod member;
pub mod persona;
pub mod render;
pub mod resolve;
pub mod run;
pub mod scratch;
pub mod smoke;
pub mod sweep;
