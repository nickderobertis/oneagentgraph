//! The `oneagentgraph` binary.
//!
//! At the interface-only stage this parses the full command surface from
//! `docs/contract.md` and then refuses: no graph runs, and no subcommand does
//! its work. The refusal is loud and carries its own exit code so a caller that
//! wired this in early fails visibly rather than reading an empty event stream
//! as a graph that settled.

use clap::Parser;
use oneagentgraph::cli::{Cli, Command};

/// The interface-only refusal, kept distinct from every code the contract
/// assigns: `0` success, `1` a member failed or died, `2` invalid config. It
/// goes away with the implementation.
const EXIT_NOT_IMPLEMENTED: i32 = 3;

fn main() {
    let cli = Cli::parse();
    let command = name_of(&cli.command);
    eprintln!(
        "oneagentgraph: NOT IMPLEMENTED — `{command}` parses per docs/contract.md, \
         but this build implements none of it."
    );
    eprintln!(
        "ACTION: use a release that implements the contract; \
         `oneagentgraph --help` shows the surface this one agrees to."
    );
    std::process::exit(EXIT_NOT_IMPLEMENTED);
}

/// The subcommand's name as a user typed it, for the refusal above.
fn name_of(command: &Command) -> &'static str {
    match command {
        Command::Run(_) => "run",
        Command::Validate(_) => "validate",
        Command::Trigger(_) => "trigger",
        Command::ResetTimer(_) => "reset-timer",
        Command::Cancel(_) => "cancel",
        Command::History(_) => "history",
        Command::Health => "health",
        Command::Smoke(_) => "smoke",
        Command::Persona(_) => "persona",
    }
}
