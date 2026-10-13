//! `SourceControl` for Subversion, through the `svn` command-line client.
//!
//! Status and diffs use `svn status --xml` and `svn cat -r BASE`. `-r BASE`
//! reads the working copy's pristine copy, so neither touches the network.
//! History (`svn log`, `svn cat -r N`) asks the repository. The only changes
//! made are `svn move` and `svn delete`, when the user renames or deletes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use tethys_core::ports::{PortResult, SourceControl};
use tethys_core::scm::{ChangeKind, Commit, FileStatus, LastChange, WorkingCopyInfo};

#[derive(Debug, Clone)]
pub struct Subversion {
    /// The `svn` executable; `svn` from PATH by default.
    program: PathBuf,
}

impl Default for Subversion {
    fn default() -> Self {
        Self {
            program: PathBuf::from("svn"),
        }
    }
}

impl Subversion {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the `svn` client can be run at all.
    pub fn is_available(&self) -> bool {
        self.run(&["--version", "--quiet"])
            .is_ok_and(|o| o.status.success())
    }

    fn run(&self, args: &[&str]) -> std::io::Result<Output> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args).arg("--non-interactive");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // svn is a console program: don't flash a console window from the GUI.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.output()
    }
}

/// `@` marks a peg revision in svn paths; a trailing `@` escapes any in the name.
fn svn_path(path: &Path) -> String {
    format!("{}@", path.display())
}

impl SourceControl for Subversion {
    fn name(&self) -> &'static str {
        "Subversion"
    }

    fn working_copy_root(&self, dir: &Path) -> Option<PathBuf> {
        // Subversion 1.7+ keeps a single `.svn` at the working copy root.
        dir.ancestors()
            .find(|d| d.join(".svn").is_dir())
            .map(Path::to_path_buf)
    }

    fn info(&self, root: &Path) -> PortResult<WorkingCopyInfo> {
        let target = svn_path(root);
        let out = self.run(&["info", "--xml", &target])?;
        if !out.status.success() {
            return Err(format!(
                "svn info failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        parse_info(&String::from_utf8_lossy(&out.stdout))
    }

    fn status(&self, root: &Path) -> PortResult<Vec<FileStatus>> {
        let target = svn_path(root);
        // `--no-ignore` reports ignored items too, so the tree can mark them.
        let out = self.run(&["status", "--xml", "--no-ignore", &target])?;
        if !out.status.success() {
            return Err(format!(
                "svn status failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        parse_status(&String::from_utf8_lossy(&out.stdout), root)
    }

    fn base_text(&self, file: &Path) -> PortResult<Option<String>> {
        let target = svn_path(file);
        let out = self.run(&["cat", "-r", "BASE", &target])?;
        if out.status.success() {
            return Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Not under version control, or added and never committed: no base.
        if [
            "W200005", "E200005", "E200009", "E155010", "E195002", "W155010",
        ]
        .iter()
        .any(|code| stderr.contains(code))
        {
            return Ok(None);
        }
        Err(format!("svn cat failed: {}", stderr.trim()).into())
    }

    fn is_versioned(&self, path: &Path) -> PortResult<bool> {
        let target = svn_path(path);
        let out = self.run(&["info", "--xml", &target])?;
        if out.status.success() {
            return Ok(true);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Not under version control (W155010), reported as E200009.
        if ["W155010", "E200009"]
            .iter()
            .any(|code| stderr.contains(code))
        {
            return Ok(false);
        }
        Err(format!("svn info failed: {}", stderr.trim()).into())
    }

    fn rename(&self, from: &Path, to: &Path) -> PortResult<()> {
        let (from, to) = (svn_path(from), svn_target(to));
        self.checked(&["move", "-q", &from, &to])
    }

    fn delete(&self, path: &Path) -> PortResult<()> {
        let target = svn_path(path);
        self.checked(&["delete", "-q", "--force", &target])
    }

    fn history(&self, file: &Path, limit: usize) -> PortResult<Vec<Commit>> {
        let (target, limit) = (svn_path(file), limit.to_string());
        let out = self.run(&["log", "--xml", "-l", &limit, &target])?;
        if !out.status.success() {
            return Err(format!(
                "svn log failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        parse_log(&String::from_utf8_lossy(&out.stdout))
    }

    fn change(&self, file: &Path, commit: &Commit) -> PortResult<(Option<String>, Option<String>)> {
        let revision: u64 = commit
            .revision
            .parse()
            .map_err(|_| format!("{} isn't a Subversion revision", commit.revision))?;
        // Pegged at BASE, svn follows the file back through renames.
        let target = format!("{}@BASE", file.display());
        let text = |revision: u64| -> PortResult<Option<String>> {
            let revision = revision.to_string();
            let out = self.run(&["cat", "-r", &revision, &target])?;
            if out.status.success() {
                return Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()));
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            // The file didn't exist at that revision.
            if ["E195012", "E160013", "W160013"]
                .iter()
                .any(|code| stderr.contains(code))
            {
                return Ok(None);
            }
            Err(format!("svn cat failed: {}", stderr.trim()).into())
        };
        Ok((text(revision - 1)?, text(revision)?))
    }
}

impl Subversion {
    /// Runs a command that must succeed.
    fn checked(&self, args: &[&str]) -> PortResult<()> {
        let out = self.run(args)?;
        if !out.status.success() {
            return Err(format!(
                "svn {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            )
            .into());
        }
        Ok(())
    }
}

/// A destination path: svn refuses a peg revision there, so `@` is only
/// escaped when the name has one.
fn svn_target(path: &Path) -> String {
    let path = path.display().to_string();
    if path.contains('@') {
        format!("{path}@")
    } else {
        path
    }
}

/// Parses `svn log --xml`.
pub fn parse_log(xml: &str) -> PortResult<Vec<Commit>> {
    let mut reader = Reader::from_str(xml);
    let mut commits = Vec::new();
    let mut commit: Option<Commit> = None;
    let mut current = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_string();
                if name == "logentry" {
                    commit = Some(Commit {
                        revision: attribute(&e, "revision")?.unwrap_or_default(),
                        author: String::new(),
                        date: String::new(),
                        message: String::new(),
                        locator: String::new(),
                    });
                }
                current = name;
            }
            Event::Text(t) => {
                if let Some(commit) = commit.as_mut() {
                    let text = t.xml10_content().into_owned();
                    match current.as_str() {
                        "author" => commit.author.push_str(&text),
                        "date" => commit.date.push_str(&text),
                        "msg" => commit.message.push_str(&text),
                        _ => {}
                    }
                }
            }
            Event::GeneralRef(r) => {
                // An entity in the message text, e.g. `&amp;`.
                if let (Some(commit), "msg") = (commit.as_mut(), current.as_str())
                    && let Some(c) = r.resolve_char_ref()?.or_else(|| match r.as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    })
                {
                    commit.message.push(c);
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == "logentry"
                    && let Some(mut commit) = commit.take()
                {
                    commit.message = commit.message.trim().to_string();
                    commits.push(commit);
                }
                current.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(commits)
}

/// Parses `svn info --xml` for the first entry.
pub fn parse_info(xml: &str) -> PortResult<WorkingCopyInfo> {
    let mut reader = Reader::from_str(xml);
    let mut info = WorkingCopyInfo::default();
    let mut commit_revision: Option<String> = None;
    let (mut author, mut date) = (None, None);
    // The element whose text we're reading, and whether we're inside <commit>.
    let mut current = String::new();
    let mut in_commit = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_string();
                match name.as_str() {
                    "entry" if info.revision.is_none() => {
                        info.revision = attribute(&e, "revision")?;
                    }
                    "commit" => {
                        in_commit = true;
                        commit_revision = attribute(&e, "revision")?;
                    }
                    _ => {}
                }
                current = name;
            }
            Event::Text(t) => {
                let text = t.xml10_content().trim().to_string();
                if text.is_empty() {
                    continue;
                }
                match (current.as_str(), in_commit) {
                    ("url", false) => info.url = text,
                    ("relative-url", false) => info.branch = Some(text),
                    ("author", true) => author = Some(text),
                    ("date", true) => date = Some(text),
                    _ => {}
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == "commit" {
                    in_commit = false;
                }
                if e.name().as_ref() == "entry" {
                    break;
                }
                current.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some(revision) = commit_revision {
        info.last_change = Some(LastChange {
            revision,
            author: author.unwrap_or_default(),
            date: date.unwrap_or_default(),
        });
    }
    if info.url.is_empty() {
        return Err("svn info returned no URL".into());
    }
    Ok(info)
}

/// Parses `svn status --xml`. Relative entry paths are resolved against `root`.
pub fn parse_status(xml: &str, root: &Path) -> PortResult<Vec<FileStatus>> {
    let mut reader = Reader::from_str(xml);
    let mut files = Vec::new();
    let mut entry_path: Option<String> = None;
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                "entry" => {
                    entry_path = attribute(&e, "path")?;
                }
                "wc-status" => {
                    let item = attribute(&e, "item")?.unwrap_or_default();
                    if let (Some(kind), Some(path)) = (change_kind(&item), entry_path.as_ref()) {
                        let path = PathBuf::from(path);
                        let path = if path.is_absolute() {
                            path
                        } else {
                            root.join(path)
                        };
                        files.push(FileStatus { path, kind });
                    }
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == "entry" => entry_path = None,
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(files)
}

fn attribute(e: &quick_xml::events::BytesStart, name: &str) -> PortResult<Option<String>> {
    for attr in e.attributes() {
        let attr = attr?;
        if attr.key.as_ref() == name {
            return Ok(Some(
                attr.normalized_value(XmlVersion::Implicit1_0)?.into_owned(),
            ));
        }
    }
    Ok(None)
}

fn change_kind(item: &str) -> Option<ChangeKind> {
    Some(match item {
        "modified" => ChangeKind::Modified,
        "added" => ChangeKind::Added,
        "deleted" => ChangeKind::Deleted,
        "missing" => ChangeKind::Missing,
        "replaced" => ChangeKind::Replaced,
        "conflicted" => ChangeKind::Conflicted,
        "unversioned" => ChangeKind::Unversioned,
        "ignored" => ChangeKind::Ignored,
        // normal, external, incomplete, obstructed, none
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<status>
<target path="D:\p">
<entry path="D:\p\.claude\scheduled_tasks.lock">
<wc-status props="none" item="unversioned"></wc-status>
</entry>
<entry path="D:\p\Content\VH_Mothership_3.uasset">
<wc-status item="modified" revision="722" props="normal">
<commit revision="722"><author>someone</author></commit>
<lock><token>opaquelocktoken:1</token></lock>
</wc-status>
</entry>
<entry path="Source\New &amp; Shiny.cpp">
<wc-status props="none" item="added" revision="-1"/>
</entry>
<entry path="D:\p\Source\Ext">
<wc-status props="none" item="external"></wc-status>
</entry>
<entry path="D:\p\Intermediate">
<wc-status props="none" item="ignored"></wc-status>
</entry>
</target>
</status>"#;

    #[test]
    fn parses_info_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<info>
<entry path="." revision="712" kind="dir">
<url>https://host/svn/game/trunk</url>
<relative-url>^/trunk</relative-url>
<repository><root>https://host/svn/game</root><uuid>x</uuid></repository>
<wc-info><wcroot-abspath>D:/dev/repos/game</wcroot-abspath></wc-info>
<commit revision="710"><author>cashworth</author><date>2026-10-06T11:26:47.183781Z</date></commit>
</entry>
</info>"#;
        let info = parse_info(xml).unwrap();
        assert_eq!(info.url, "https://host/svn/game/trunk");
        assert_eq!(info.branch.as_deref(), Some("^/trunk"));
        assert_eq!(info.revision.as_deref(), Some("712"));
        let last = info.last_change.unwrap();
        assert_eq!(
            (last.revision.as_str(), last.author.as_str()),
            ("710", "cashworth")
        );
        assert!(last.date.starts_with("2026-10-06"));
    }

    #[test]
    fn parses_status_xml() {
        let files = parse_status(STATUS, Path::new(r"D:\p")).unwrap();
        let got: Vec<_> = files
            .iter()
            .map(|f| (f.path.display().to_string(), f.kind))
            .collect();
        assert_eq!(
            got,
            [
                (
                    r"D:\p\.claude\scheduled_tasks.lock".to_string(),
                    ChangeKind::Unversioned
                ),
                (
                    r"D:\p\Content\VH_Mothership_3.uasset".to_string(),
                    ChangeKind::Modified
                ),
                (
                    r"D:\p\Source\New & Shiny.cpp".to_string(),
                    ChangeKind::Added
                ),
                (r"D:\p\Intermediate".to_string(), ChangeKind::Ignored),
            ]
        );
    }

    /// End to end against a scratch repository, when svn and svnadmin exist.
    #[test]
    fn status_and_base_against_a_real_repository() {
        let svn = Subversion::new();
        let svnadmin_ok = Command::new("svnadmin")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !svn.is_available() || !svnadmin_ok {
            eprintln!("svn/svnadmin not found; skipping");
            return;
        }

        let base = std::env::temp_dir().join(format!("tethys-svn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wc = base.join("wc");
        std::fs::create_dir_all(&base).unwrap();
        let run = |program: &str, args: &[&str]| {
            let out = Command::new(program).args(args).output().unwrap();
            assert!(out.status.success(), "{program} {args:?}: {out:?}");
        };
        run("svnadmin", &["create", repo.to_str().unwrap()]);
        let url = format!("file:///{}", repo.display().to_string().replace('\\', "/"));
        run("svn", &["checkout", "-q", &url, wc.to_str().unwrap()]);
        std::fs::write(wc.join("a.txt"), "one\ntwo\n").unwrap();
        run("svn", &["add", "-q", wc.join("a.txt").to_str().unwrap()]);
        run("svn", &["commit", "-q", "-m", "init", wc.to_str().unwrap()]);
        std::fs::write(wc.join("a.txt"), "one\n2\n").unwrap();
        std::fs::write(wc.join("new.txt"), "x\n").unwrap();

        assert!(svn.is_working_copy(&wc.join("sub")));
        let info = svn.info(&wc).unwrap();
        // Committing doesn't bump the folder's own revision until `svn update`.
        assert_eq!(info.revision.as_deref(), Some("0"));
        assert!(info.url.starts_with("file:"));
        assert!(!svn.is_working_copy(&base));

        let mut status = svn.status(&wc).unwrap();
        status.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(status.len(), 2);
        assert_eq!(status[0].kind, ChangeKind::Modified);
        assert!(status[0].path.ends_with("a.txt"));
        assert_eq!(status[1].kind, ChangeKind::Unversioned);

        assert_eq!(
            svn.base_text(&wc.join("a.txt")).unwrap().as_deref(),
            Some("one\ntwo\n")
        );
        assert_eq!(svn.base_text(&wc.join("new.txt")).unwrap(), None);
        std::fs::write(wc.join("added.txt"), "y\n").unwrap();
        run(
            "svn",
            &["add", "-q", wc.join("added.txt").to_str().unwrap()],
        );
        assert_eq!(svn.base_text(&wc.join("added.txt")).unwrap(), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_log_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<log>
<logentry revision="12">
<author>chris</author>
<date>2026-10-13T09:00:00.000000Z</date>
<msg>Fix &amp; tidy
second line</msg>
</logentry>
<logentry revision="3">
<author>sam</author>
<date>2026-10-01T09:00:00.000000Z</date>
<msg></msg>
</logentry>
</log>"#;
        let log = parse_log(xml).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].revision, "12");
        assert_eq!(log[0].author, "chris");
        assert_eq!(log[0].message, "Fix & tidy\nsecond line");
        assert_eq!(log[1].revision, "3");
        assert_eq!(log[1].message, "");
    }

    /// Rename, history, a commit's change and delete, against a scratch
    /// repository when svn and svnadmin exist.
    #[test]
    fn edits_and_history_against_a_real_repository() {
        let svn = Subversion::new();
        let svnadmin_ok = Command::new("svnadmin")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !svn.is_available() || !svnadmin_ok {
            eprintln!("svn/svnadmin not found; skipping");
            return;
        }

        let base = std::env::temp_dir().join(format!("tethys-svn-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        let wc = base.join("wc");
        std::fs::create_dir_all(&base).unwrap();
        let run = |program: &str, args: &[&str]| {
            let out = Command::new(program).args(args).output().unwrap();
            assert!(out.status.success(), "{program} {args:?}: {out:?}");
        };
        let wc_arg = wc.to_str().unwrap();
        run("svnadmin", &["create", repo.to_str().unwrap()]);
        let url = format!("file:///{}", repo.display().to_string().replace('\\', "/"));
        run("svn", &["checkout", "-q", &url, wc_arg]);
        std::fs::write(wc.join("a.txt"), "one\n").unwrap();
        run("svn", &["add", "-q", wc.join("a.txt").to_str().unwrap()]);
        run("svn", &["commit", "-q", "-m", "add a", wc_arg]);
        std::fs::write(wc.join("a.txt"), "one\ntwo\n").unwrap();
        run("svn", &["commit", "-q", "-m", "grow a", wc_arg]);

        std::fs::write(wc.join("loose.txt"), "x\n").unwrap();
        assert!(svn.is_versioned(&wc.join("a.txt")).unwrap());
        assert!(!svn.is_versioned(&wc.join("loose.txt")).unwrap());

        // Renamed through svn, so history follows it.
        svn.rename(&wc.join("a.txt"), &wc.join("b.txt")).unwrap();
        run("svn", &["commit", "-q", "-m", "rename", wc_arg]);
        run("svn", &["update", "-q", wc_arg]);
        let b = wc.join("b.txt");
        let log = svn.history(&b, 10).unwrap();
        let revisions: Vec<_> = log.iter().map(|c| c.revision.as_str()).collect();
        assert_eq!(revisions, ["3", "2", "1"]);
        assert_eq!(log[1].message, "grow a");

        assert_eq!(
            svn.change(&b, &log[1]).unwrap(),
            (Some("one\n".into()), Some("one\ntwo\n".into()))
        );
        // Added in r1: nothing before.
        assert_eq!(
            svn.change(&b, &log[2]).unwrap(),
            (None, Some("one\n".into()))
        );

        std::fs::write(&b, "edited\n").unwrap();
        svn.delete(&b).unwrap();
        assert!(!b.exists(), "deleted from disk, local edit and all");
        let status = svn.status(&wc).unwrap();
        assert!(
            status
                .iter()
                .any(|f| f.path.ends_with("b.txt") && f.kind == ChangeKind::Deleted),
            "{status:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
