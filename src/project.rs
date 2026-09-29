//! The project and worktree a session's note is filed under.

use std::fs;
use std::path::Path;

const UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectId {
    pub name: String,
    pub worktree: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Resolved {
    id: ProjectId,
    in_repo: bool,
}

impl Resolved {
    fn repo(name: &str, worktree: Option<String>) -> Self {
        Self {
            id: ProjectId {
                name: clean(name),
                worktree,
            },
            in_repo: true,
        }
    }

    fn outside(dir: &Path) -> Self {
        Self {
            id: ProjectId {
                name: clean(&basename(dir)),
                worktree: None,
            },
            in_repo: false,
        }
    }
}

enum Level {
    NoMarker,
    Broken,
    Checkout {
        main: String,
        worktree: Option<String>,
    },
}

fn resolve(dir: &Path, home: &Path) -> Resolved {
    // Codex Desktop makes a fresh directory here for every chat not tied to a folder.
    let chats = home.join("Documents").join("Codex");
    if dir != chats.as_path() && dir.starts_with(&chats) {
        return Resolved::repo("chat", None);
    }
    for level in dir.ancestors() {
        match markers(level) {
            Level::NoMarker => {}
            Level::Broken => return Resolved::outside(level),
            Level::Checkout { main, worktree } => return Resolved::repo(&main, worktree),
        }
    }
    Resolved::outside(dir)
}

fn markers(level: &Path) -> Level {
    let jj = level.join(".jj").join("repo");
    if let Ok(meta) = fs::metadata(&jj) {
        return if meta.is_dir() {
            main_checkout(level)
        } else {
            jj_workspace(level, &jj)
        };
    }
    let git = level.join(".git");
    if let Ok(meta) = fs::metadata(&git) {
        return if meta.is_dir() {
            main_checkout(level)
        } else {
            git_file(level, &git)
        };
    }
    Level::NoMarker
}

fn main_checkout(level: &Path) -> Level {
    Level::Checkout {
        main: basename(level),
        worktree: None,
    }
}

fn git_file(_level: &Path, _file: &Path) -> Level {
    Level::Broken
}

fn jj_workspace(_level: &Path, _pointer: &Path) -> Level {
    Level::Broken
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn clean(name: &str) -> String {
    let name = name.trim_start_matches('.');
    if name.is_empty() {
        UNKNOWN.to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn mkdir(p: &Path) {
        fs::create_dir_all(p).unwrap();
    }

    fn write(p: &Path, contents: &str) {
        mkdir(p.parent().unwrap());
        fs::write(p, contents).unwrap();
    }

    fn at(dir: &Path) -> Resolved {
        resolve(dir, Path::new("/nonexistent-home"))
    }

    fn repo(name: &str, worktree: Option<&str>) -> Resolved {
        Resolved {
            id: ProjectId {
                name: name.into(),
                worktree: worktree.map(Into::into),
            },
            in_repo: true,
        }
    }

    fn plain(name: &str) -> Resolved {
        Resolved {
            id: ProjectId {
                name: name.into(),
                worktree: None,
            },
            in_repo: false,
        }
    }

    #[test]
    fn git_directory_names_the_checkout() {
        let root = tmp();
        let proj = root.path().join("proj");
        mkdir(&proj.join(".git"));
        assert_eq!(at(&proj), repo("proj", None));
        assert_eq!(at(&proj.join("src/deep")), repo("proj", None));
    }

    #[test]
    fn jj_repo_directory_names_the_checkout() {
        let root = tmp();
        let proj = root.path().join("proj");
        mkdir(&proj.join(".jj/repo"));
        assert_eq!(at(&proj.join("src")), repo("proj", None));
    }

    #[test]
    fn colocated_jj_and_git_name_the_checkout() {
        let root = tmp();
        let proj = root.path().join("proj");
        mkdir(&proj.join(".jj/repo"));
        mkdir(&proj.join(".git"));
        assert_eq!(at(&proj), repo("proj", None));
    }

    #[test]
    fn jj_without_repo_is_not_a_marker() {
        let root = tmp();
        let proj = root.path().join("proj");
        mkdir(&proj.join(".git"));
        mkdir(&proj.join("sub/.jj"));
        assert_eq!(at(&proj.join("sub")), repo("proj", None));
    }

    #[test]
    fn missing_dir_inside_a_checkout_resolves_to_it() {
        let root = tmp();
        let proj = root.path().join("proj");
        mkdir(&proj.join(".git"));
        assert_eq!(at(&proj.join(".workspaces/gone/src")), repo("proj", None));
    }

    #[test]
    fn no_marker_names_the_directory() {
        let root = tmp();
        let dir = root.path().join("a/b");
        mkdir(&dir);
        assert_eq!(at(&dir), plain("b"));
    }

    #[test]
    fn codex_chat_directories_are_chat() {
        let home = tmp();
        let chats = home.path().join("Documents/Codex");
        assert_eq!(
            resolve(&chats.join("2026-05-21"), home.path()),
            repo("chat", None)
        );
        assert_eq!(resolve(&chats, home.path()), plain("Codex"));
    }

    #[test]
    fn leading_dots_are_stripped() {
        let root = tmp();
        mkdir(&root.path().join(".dotfiles/.git"));
        assert_eq!(at(&root.path().join(".dotfiles")), repo("dotfiles", None));
        assert_eq!(at(&root.path().join("...")), plain("unknown"));
        assert_eq!(at(Path::new("/")), plain("unknown"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_are_lossy() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let root = tmp();
        let dir = root.path().join(OsStr::from_bytes(b"caf\xe9"));
        assert_eq!(at(&dir), plain("caf\u{FFFD}"));
    }
}
