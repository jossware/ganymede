//! Where ganymede reads and writes outside the vault.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The environment, read once. Either half can be missing, and `hook`
/// still has to report that.
#[derive(Debug, Clone, PartialEq)]
pub struct Env {
    pub home: Option<PathBuf>,
    /// `<state>/ganymede`, holding `archive.lock` and `errors.jsonl`.
    pub state: Option<PathBuf>,
}

impl Env {
    pub fn read() -> Env {
        Env::from_vars(env::var_os("HOME"), env::var_os("XDG_STATE_HOME"))
    }

    pub fn from_vars(home: Option<OsString>, xdg_state_home: Option<OsString>) -> Env {
        let home = home.filter(|h| !h.is_empty()).map(PathBuf::from);
        // The XDG spec says to ignore a relative value.
        let state = match xdg_state_home.map(PathBuf::from) {
            Some(xdg) if xdg.is_absolute() => Some(xdg.join("ganymede")),
            _ => home.as_ref().map(|h| h.join(".local/state/ganymede")),
        };
        Env { home, state }
    }
}

pub fn archive_lock(state: &Path) -> PathBuf {
    state.join("archive.lock")
}

pub fn error_log(state: &Path) -> PathBuf {
    state.join("errors.jsonl")
}

/// The archive root: `--dir`, else `~/org/Calendar/AI`. Codex may run
/// the hook without a shell, so a leading `~/` is expanded here.
pub fn archive_dir(dir: Option<&Path>, home: &Path) -> PathBuf {
    match dir {
        None => home.join("org/Calendar/AI"),
        Some(dir) => match dir.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => dir.to_path_buf(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: Option<&str>, xdg: Option<&str>) -> Env {
        Env::from_vars(home.map(Into::into), xdg.map(Into::into))
    }

    #[test]
    fn state_defaults_under_home() {
        assert_eq!(
            env(Some("/h"), None),
            Env {
                home: Some("/h".into()),
                state: Some("/h/.local/state/ganymede".into()),
            }
        );
    }

    #[test]
    fn absolute_xdg_state_home_wins() {
        let e = env(Some("/h"), Some("/s"));
        assert_eq!(e.state, Some("/s/ganymede".into()));
    }

    #[test]
    fn relative_xdg_state_home_is_ignored() {
        let e = env(Some("/h"), Some("s"));
        assert_eq!(e.state, Some("/h/.local/state/ganymede".into()));
    }

    #[test]
    fn empty_home_is_missing() {
        assert_eq!(
            env(Some(""), None),
            Env {
                home: None,
                state: None
            }
        );
        assert_eq!(
            env(None, None),
            Env {
                home: None,
                state: None
            }
        );
    }

    #[test]
    fn state_survives_a_missing_home() {
        let e = env(None, Some("/s"));
        assert_eq!(e.home, None);
        assert_eq!(e.state, Some("/s/ganymede".into()));
    }

    #[test]
    fn state_files() {
        let state = Path::new("/s/ganymede");
        assert_eq!(archive_lock(state), Path::new("/s/ganymede/archive.lock"));
        assert_eq!(error_log(state), Path::new("/s/ganymede/errors.jsonl"));
    }

    #[test]
    fn archive_dir_defaults_and_expands_tilde() {
        let home = Path::new("/h");
        assert_eq!(archive_dir(None, home), Path::new("/h/org/Calendar/AI"));
        assert_eq!(
            archive_dir(Some(Path::new("~/vault/AI")), home),
            Path::new("/h/vault/AI")
        );
        assert_eq!(
            archive_dir(Some(Path::new("/abs/AI")), home),
            Path::new("/abs/AI")
        );
        assert_eq!(
            archive_dir(Some(Path::new("~user/AI")), home),
            Path::new("~user/AI")
        );
    }
}
