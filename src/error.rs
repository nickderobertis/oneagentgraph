//! The failure modes the contract names, and the exit codes they map to.

// llmlint: ignore-file[invalid_states_unrepresentable] the failing member is named by a
// `String` because a `MemberName` newtype is a public item `docs/contract.md` does not
// name, and minting one is the interface drift the interface-only stage forbids (see
// AGENTS.md). Revisit when the runtime owns a member registry to validate against.

/// What can go wrong running a graph.
///
/// The variants are exactly the two failures `docs/contract.md` distinguishes,
/// and they carry the process exit codes it assigns them:
/// [`EXIT_MEMBER_FAILED`] and [`EXIT_INVALID_CONFIG`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The graph config could not be read, parsed, or validated. Exits
    /// [`EXIT_INVALID_CONFIG`].
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    /// A member failed or died. The event stream says which and why; this is the
    /// summary the process exits on. Exits [`EXIT_MEMBER_FAILED`].
    #[error("member '{member}' failed: {reason}")]
    MemberFailed {
        /// The member that failed.
        member: String,
        /// Why, in one line.
        reason: String,
    },
}

/// Every member settled successfully.
pub const EXIT_SUCCESS: i32 = 0;

/// A member failed or died.
pub const EXIT_MEMBER_FAILED: i32 = 1;

/// The graph config is invalid.
pub const EXIT_INVALID_CONFIG: i32 = 2;
