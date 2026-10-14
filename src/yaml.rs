//! YAML, read and written one way.
//!
//! Every YAML document this crate reads — a graph, a persona, a base config, an
//! event filter file, a `--set` value — and the effective onejudge config it
//! writes goes through here, so one set of reader and writer settings holds for
//! all of them, and the crate's own tests read and write under the same ones.
//!
//! Not part of the library's interface: it is public only so the binary and the
//! integration tests reach the same settings, and its error carries no type of
//! the YAML crate underneath.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options, SerializerOptions};

/// A document that could not be read, or a value that could not be written: the
/// YAML crate's own message, which names the line and column it failed at.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

/// Read one YAML document as `T`.
///
/// # Errors
///
/// [`Error`] when the text is not YAML, or is YAML that is not a `T`.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_saphyr::from_str_with_options(text, reader()).map_err(|err| Error(err.to_string()))
}

/// Write `value` as one YAML document.
///
/// # Errors
///
/// [`Error`] when `value` has no YAML rendering.
pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_saphyr::to_string_with_options(&value, writer()).map_err(|err| Error(err.to_string()))
}

fn reader() -> Options {
    let mut options = Options::default();
    // Comments are not data. Kept, they count against a limit the host's real
    // base config exceeds, with a long comment between `user:` and its first
    // entry.
    options.emit_comments = false;
    // `true` and `false` are booleans; `yes`, `on` and the rest are strings.
    options.strict_booleans = true;
    // `<<` is a key like any other, and a repeated key keeps its last value:
    // the files this crate reads were written against that reading.
    options.merge_keys = MergeKeyPolicy::AsOrdinary;
    options.duplicate_keys = DuplicateKeyPolicy::LastWins;
    // A refusal is one line naming what failed and where, prefixed by the
    // caller with the file or option it came from — not a rendered excerpt.
    options.with_snippet = false;
    // A tag this crate cannot honour is refused rather than dropped.
    options.reject_unsupported_tags = true;
    options
}

fn writer() -> SerializerOptions {
    let mut options = SerializerOptions::default();
    // A long one-line string stays one line rather than folding into a `>-`
    // block, so what a reader greps for in a written config is on one line.
    options.folded_wrap_chars = usize::MAX;
    options.min_fold_chars = usize::MAX;
    options
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    /// An operator's base config explains a mapping in a long comment between
    /// its key and its first entry; the reader takes it, which it would not
    /// with comments kept.
    #[test]
    fn a_long_run_of_comments_before_a_key_still_reads() {
        let commentary = "  # a line of commentary\n".repeat(200);
        let document = format!("user:\n{commentary}  persona: x\n");
        let read: Value = super::from_str(&document).expect("the document reads");
        assert_eq!(read["user"]["persona"], "x");
    }

    #[test]
    fn a_refusal_names_the_line_and_column() {
        let err = super::from_str::<Value>("a: [\n").expect_err("an unterminated sequence");
        assert!(err.to_string().contains("line"), "{err}");
    }

    #[test]
    fn a_long_string_is_written_on_one_line() {
        let long = "word ".repeat(60);
        let written = super::to_string(&serde_json::json!({ "s": long.trim() })).expect("writes");
        assert_eq!(written.lines().count(), 1, "{written}");
    }
}
