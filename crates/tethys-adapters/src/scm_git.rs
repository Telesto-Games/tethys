//! `SourceControl` for git, through the `git` command-line client.
//!
//! Local only: `status --porcelain=v2`, `rev-parse`, `symbolic-ref`, `log`,
//! `ls-files`, `ls-remote --get-url` (which only reads config) and
//! `cat-file`. The only changes made are `git mv` and `git rm`, when the user
//! renames or deletes. Every command runs with `--no-optional-locks`, so
//! refreshing status never writes the index or contends with an agent
//! running git in the same repo.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tethys_core::ports::{PortResult, SourceControl};
use tethys_core::scm::{ChangeKind, Commit, FileStatus, LastChange, WorkingCopyInfo};

#[derive(Debug, Clone)]
pub struct Git {
    /// The `git` executable; `git` from PATH by default.
    program: PathBuf,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            program: PathBuf::from("git"),
        }
    }
}

impl Git {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the `git` client can be run at all.
    pub fn is_available(&self) -> bool {
        Command::new(&self.program)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Runs git in `dir`. File names are passed as pathspecs, so
    /// `--literal-pathspecs` keeps `[`, `*` and the like in a name from
    /// matching other files (`rm` would delete them).
    fn run(&self, dir: &Path, args: &[&str]) -> std::io::Result<Output> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("--no-optional-locks")
            .arg("--literal-pathspecs")
            .arg("-C")
            .arg(dir)
            .args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // git is a console program: don't flash a console window from the GUI.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.output()
    }

    /// Standard output of a command that must succeed.
    fn output(&self, dir: &Path, args: &[&str]) -> PortResult<String> {
        let out = self.run(dir, args)?;
        if !out.status.success() {
            return Err(failed(args, &out).into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Standard output of a yes/no query: `None` when git exits with 1, which
    /// `--verify -q` and `symbolic-ref -q` use for "no such thing". Any other
    /// failure is an error.
    fn query(&self, dir: &Path, args: &[&str]) -> PortResult<Option<String>> {
        let out = self.run(dir, args)?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
            Some(1) => Ok(None),
            _ => Err(failed(args, &out).into()),
        }
    }
}

fn failed(args: &[&str], out: &Output) -> String {
    format!(
        "git {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// Drops the newline git ends single-value output with.
fn line(s: String) -> String {
    s.trim_end_matches(['\r', '\n']).to_string()
}

impl SourceControl for Git {
    fn name(&self) -> &'static str {
        "Git"
    }

    fn working_copy_root(&self, dir: &Path) -> Option<PathBuf> {
        // `.git` is a folder, or a file in linked worktrees and submodules.
        dir.ancestors()
            .find(|d| d.join(".git").exists())
            .map(Path::to_path_buf)
    }

    fn base_label(&self) -> &'static str {
        "HEAD"
    }

    fn info(&self, root: &Path) -> PortResult<WorkingCopyInfo> {
        let toplevel = line(self.output(root, &["rev-parse", "--show-toplevel"])?);
        // Detached HEAD has no branch.
        let branch = self
            .query(root, &["symbolic-ref", "--short", "-q", "HEAD"])?
            .map(line);
        // A new repository has no commits yet.
        let revision = self
            .query(root, &["rev-parse", "--short", "-q", "--verify", "HEAD"])?
            .map(line);
        let last_change = match revision {
            Some(_) => parse_log(
                &self.output(root, &["log", "-1", "--format=%h%x00%an%x00%cI", "--", "."])?,
            ),
            None => None,
        };
        // The branch's upstream remote, or origin; the repository folder without one.
        let url = match self.run(root, &["ls-remote", "--get-url"])? {
            out if out.status.success() => line(String::from_utf8_lossy(&out.stdout).into()),
            _ => native_path(&toplevel).display().to_string(),
        };
        Ok(WorkingCopyInfo {
            url,
            branch,
            revision,
            last_change,
        })
    }

    fn status(&self, root: &Path) -> PortResult<Vec<FileStatus>> {
        // Status paths are relative to the repository, which may be above `root`.
        let prefix = line(self.output(root, &["rev-parse", "--show-prefix"])?);
        let out = self.output(
            root,
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=normal",
                // Ignored folders are named once, not file by file.
                "--ignored=traditional",
                "--",
                ".",
            ],
        )?;
        parse_status(&out, &prefix, root)
    }

    fn base_text(&self, file: &Path) -> PortResult<Option<String>> {
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
            return Ok(None);
        };
        let name = name
            .to_str()
            .ok_or_else(|| format!("{} isn't a UTF-8 path", file.display()))?;
        // `./` makes the path relative to `dir` rather than the repository.
        let spec = format!("HEAD:./{name}");
        // Not in HEAD: unversioned, newly added, or no commits yet.
        if self
            .query(dir, &["rev-parse", "-q", "--verify", &spec])?
            .is_none()
        {
            return Ok(None);
        }
        // `--filters` converts line endings as a checkout would, so the diff
        // doesn't show every line changed under `core.autocrlf`.
        Ok(Some(self.output(dir, &["cat-file", "--filters", &spec])?))
    }

    fn is_versioned(&self, path: &Path) -> PortResult<bool> {
        let (dir, name) = split(path)?;
        // Lists the tracked file, or the tracked files in a folder.
        Ok(!self
            .output(dir, &["ls-files", "-z", "--", name])?
            .is_empty())
    }

    fn rename(&self, from: &Path, to: &Path) -> PortResult<()> {
        let (dir, old) = split(from)?;
        let (_, new) = split(to)?;
        self.output(dir, &["mv", "--", old, new])?;
        Ok(())
    }

    fn delete(&self, path: &Path) -> PortResult<()> {
        let (dir, name) = split(path)?;
        self.output(dir, &["rm", "-r", "-f", "-q", "--", name])?;
        Ok(())
    }

    fn history(&self, file: &Path, limit: usize) -> PortResult<Vec<Commit>> {
        let (dir, name) = split(file)?;
        // The file's path from the top of the repository, for commits whose
        // record doesn't name it.
        let prefix = line(self.output(dir, &["rev-parse", "--show-prefix"])?);
        let limit = format!("-n{limit}");
        let out = self.output(
            dir,
            &[
                "log",
                "--follow",
                &limit,
                "--format=%x1e%H%x00%an%x00%cI%x00%s",
                "--name-status",
                "-z",
                "--",
                name,
            ],
        )?;
        Ok(parse_history(&out, &format!("{prefix}{name}")))
    }

    fn change(&self, file: &Path, commit: &Commit) -> PortResult<(Option<String>, Option<String>)> {
        let (dir, _) = split(file)?;
        let (before, after) = commit.locator.split_once('\0').unwrap_or_default();
        let text = |spec: String| -> PortResult<Option<String>> {
            // A path the commit has no version of, or a root commit's parent.
            if self
                .query(dir, &["rev-parse", "-q", "--verify", &spec])?
                .is_none()
            {
                return Ok(None);
            }
            Ok(Some(self.output(dir, &["cat-file", "--filters", &spec])?))
        };
        // Locator paths are from the top of the repository, as `rev:path` reads them.
        let before = match before {
            "" => None,
            path => text(format!("{}^:{path}", commit.revision))?,
        };
        let after = match after {
            "" => None,
            path => text(format!("{}:{path}", commit.revision))?,
        };
        Ok((before, after))
    }
}

/// `path`'s folder, and its name as UTF-8.
fn split(path: &Path) -> PortResult<(&Path, &str)> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(format!("{} has no parent folder", path.display()).into());
    };
    let name = name
        .to_str()
        .ok_or_else(|| format!("{} isn't a UTF-8 path", path.display()))?;
    Ok((dir, name))
}

/// Parses `git log --format=%x1e%H%x00%an%x00%cI%x00%s --name-status -z`.
///
/// Each record is the header fields, then the file's status: `M`/`A`/`D`
/// and a path, or `R…`/`C…` and the old and new paths. The locator is the
/// path before and after the commit, from the top of the repository,
/// separated by a NUL; empty where the file didn't exist.
///
/// A merge commit has no status lines (git doesn't diff merges without
/// `-m`), so its path is taken from the next newer commit, whose "before"
/// is where the file was after the merge; `path` for the newest commit.
pub fn parse_history(out: &str, path: &str) -> Vec<Commit> {
    let mut newer_before: Option<String> = None;
    out.split('\x1e')
        .filter_map(|record| {
            let mut fields = record.split('\0').map(|f| f.trim_matches('\n'));
            let revision = fields.next().filter(|r| !r.is_empty())?.to_string();
            let author = fields.next().unwrap_or_default().to_string();
            let date = fields.next().unwrap_or_default().to_string();
            let message = fields.next().unwrap_or_default().to_string();
            let (before, after) = match fields.next().unwrap_or_default() {
                s if s.starts_with('A') => {
                    (String::new(), fields.next().unwrap_or_default().into())
                }
                s if s.starts_with('D') => {
                    (fields.next().unwrap_or_default().into(), String::new())
                }
                s if s.starts_with('R') || s.starts_with('C') => {
                    let old = fields.next().unwrap_or_default().to_string();
                    (old, fields.next().unwrap_or_default().into())
                }
                // No status: a merge. The same path both sides.
                "" => {
                    let p = newer_before.clone().unwrap_or_else(|| path.to_string());
                    (p.clone(), p)
                }
                // M, T: the same path both sides.
                _ => {
                    let p = fields.next().unwrap_or_default().to_string();
                    (p.clone(), p)
                }
            };
            if !before.is_empty() {
                newer_before = Some(before.clone());
            }
            Some(Commit {
                revision,
                author,
                date,
                message,
                locator: format!("{before}\0{after}"),
            })
        })
        .collect()
}

/// Parses `git log --format=%h%x00%an%x00%cI`; `None` when there's no commit.
pub fn parse_log(out: &str) -> Option<LastChange> {
    let mut fields = out.trim_end_matches(['\r', '\n']).split('\0');
    let revision = fields.next().filter(|r| !r.is_empty())?.to_string();
    Some(LastChange {
        revision,
        author: fields.next().unwrap_or_default().to_string(),
        date: fields.next().unwrap_or_default().to_string(),
    })
}

/// Parses `git status --porcelain=v2 -z`. Paths in it are relative to the
/// repository; `prefix` is `root`'s path within the repository (`Proj/`, or
/// empty at the top), as `git rev-parse --show-prefix` prints it.
pub fn parse_status(out: &str, prefix: &str, root: &Path) -> PortResult<Vec<FileStatus>> {
    let mut files = Vec::new();
    let mut push = |path: &str, kind| {
        if let Some(path) = under_root(root, prefix, path) {
            files.push(FileStatus { path, kind });
        }
    };
    let mut records = out.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        match record.as_bytes()[0] {
            // `1 XY sub mH mI mW hH hI path`
            b'1' => {
                let (xy, path) = fields(record, 8)?;
                push(path, tracked_kind(xy));
            }
            // `2 XY sub mH mI mW hH hI Xscore path`, then the original path.
            b'2' => {
                let (xy, path) = fields(record, 9)?;
                let orig = records
                    .next()
                    .ok_or("git status: rename without its original path")?;
                push(path, tracked_kind(xy));
                // A rename leaves its old path behind (a copy doesn't).
                if xy.starts_with('R') {
                    push(orig, ChangeKind::Deleted);
                }
            }
            // `u XY sub m1 m2 m3 mW h1 h2 h3 path`
            b'u' => {
                let (_, path) = fields(record, 10)?;
                push(path, ChangeKind::Conflicted);
            }
            b'?' => push(&record[2..], ChangeKind::Unversioned),
            b'!' => push(&record[2..], ChangeKind::Ignored),
            // `#` headers.
            _ => {}
        }
    }
    Ok(files)
}

/// Splits a status record into its XY field and the path after `skip` fields.
fn fields(record: &str, skip: usize) -> PortResult<(&str, &str)> {
    let mut parts = record.splitn(skip + 1, ' ');
    let xy = parts.nth(1);
    match (xy, parts.nth(skip - 2)) {
        (Some(xy), Some(path)) if xy.len() == 2 => Ok((xy, path)),
        _ => Err(format!("git status: can't read {record:?}").into()),
    }
}

/// What an ordinary or renamed entry's XY (index, then worktree) means.
fn tracked_kind(xy: &str) -> ChangeKind {
    let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
    if x == b'D' || y == b'D' {
        ChangeKind::Deleted
    } else if matches!(x, b'A' | b'R' | b'C') || y == b'A' {
        // `y == A` is `git add -N`.
        ChangeKind::Added
    } else {
        ChangeKind::Modified
    }
}

/// A repository-relative `a/b/c` (or `a/b/` for a folder) as a path under
/// `root`, or `None` if it's outside `root`.
fn under_root(root: &Path, prefix: &str, path: &str) -> Option<PathBuf> {
    let rel = path.strip_prefix(prefix)?.trim_end_matches('/');
    if rel.is_empty() {
        return None;
    }
    let mut full = root.to_path_buf();
    full.extend(rel.split('/'));
    Some(full)
}

/// `C:/a/b` as git prints it, with the platform's separators.
fn native_path(path: &str) -> PathBuf {
    Path::new(path).components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2_status() {
        let out = [
            "# branch.oid 0d7967a",
            "1 .M N... 100644 100644 100644 422c 422c Proj/Src/x.txt",
            "1 A. N... 000000 100644 100644 0000 b680 Proj/added file.txt",
            "1 .D N... 100644 100644 000000 0105 0105 Proj/gone.txt",
            "2 R. N... 100644 100644 100644 4286 4286 R100 Proj/ren2.txt",
            "Proj/ren.txt",
            "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc Proj/both.txt",
            "? Proj/Src/new.txt",
            "? Proj/U/",
            "! Proj/Intermediate/",
            "1 .M N... 100644 100644 100644 422c 422c Other/outside.txt",
            "",
        ]
        .join("\0");
        let root = Path::new(r"D:\r\Proj");
        let got: Vec<_> = parse_status(&out, "Proj/", root)
            .unwrap()
            .into_iter()
            .map(|f| (f.path.display().to_string(), f.kind))
            .collect();
        let want = [
            (r"D:\r\Proj\Src\x.txt", ChangeKind::Modified),
            (r"D:\r\Proj\added file.txt", ChangeKind::Added),
            (r"D:\r\Proj\gone.txt", ChangeKind::Deleted),
            (r"D:\r\Proj\ren2.txt", ChangeKind::Added),
            (r"D:\r\Proj\ren.txt", ChangeKind::Deleted),
            (r"D:\r\Proj\both.txt", ChangeKind::Conflicted),
            (r"D:\r\Proj\Src\new.txt", ChangeKind::Unversioned),
            (r"D:\r\Proj\U", ChangeKind::Unversioned),
            (r"D:\r\Proj\Intermediate", ChangeKind::Ignored),
        ];
        let want: Vec<_> = want.iter().map(|(p, k)| (p.to_string(), *k)).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn rejects_truncated_records() {
        assert!(parse_status("1 .M N... 100644\0", "", Path::new(r"D:\r")).is_err());
    }

    #[test]
    fn parses_log_line() {
        let last = parse_log("0d7967a\0Chris\x002026-10-08T16:36:32+01:00\n").unwrap();
        assert_eq!(last.revision, "0d7967a");
        assert_eq!(last.author, "Chris");
        assert!(last.date.starts_with("2026-10-08"));
        assert_eq!(parse_log(""), None);
    }

    /// End to end against a scratch repository, when git exists.
    #[test]
    fn status_info_and_base_against_a_real_repository() {
        let git = Git::new();
        if !git.is_available() {
            eprintln!("git not found; skipping");
            return;
        }
        let base = std::env::temp_dir().join(format!("tethys-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let proj = repo.join("Proj");
        std::fs::create_dir_all(&proj).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=Tester", "-c", "user.email=t@example.com"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "core.autocrlf", "false"]);

        // No commits yet: no base, no revision.
        std::fs::write(proj.join("a.txt"), "one\ntwo\n").unwrap();
        assert_eq!(git.base_text(&proj.join("a.txt")).unwrap(), None);
        let info = git.info(&proj).unwrap();
        assert_eq!(info.revision, None);
        assert_eq!(info.branch.as_deref(), Some("main"));

        std::fs::write(repo.join("top.txt"), "t\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        std::fs::write(proj.join("a.txt"), "one\n2\n").unwrap();
        std::fs::write(repo.join("top.txt"), "changed\n").unwrap();
        std::fs::write(proj.join("new.txt"), "x\n").unwrap();

        assert_eq!(git.working_copy_root(&proj.join("sub")), Some(repo.clone()));
        assert_eq!(git.working_copy_root(&base), None);

        let info = git.info(&proj).unwrap();
        assert_eq!(info.branch.as_deref(), Some("main"));
        let last = info.last_change.unwrap();
        assert_eq!(Some(last.revision), info.revision);
        assert_eq!(last.author, "Tester");
        // No remote: the repository folder.
        assert!(Path::new(&info.url).ends_with("repo"), "{}", info.url);

        // Only changes under the project folder, not `top.txt` above it.
        let mut status = git.status(&proj).unwrap();
        status.sort_by(|a, b| a.path.cmp(&b.path));
        let got: Vec<_> = status.iter().map(|f| (f.path.clone(), f.kind)).collect();
        assert_eq!(
            got,
            [
                (proj.join("a.txt"), ChangeKind::Modified),
                (proj.join("new.txt"), ChangeKind::Unversioned),
            ]
        );

        assert_eq!(
            git.base_text(&proj.join("a.txt")).unwrap().as_deref(),
            Some("one\ntwo\n")
        );
        assert_eq!(git.base_text(&proj.join("new.txt")).unwrap(), None);
        run(&["add", "Proj/new.txt"]);
        assert_eq!(git.base_text(&proj.join("new.txt")).unwrap(), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_history_records() {
        let out = "\x1eaaa\0Chris\x002026-10-13T09:00:00+01:00\0Rename\0\nR095\0Proj/a.txt\0Proj/b.txt\0\
                   \x1ebbb\0Sam\x002026-10-12T09:00:00+01:00\0Edit\0\nM\0Proj/a.txt\0\
                   \x1eccc\0Sam\x002026-10-11T09:00:00+01:00\0Add\0\nA\0Proj/a.txt\0";
        let log = parse_history(out, "Proj/b.txt");
        let got: Vec<_> = log
            .iter()
            .map(|c| (c.revision.as_str(), c.message.as_str(), c.locator.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("aaa", "Rename", "Proj/a.txt\0Proj/b.txt"),
                ("bbb", "Edit", "Proj/a.txt\0Proj/a.txt"),
                ("ccc", "Add", "\0Proj/a.txt"),
            ]
        );
        assert_eq!(log[0].author, "Chris");
    }

    /// A merge commit has no status lines: it takes the path the file had
    /// just after it, from the next newer commit, or the file's own path.
    #[test]
    fn merge_commits_take_the_path_from_the_newer_commit() {
        let out = "\x1eaaa\0Chris\x002026-10-13T09:00:00+01:00\0Merge\0\n\
                   \x1ebbb\0Chris\x002026-10-12T09:00:00+01:00\0Rename\0\nR095\0Proj/a.txt\0Proj/b.txt\0\
                   \x1eccc\0Sam\x002026-10-11T09:00:00+01:00\0Merge\0\n\
                   \x1eddd\0Sam\x002026-10-10T09:00:00+01:00\0Add\0\nA\0Proj/a.txt\0";
        let log = parse_history(out, "Proj/b.txt");
        let got: Vec<_> = log
            .iter()
            .map(|c| (c.revision.as_str(), c.locator.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("aaa", "Proj/b.txt\0Proj/b.txt"),
                ("bbb", "Proj/a.txt\0Proj/b.txt"),
                ("ccc", "Proj/a.txt\0Proj/a.txt"),
                ("ddd", "\0Proj/a.txt"),
            ]
        );
    }

    /// Rename, history, a commit's change and delete, against a scratch
    /// repository when git exists.
    #[test]
    fn edits_and_history_against_a_real_repository() {
        let git = Git::new();
        if !git.is_available() {
            eprintln!("git not found; skipping");
            return;
        }
        let base = std::env::temp_dir().join(format!("tethys-git-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let proj = repo.join("Proj");
        std::fs::create_dir_all(&proj).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=Tester", "-c", "user.email=t@example.com"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "core.autocrlf", "false"]);
        std::fs::write(proj.join("a.txt"), "one\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "add a"]);
        std::fs::write(proj.join("a.txt"), "one\ntwo\n").unwrap();
        run(&["commit", "-q", "-am", "grow a"]);

        std::fs::write(proj.join("loose.txt"), "x\n").unwrap();
        assert!(git.is_versioned(&proj.join("a.txt")).unwrap());
        assert!(
            git.is_versioned(&proj).unwrap(),
            "a folder with tracked files"
        );
        assert!(!git.is_versioned(&proj.join("loose.txt")).unwrap());

        git.rename(&proj.join("a.txt"), &proj.join("b.txt"))
            .unwrap();
        run(&["commit", "-q", "-m", "rename"]);
        let b = proj.join("b.txt");
        let log = git.history(&b, 10).unwrap();
        let messages: Vec<_> = log.iter().map(|c| c.message.as_str()).collect();
        assert_eq!(messages, ["rename", "grow a", "add a"]);

        // The rename changed nothing; before it, the file had another name.
        assert_eq!(
            git.change(&b, &log[0]).unwrap(),
            (Some("one\ntwo\n".into()), Some("one\ntwo\n".into()))
        );
        assert_eq!(
            git.change(&b, &log[1]).unwrap(),
            (Some("one\n".into()), Some("one\ntwo\n".into()))
        );
        // The root commit added it.
        assert_eq!(
            git.change(&b, &log[2]).unwrap(),
            (None, Some("one\n".into()))
        );

        // A name with glob characters is taken literally: `[b].txt` is
        // itself, not `b.txt`.
        let glob = proj.join("[b].txt");
        std::fs::write(&glob, "g\n").unwrap();
        assert!(!git.is_versioned(&glob).unwrap());
        run(&["--literal-pathspecs", "add", "Proj/[b].txt"]);
        assert!(git.is_versioned(&glob).unwrap());
        git.delete(&glob).unwrap();
        assert!(!glob.exists());
        assert!(b.exists(), "deleting [b].txt must leave b.txt alone");

        std::fs::write(&b, "edited\n").unwrap();
        git.delete(&b).unwrap();
        assert!(!b.exists(), "deleted from disk, local edit and all");
        assert!(!git.is_versioned(&b).unwrap());
        let _ = std::fs::remove_dir_all(&base);
    }
}
