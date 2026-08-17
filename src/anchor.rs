//! The one place a written relative path is anchored to the directory the config
//! naming it was written in.
//!
//! A module rather than a helper beside one caller, because the rule is not one
//! call path's: a graph names configs, a config names a skill, a graph names a
//! persona catalog, and a `join` gets every one of them wrong on Windows for the
//! reason [`anchored`] gives. Anchor through this from any new site.

use std::path::{Component, Path, PathBuf};

/// One relative path, anchored to the directory the config naming it was written
/// in — the same string on every platform.
///
/// `None` when there is nothing to anchor: an empty value, or a path that names
/// its own root and so already says where it starts from.
///
/// The splice is textual because `Path::join` and [`std::path::absolute`] answer
/// for the host: on Windows they spell the separator differently and re-root a
/// path that carries no drive — `Path::is_relative` calls `/graphs/api`
/// *relative* there — under a drive its author never wrote, naming a file nobody
/// asked for. Only a base with no root of its own is made absolute, against the
/// directory this process runs in, because that is the one it was written
/// against.
///
/// Purely lexical, as every caller's documentation promises: nothing is read.
pub(crate) fn anchored(base_dir: &Path, written: &str) -> Option<String> {
    let path = Path::new(written);
    if written.is_empty() || names_its_own_root(path) {
        return None;
    }
    let base = if names_its_own_root(base_dir) {
        base_dir.to_path_buf()
    } else {
        // An empty base is a config named by a bare filename — `Path::parent`
        // answers `""` for one — so the directory it was written in is the
        // directory this process runs in. Named as `.` because that is the same
        // directory and [`std::path::absolute`] refuses an empty path.
        let base = if base_dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            base_dir
        };
        std::path::absolute(base).unwrap_or_else(|_| base.to_path_buf())
    };
    let base = base.display().to_string();
    let separator = separator_of(&base).to_string();
    // A `.` says "the directory this config is in", which is what the base
    // already names; every other component is carried exactly as written.
    let relative: Vec<String> = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let relative = relative.join(&separator);
    Some(if base.ends_with(['/', '\\']) {
        format!("{base}{relative}")
    } else {
        format!("{base}{separator}{relative}")
    })
}

/// The same rule for a caller holding paths rather than config strings: the
/// anchored path, or `written` exactly as it stands when there is nothing to
/// anchor — no base to anchor against, an empty value, or a path already naming
/// its own root.
///
/// Spelled through [`anchored`] rather than beside it, so a path this crate
/// resolves and a path it stamps into a config are anchored by one rule. The
/// round trip through a string is lossless for every path this is asked about:
/// each arrives from a YAML or TOML document, which is UTF-8 by the time it
/// parses.
pub(crate) fn anchored_path(base_dir: Option<&Path>, written: &Path) -> PathBuf {
    let Some(base_dir) = base_dir else {
        return written.to_path_buf();
    };
    anchored(base_dir, &written.to_string_lossy())
        .map_or_else(|| written.to_path_buf(), PathBuf::from)
}

/// Whether a path carries a root of its own, and so says where it starts from
/// rather than leaving that to whatever directory it is read in.
///
/// Not `Path::is_absolute`, which answers a narrower question on Windows — a
/// rooted path with no drive is not absolute there. Every Windows form an
/// operator writes carries a root: `C:\…`, `\\server\share\…`, a verbatim
/// `\\?\…`.
pub(crate) fn names_its_own_root(path: &Path) -> bool {
    path.has_root()
}

/// The separator to splice with: the one the base directory is already spelled
/// with, so an anchored path reads as a path of the platform it was written on.
///
/// Windows resolves `/` and `\` alike, so this decides what an operator — and a
/// refusal naming the path back to them — reads, never whether the file opens.
fn separator_of(base: &str) -> char {
    base.chars()
        .rev()
        .find(|character| matches!(character, '/' | '\\'))
        .unwrap_or('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule every caller relies on, in one place: a path that names its own
    /// root is handed back untouched even when it carries no drive, and a
    /// relative one is spliced onto the base with the base's own separator.
    ///
    /// The base is a real temporary directory rather than a typed-out `/base`,
    /// because that is what carries a drive prefix on Windows — the only base a
    /// `join` would re-root `/graphs/api` under.
    #[test]
    fn a_path_naming_its_own_root_is_never_re_rooted_under_the_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(anchored(dir.path(), "/graphs/api"), None);
        assert_eq!(
            anchored_path(Some(dir.path()), Path::new("/graphs/api")),
            PathBuf::from("/graphs/api")
        );
        assert_eq!(
            anchored_path(Some(dir.path()), Path::new("./graphs/api")),
            dir.path().join("graphs").join("api")
        );
        assert_eq!(
            anchored_path(None, Path::new("graphs/api")),
            PathBuf::from("graphs/api")
        );
        assert_eq!(anchored(dir.path(), ""), None);
        assert_eq!(
            anchored_path(Some(dir.path()), Path::new("")),
            PathBuf::from("")
        );
    }

    /// A base already ending in a separator is not given a second one, and the
    /// separator spliced with is the base's rather than this host's.
    ///
    /// `separator_of` is asked directly for the Windows spellings: a `C:\…` base
    /// is not a rooted path on a Unix host, so [`anchored`] cannot be handed one
    /// there, but the choice of separator is a decision about a string and holds
    /// wherever it is made.
    #[test]
    fn the_splice_uses_the_base_s_own_separator_exactly_once() {
        assert_eq!(
            anchored(Path::new("/graphs/"), "api.yaml").as_deref(),
            Some("/graphs/api.yaml")
        );
        assert_eq!(separator_of("/graphs/a"), '/');
        assert_eq!(separator_of(r"\\server\share"), '\\');
        assert_eq!(separator_of(r"C:\graphs\a"), '\\');
        // No separator at all: a base that is one name. `/` is the spelling
        // Windows also accepts, so it is the one to guess with.
        assert_eq!(separator_of("graphs"), '/');
    }
}
