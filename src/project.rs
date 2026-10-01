//! The project and worktree a session's note is filed under.

use std::fs;
use std::path::{Path, PathBuf};

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

/// `pinned` is the `project:` of the note being rewritten. A note never
/// moves once written, so it overrides whatever the cwds now say.
pub fn identify(cwds: &[PathBuf], pinned: Option<&str>, home: &Path) -> ProjectId {
    let resolved: Vec<Resolved> = cwds.iter().map(|dir| resolve(dir, home)).collect();
    let name = match pinned.filter(|name| !name.trim().is_empty()) {
        Some(name) => name.to_string(),
        None => resolved
            .iter()
            .find(|r| r.in_repo)
            .or(resolved.first())
            .map_or_else(|| UNKNOWN.to_string(), |r| r.id.name.clone()),
    };
    let worktree = resolved
        .iter()
        .rev()
        .find(|r| r.id.name == name)
        .and_then(|r| r.id.worktree.clone());
    ProjectId { name, worktree }
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
    match fs::metadata(&jj) {
        Ok(meta) if meta.is_dir() => return main_checkout(level),
        Ok(_) => return jj_workspace(level, &jj),
        Err(_) if present(&jj) => return Level::Broken,
        Err(_) => {}
    }
    let git = level.join(".git");
    match fs::metadata(&git) {
        Ok(meta) if meta.is_dir() => main_checkout(level),
        Ok(_) => git_file(level, &git),
        Err(_) if present(&git) => Level::Broken,
        Err(_) => Level::NoMarker,
    }
}

/// The marker entry exists, whether or not its target can be reached. One
/// that cannot be followed is a broken pointer.
fn present(marker: &Path) -> bool {
    fs::symlink_metadata(marker).is_ok()
}

fn main_checkout(level: &Path) -> Level {
    Level::Checkout {
        main: basename(level),
        worktree: None,
    }
}

fn git_file(level: &Path, file: &Path) -> Level {
    let contents = read_pointer(file).unwrap_or_default();
    let gitdir = contents
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .and_then(|target| follow(level, target));
    let Some(gitdir) = gitdir else {
        return Level::Broken;
    };
    let commondir = gitdir.join("commondir");
    if !commondir.exists() {
        // Submodules and `--separate-git-dir` checkouts own their git dir outright.
        return main_checkout(level);
    }
    let common = read_pointer(&commondir).and_then(|target| follow(&gitdir, &target));
    match common {
        Some(common) => Level::Checkout {
            main: common_name(&common),
            worktree: Some(basename(level)),
        },
        None => Level::Broken,
    }
}

/// A FIFO would block the read, and with it the hook.
fn read_pointer(file: &Path) -> Option<String> {
    fs::metadata(file).ok().filter(fs::Metadata::is_file)?;
    fs::read_to_string(file).ok()
}

/// Every pointer names a git or jj directory. A target that is not one is a
/// broken pointer.
fn follow(base: &Path, target: &str) -> Option<PathBuf> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    fs::canonicalize(base.join(target))
        .ok()
        .filter(|path| path.is_dir())
}

fn common_name(common: &Path) -> String {
    let own = basename(common);
    // `foo/.bare` is a convention for keeping worktrees beside a bare repo.
    if own == ".git" || own == ".bare" {
        return common.parent().map(basename).unwrap_or_default();
    }
    own.strip_suffix(".git").unwrap_or(&own).to_string()
}

fn jj_workspace(level: &Path, pointer: &Path) -> Level {
    let repo = read_pointer(pointer).and_then(|target| follow(&level.join(".jj"), &target));
    match repo
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::parent)
    {
        Some(main) => Level::Checkout {
            main: basename(main),
            worktree: Some(basename(level)),
        },
        None => Level::Broken,
    }
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

    /// Lays out a linked worktree the way git 2.55 does.
    fn link_worktree(common: &Path, wt: &Path) {
        let admin = common.join("worktrees").join(wt.file_name().unwrap());
        write(&admin.join("commondir"), "../..\n");
        write(&wt.join(".git"), &format!("gitdir: {}\n", admin.display()));
    }

    #[test]
    fn worktree_names_main_and_worktree() {
        let root = tmp();
        let main = root.path().join("main");
        let wt = root.path().join("wt");
        mkdir(&main.join(".git"));
        link_worktree(&main.join(".git"), &wt);
        assert_eq!(at(&wt), repo("main", Some("wt")));
        assert_eq!(at(&wt.join("src/deep")), repo("main", Some("wt")));
    }

    #[test]
    fn worktree_with_relative_gitdir() {
        let root = tmp();
        let main = root.path().join("main");
        let wt = root.path().join("wt");
        mkdir(&main.join(".git"));
        link_worktree(&main.join(".git"), &wt);
        write(&wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n");
        assert_eq!(at(&wt), repo("main", Some("wt")));
    }

    #[test]
    fn pointer_whitespace_is_trimmed() {
        let root = tmp();
        let main = root.path().join("main");
        let wt = root.path().join("wt");
        mkdir(&main.join(".git"));
        link_worktree(&main.join(".git"), &wt);
        let admin = main.join(".git/worktrees/wt");
        write(
            &wt.join(".git"),
            &format!("gitdir: {}  \r\n", admin.display()),
        );
        write(&admin.join("commondir"), "../..\r\n");
        assert_eq!(at(&wt), repo("main", Some("wt")));
    }

    #[test]
    fn bare_common_dir_names_itself() {
        let root = tmp();
        let bare = root.path().join("krew-index");
        mkdir(&bare.join("objects"));
        link_worktree(&bare, &bare.join("main"));
        assert_eq!(at(&bare.join("main")), repo("krew-index", Some("main")));
        assert_eq!(at(&bare), plain("krew-index"));
    }

    #[test]
    fn bare_common_dir_drops_git_suffix() {
        let root = tmp();
        let bare = root.path().join("foo.git");
        mkdir(&bare);
        link_worktree(&bare, &root.path().join("foo-wt"));
        assert_eq!(at(&root.path().join("foo-wt")), repo("foo", Some("foo-wt")));
    }

    #[test]
    fn hidden_bare_common_dir_defers_to_its_parent() {
        let root = tmp();
        let foo = root.path().join("foo");
        mkdir(&foo.join(".bare"));
        link_worktree(&foo.join(".bare"), &foo.join("main"));
        assert_eq!(at(&foo.join("main")), repo("foo", Some("main")));
    }

    #[test]
    fn other_hidden_bare_common_dirs_name_themselves() {
        let root = tmp();
        let repos = root.path().join("repos");
        mkdir(&repos.join(".dotfiles"));
        link_worktree(&repos.join(".dotfiles"), &repos.join("wt"));
        assert_eq!(at(&repos.join("wt")), repo("dotfiles", Some("wt")));
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_pointers_are_broken() {
        use std::process::Command;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let root = tmp();
        let main = root.path().join("main");
        mkdir(&main.join(".git"));
        let fifo_git = root.path().join("wt1");
        let fifo_common = root.path().join("wt2");
        let fifo_jj = root.path().join("ws1");
        mkdir(&fifo_git);
        link_worktree(&main.join(".git"), &fifo_common);
        let commondir = main.join(".git/worktrees/wt2/commondir");
        fs::remove_file(&commondir).unwrap();
        mkdir(&fifo_jj.join(".jj"));
        for fifo in [fifo_git.join(".git"), commondir, fifo_jj.join(".jj/repo")] {
            assert!(
                Command::new("mkfifo")
                    .arg(&fifo)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        for (dir, name) in [(fifo_git, "wt1"), (fifo_common, "wt2"), (fifo_jj, "ws1")] {
            let (tx, rx) = mpsc::channel();
            let probe = dir.clone();
            thread::spawn(move || tx.send(at(&probe)).unwrap());
            let got = rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("{name}: blocked reading a FIFO"));
            assert_eq!(got, plain(name), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_marker_symlinks_are_broken() {
        use std::os::unix::fs::symlink;

        let root = tmp();
        let outer = root.path().join("outer");
        mkdir(&outer.join(".git"));
        let git = outer.join("wt1");
        mkdir(&git);
        symlink(root.path().join("gone"), git.join(".git")).unwrap();
        let jj = outer.join("ws1");
        mkdir(&jj.join(".jj"));
        symlink(root.path().join("gone"), jj.join(".jj/repo")).unwrap();

        for (dir, name) in [(git, "wt1"), (jj, "ws1")] {
            assert_eq!(at(&dir.join("src")), plain(name), "{name}");
        }
    }

    #[test]
    fn pointers_to_files_are_broken() {
        let root = tmp();
        let main = root.path().join("main");
        mkdir(&main.join(".git"));
        let stray = root.path().join("stray");
        write(&stray, "not a repo\n");

        let git_to_file = root.path().join("wt1");
        write(
            &git_to_file.join(".git"),
            &format!("gitdir: {}\n", stray.display()),
        );

        let common_to_file = root.path().join("wt2");
        link_worktree(&main.join(".git"), &common_to_file);
        write(
            &main.join(".git/worktrees/wt2/commondir"),
            &stray.display().to_string(),
        );

        let jj_to_file = root.path().join("ws1");
        write(&jj_to_file.join(".jj/repo"), &stray.display().to_string());

        for (dir, name) in [
            (git_to_file, "wt1"),
            (common_to_file, "wt2"),
            (jj_to_file, "ws1"),
        ] {
            assert_eq!(at(&dir), plain(name), "{name}");
        }
    }

    #[test]
    fn submodule_is_its_own_main_checkout() {
        let root = tmp();
        let sup = root.path().join("sup");
        mkdir(&sup.join(".git/modules/themes/sub"));
        write(
            &sup.join("themes/sub/.git"),
            "gitdir: ../../.git/modules/themes/sub\n",
        );
        assert_eq!(at(&sup.join("themes/sub/layouts")), repo("sub", None));
    }

    #[test]
    fn broken_git_pointers_name_the_checkout() {
        let root = tmp();
        let main = root.path().join("main");
        mkdir(&main.join(".git"));

        let missing_gitdir = root.path().join("wt1");
        write(
            &missing_gitdir.join(".git"),
            &format!("gitdir: {}\n", root.path().join("nowhere").display()),
        );

        let missing_common = root.path().join("wt2");
        link_worktree(&main.join(".git"), &missing_common);
        write(
            &main.join(".git/worktrees/wt2/commondir"),
            "../../missing\n",
        );

        let empty = root.path().join("wt3");
        write(&empty.join(".git"), "");

        let no_prefix = root.path().join("wt4");
        write(&no_prefix.join(".git"), "nonsense\n");

        let empty_target = root.path().join("wt5");
        write(&empty_target.join(".git"), "gitdir: \n");

        for (wt, name) in [
            (&missing_gitdir, "wt1"),
            (&missing_common, "wt2"),
            (&empty, "wt3"),
            (&no_prefix, "wt4"),
            (&empty_target, "wt5"),
        ] {
            assert_eq!(at(&wt.join("src")), plain(name), "{name}");
        }
    }

    #[test]
    fn jj_workspace_names_main_and_workspace() {
        let root = tmp();
        let main = root.path().join("main");
        let ws = root.path().join("ws1");
        mkdir(&main.join(".jj/repo"));
        // jj 0.44 writes the pointer relative to the workspace's `.jj`.
        write(&ws.join(".jj/repo"), "../../main/.jj/repo");
        assert_eq!(at(&ws), repo("main", Some("ws1")));
        assert_eq!(at(&ws.join("src")), repo("main", Some("ws1")));
    }

    #[test]
    fn jj_workspace_nested_in_main() {
        let root = tmp();
        let main = root.path().join("main");
        let ws = main.join(".workspaces/ws");
        mkdir(&main.join(".jj/repo"));
        write(&ws.join(".jj/repo"), "../../../.jj/repo");
        assert_eq!(at(&ws), repo("main", Some("ws")));
    }

    #[test]
    fn jj_workspace_with_absolute_pointer() {
        let root = tmp();
        let main = root.path().join("main");
        let ws = root.path().join("ws1");
        mkdir(&main.join(".jj/repo"));
        write(
            &ws.join(".jj/repo"),
            &main.join(".jj/repo").display().to_string(),
        );
        assert_eq!(at(&ws), repo("main", Some("ws1")));
    }

    #[test]
    fn broken_jj_pointer_names_the_workspace() {
        let root = tmp();
        let ws = root.path().join("ws1");
        write(&ws.join(".jj/repo"), "../../gone/.jj/repo");
        assert_eq!(at(&ws.join("src")), plain("ws1"));
    }

    fn id(name: &str, worktree: Option<&str>) -> ProjectId {
        ProjectId {
            name: name.into(),
            worktree: worktree.map(Into::into),
        }
    }

    #[test]
    fn first_cwd_in_a_repo_wins() {
        let root = tmp();
        let (p1, p2) = (root.path().join("p1"), root.path().join("p2"));
        mkdir(&p1.join(".git"));
        mkdir(&p2.join(".git"));
        let cwds = [root.path().to_path_buf(), p1, p2];
        assert_eq!(identify(&cwds, None, root.path()), id("p1", None));
    }

    #[test]
    fn with_no_repo_the_first_cwd_wins() {
        let root = tmp();
        let cwds = [root.path().join("a"), root.path().join("b")];
        assert_eq!(identify(&cwds, None, root.path()), id("a", None));
    }

    #[test]
    fn no_cwds_is_unknown() {
        let root = tmp();
        assert_eq!(identify(&[], None, root.path()), id("unknown", None));
    }

    #[test]
    fn chat_counts_as_a_repo() {
        let root = tmp();
        let cwds = [
            root.path().join("x"),
            root.path().join("Documents/Codex/c1"),
        ];
        assert_eq!(identify(&cwds, None, root.path()), id("chat", None));
    }

    #[test]
    fn worktree_comes_from_the_last_matching_cwd() {
        let root = tmp();
        let main = root.path().join("main");
        let wt = root.path().join("wt");
        mkdir(&main.join(".git"));
        link_worktree(&main.join(".git"), &wt);
        let into_wt = [main.clone(), wt.clone()];
        assert_eq!(
            identify(&into_wt, None, root.path()),
            id("main", Some("wt"))
        );
        let back_to_main = [wt, main];
        assert_eq!(identify(&back_to_main, None, root.path()), id("main", None));
    }

    #[test]
    fn pinned_name_is_kept_without_a_match() {
        let root = tmp();
        let main = root.path().join("main");
        mkdir(&main.join(".git"));
        assert_eq!(
            identify(&[main], Some("old-name"), root.path()),
            id("old-name", None)
        );
    }

    #[test]
    fn pinned_name_takes_its_worktree() {
        let root = tmp();
        let main = root.path().join("main");
        let wt = root.path().join("wt");
        mkdir(&main.join(".git"));
        link_worktree(&main.join(".git"), &wt);
        let cwds = [root.path().join("elsewhere"), wt];
        assert_eq!(
            identify(&cwds, Some("main"), root.path()),
            id("main", Some("wt"))
        );
    }

    #[test]
    fn pinned_name_matches_after_dot_stripping() {
        let root = tmp();
        let dotfiles = root.path().join(".dotfiles");
        let wt = root.path().join("dotfiles-wt");
        mkdir(&dotfiles.join(".git"));
        link_worktree(&dotfiles.join(".git"), &wt);
        assert_eq!(
            identify(&[wt], Some("dotfiles"), root.path()),
            id("dotfiles", Some("dotfiles-wt"))
        );
    }

    #[test]
    fn blank_pinned_name_is_recomputed() {
        let root = tmp();
        let main = root.path().join("main");
        mkdir(&main.join(".git"));
        let cwds = [main];
        for pinned in ["", "  "] {
            assert_eq!(
                identify(&cwds, Some(pinned), root.path()),
                id("main", None),
                "{pinned:?}"
            );
        }
    }
}
