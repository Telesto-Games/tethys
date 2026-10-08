//! `SourceControl` for git, through the `git` command-line client.
//!
//! Read-only and local: `status --porcelain=v2`, `rev-parse`, `symbolic-ref`,
//! `log`, `ls-remote --get-url` (which only reads config) and `cat-file`.
//! Every command runs with `--no-optional-locks`, so refreshing status never
//! writes the index or contends with an agent running git in the same repo.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tethys_core::ports::{PortResult, SourceControl};
use tethys_core::scm::{ChangeKind, FileStatus, LastChange, WorkingCopyInfo};

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

    /// Runs git in `dir`.
    fn run(&self, dir: &Path, args: &[&str]) -> std::io::Result<Output> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("--no-optional-locks").arg("-C").arg(dir).args(args);
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
            // `#` headers and `!` ignored files.
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
}
