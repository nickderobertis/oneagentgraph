//! `oneagentgraph` composes agents into a graph, constructs onejudge/oneharness
//! invocations for each member, and merges their outputs into one NDJSON event
//! stream.
//!
//! # Interface-only
//!
//! This crate is at the **interface-only** stage of its build-out. Every item
//! below is the surface named by [`docs/contract.md`](../../../docs/contract.md)
//! — the approved contract, committed verbatim — and **none of it is
//! implemented**. There are no method bodies beyond derives, trivial field
//! constructors, and serde defaults, and the binary's subcommands parse per the
//! contract and then refuse with a `NOT IMPLEMENTED` error.
//!
//! Consequently these types are useful for one thing today: reading and writing
//! the contract's wire shapes. `tests/contract.rs` drives the fenced blocks out
//! of `docs/contract.md` through them, so the document and this surface cannot
//! drift.
//!
//! This crate owns **no** harness/model/fallback logic. A graph names an
//! oneharness config per role and side; oneharness owns identity chains,
//! fallback, model pins, and quota classification, and onejudge owns the
//! two-party conversation.

#![warn(missing_docs)]

pub mod cli;
pub mod config;
pub mod error;
pub mod event;
pub mod liveness;
