//! `SourceControl` for Subversion, through the `svn` command-line client.
//!
//! Read-only: `svn status --xml` and `svn cat -r BASE` only. `-r BASE` reads
//! the working copy's pristine copy, so neither command touches the network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use tethys_core::ports::{PortResult, SourceControl};
use tethys_core::scm::{ChangeKind, FileStatus};

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

    fn is_working_copy(&self, dir: &Path) -> bool {
        dir.ancestors().any(|d| d.join(".svn").is_dir())
    }

    fn status(&self, root: &Path) -> PortResult<Vec<FileStatus>> {
        let target = svn_path(root);
        let out = self.run(&["status", "--xml", &target])?;
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
        // normal, ignored, external, incomplete, obstructed, none
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
</target>
</status>"#;

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
}
