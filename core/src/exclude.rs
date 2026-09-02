//! Filters out noisy paths (VCS metadata, dependency caches, editor lock files) that
//! most people pointing this at a general-purpose folder don't want a toast for on
//! every change.

use std::path::Path;

/// Directory or file names skipped anywhere in a watched tree, by default.
const DEFAULT_EXCLUDED_NAMES: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
];

/// Filename suffixes for the transient/lock files most editors create and remove around
/// every save (the "real" save is reported separately; these on their own are just noise).
const DEFAULT_EXCLUDED_SUFFIXES: &[&str] = &[".tmp", ".swp", ".swx", "~"];

/// Filename prefixes for the same kind of transient file, e.g. Microsoft Office's
/// `~$report.docx` lock files.
const DEFAULT_EXCLUDED_PREFIXES: &[&str] = &["~$", ".~"];

/// Which paths under a watched root should be ignored: the built-in defaults above, plus
/// any names the user added for that folder.
#[derive(Clone, Debug, Default)]
pub struct ExcludeRules {
    custom_names: Vec<String>,
}

impl ExcludeRules {
    pub fn new(custom_names: Vec<String>) -> Self {
        Self { custom_names }
    }

    /// True if `path` — or any ancestor directory in it — should be ignored.
    pub fn is_excluded(&self, path: &Path) -> bool {
        path.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| self.name_matches(name))
        })
    }

    fn name_matches(&self, name: &str) -> bool {
        DEFAULT_EXCLUDED_NAMES
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name))
            || self
                .custom_names
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name))
            || DEFAULT_EXCLUDED_SUFFIXES.iter().any(|s| name.ends_with(s))
            || DEFAULT_EXCLUDED_PREFIXES
                .iter()
                .any(|p| name.starts_with(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn excludes_default_noisy_paths() {
        let rules = ExcludeRules::default();
        assert!(rules.is_excluded(&PathBuf::from("/repo/.git/config")));
        assert!(rules.is_excluded(&PathBuf::from("/repo/node_modules/pkg/index.js")));
        assert!(rules.is_excluded(&PathBuf::from("/docs/~$report.docx")));
        assert!(rules.is_excluded(&PathBuf::from("/docs/notes.txt.swp")));
        assert!(rules.is_excluded(&PathBuf::from("/docs/notes.txt~")));
    }

    #[test]
    fn keeps_ordinary_paths() {
        let rules = ExcludeRules::default();
        assert!(!rules.is_excluded(&PathBuf::from("/docs/report.docx")));
        assert!(!rules.is_excluded(&PathBuf::from("/photos/2026/summer.jpg")));
    }

    #[test]
    fn honors_custom_excludes() {
        let rules = ExcludeRules::new(vec!["drafts".to_string()]);
        assert!(rules.is_excluded(&PathBuf::from("/docs/drafts/idea.txt")));
        assert!(!rules.is_excluded(&PathBuf::from("/docs/final/idea.txt")));
    }
}
