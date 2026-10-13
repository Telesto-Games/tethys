//! Source control: what changed in a working copy, independent of the tool.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How a file differs from the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    /// Deleted from disk without telling source control.
    Missing,
    Replaced,
    Conflicted,
    /// Not under source control.
    Unversioned,
    /// Not under source control, and matching its ignore rules.
    Ignored,
}

impl ChangeKind {
    /// The one-letter code shown next to a file (SVN's letters).
    pub fn letter(self) -> char {
        match self {
            ChangeKind::Modified => 'M',
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Missing => '!',
            ChangeKind::Replaced => 'R',
            ChangeKind::Conflicted => 'C',
            ChangeKind::Unversioned => '?',
            ChangeKind::Ignored => 'I',
        }
    }

    /// A local edit to a file that exists: added or modified (including
    /// replaced and conflicted). Not deletions or unversioned files.
    pub fn is_local_edit(self) -> bool {
        matches!(
            self,
            ChangeKind::Modified
                | ChangeKind::Added
                | ChangeKind::Replaced
                | ChangeKind::Conflicted
        )
    }

    /// Whether source control tracks the file: not unversioned or ignored.
    pub fn is_versioned(self) -> bool {
        !matches!(self, ChangeKind::Unversioned | ChangeKind::Ignored)
    }

    /// Whether a diff against the repository makes sense.
    pub fn has_base(self) -> bool {
        matches!(
            self,
            ChangeKind::Modified | ChangeKind::Replaced | ChangeKind::Conflicted
        )
    }
}

/// Where a working copy points, for display. All read locally.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingCopyInfo {
    /// The checked-out location, e.g. `https://host/svn/game/trunk`.
    pub url: String,
    /// The location within the repository, e.g. `^/trunk` (a branch, for SVN).
    pub branch: Option<String>,
    /// The working copy's revision, e.g. `712`.
    pub revision: Option<String>,
    /// The last commit to this location: revision, author and ISO date.
    pub last_change: Option<LastChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastChange {
    pub revision: String,
    pub author: String,
    pub date: String,
}

/// One commit in a file's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// `712` for Subversion, the full hash for git.
    pub revision: String,
    pub author: String,
    /// ISO 8601.
    pub date: String,
    pub message: String,
    /// Where the file was in this commit, in the source control's own terms,
    /// for [`crate::ports::SourceControl::change`]. Empty when it doesn't need one.
    pub locator: String,
}

/// One changed file. `path` is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// The changed files of a working copy, for quick lookup by path.
///
/// The file tree asks about every visible row on every frame, and a status
/// with ignored files can run to thousands of entries, so lookups go
/// through a map keyed by case-folded path rather than scanning the list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingCopyStatus {
    files: Vec<FileStatus>,
    /// Every entry by [`path_key`]. A path listed twice (a rename's old and
    /// new paths) keeps the first in path order.
    by_path: HashMap<String, ChangeKind>,
    /// Entries that aren't versioned (files, or folders named once).
    untracked: usize,
}

impl WorkingCopyStatus {
    pub fn new(mut files: Vec<FileStatus>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut by_path = HashMap::with_capacity(files.len());
        for f in &files {
            by_path.entry(path_key(&f.path)).or_insert(f.kind);
        }
        let untracked = files.iter().filter(|f| !f.kind.is_versioned()).count();
        Self {
            files,
            by_path,
            untracked,
        }
    }

    pub fn files(&self) -> &[FileStatus] {
        &self.files
    }

    /// Added and modified files, sorted by path.
    pub fn local_edits(&self) -> impl Iterator<Item = &FileStatus> {
        self.files.iter().filter(|f| f.kind.is_local_edit())
    }

    /// The change for exactly this file, if any.
    pub fn of(&self, path: &Path) -> Option<ChangeKind> {
        self.by_path.get(&path_key(path)).copied()
    }

    /// Whether source control tracks `path`. Status names an untracked or
    /// ignored folder once rather than everything in it, so this looks for
    /// `path` or any folder above it, nearest the root first. Tracked unless
    /// status says otherwise.
    pub fn tracking(&self, path: &Path) -> Tracking {
        if self.untracked == 0 {
            return Tracking::Tracked;
        }
        let mut key = String::new();
        for component in path.components() {
            push_component(&mut key, component.as_os_str());
            match self.by_path.get(&key) {
                Some(ChangeKind::Ignored) => return Tracking::Ignored,
                Some(ChangeKind::Unversioned) => return Tracking::Untracked,
                _ => {}
            }
        }
        Tracking::Tracked
    }

    /// Whether anything under directory `dir` changed (for marking folders).
    pub fn contains_changes_under(&self, dir: &Path) -> bool {
        self.files
            .iter()
            .any(|f| f.kind.is_versioned() && starts_with_ci(&f.path, dir))
    }
}

/// A path as a map key: its components, case-folded, joined by a character
/// no path contains, so `D:\p\Sub` and `d:/P/sub/` read the same.
fn path_key(path: &Path) -> String {
    let mut key = String::new();
    for component in path.components() {
        push_component(&mut key, component.as_os_str());
    }
    key
}

fn push_component(key: &mut String, component: &std::ffi::OsStr) {
    if !key.is_empty() {
        key.push('\0');
    }
    for c in component.to_string_lossy().chars() {
        key.extend(c.to_lowercase());
    }
}

/// Whether source control tracks a file or folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    Tracked,
    /// Not under source control.
    Untracked,
    /// Not under source control, and matching its ignore rules.
    Ignored,
}

/// Case-insensitive `Path::starts_with`, since Windows paths are.
fn starts_with_ci(path: &Path, prefix: &Path) -> bool {
    let mut p = path.components();
    prefix.components().all(|c| {
        p.next()
            .is_some_and(|pc| pc.as_os_str().eq_ignore_ascii_case(c.as_os_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> WorkingCopyStatus {
        WorkingCopyStatus::new(vec![
            FileStatus {
                path: PathBuf::from(r"D:\p\Source\Game\A.cpp"),
                kind: ChangeKind::Modified,
            },
            FileStatus {
                path: PathBuf::from(r"D:\p\Saved\x.log"),
                kind: ChangeKind::Unversioned,
            },
        ])
    }

    #[test]
    fn looks_up_files_case_insensitively() {
        let s = status();
        assert_eq!(
            s.of(Path::new(r"d:\P\source\game\a.cpp")),
            Some(ChangeKind::Modified)
        );
        assert_eq!(s.of(Path::new(r"D:\p\Source\Game\B.cpp")), None);
    }

    #[test]
    fn local_edits_skip_unversioned_and_deleted() {
        let mut files = status().files().to_vec();
        files.push(FileStatus {
            path: PathBuf::from(r"D:\p\Gone.cpp"),
            kind: ChangeKind::Deleted,
        });
        files.push(FileStatus {
            path: PathBuf::from(r"D:\p\New.cpp"),
            kind: ChangeKind::Added,
        });
        let s = WorkingCopyStatus::new(files);
        let edits: Vec<_> = s.local_edits().map(|f| f.kind).collect();
        assert_eq!(edits, [ChangeKind::Added, ChangeKind::Modified]);
    }

    #[test]
    fn marks_folders_with_versioned_changes() {
        let s = status();
        assert!(s.contains_changes_under(Path::new(r"D:\p\Source")));
        assert!(!s.contains_changes_under(Path::new(r"D:\p\Saved")));
        assert!(!s.contains_changes_under(Path::new(r"D:\p\Sour")));
    }

    #[test]
    fn tracking_comes_from_the_nearest_untracked_or_ignored_folder() {
        let s = WorkingCopyStatus::new(vec![
            FileStatus {
                path: PathBuf::from(r"D:\p\Source\Game\A.cpp"),
                kind: ChangeKind::Modified,
            },
            FileStatus {
                path: PathBuf::from(r"D:\p\Scratch"),
                kind: ChangeKind::Unversioned,
            },
            FileStatus {
                path: PathBuf::from(r"D:\p\Intermediate"),
                kind: ChangeKind::Ignored,
            },
        ]);
        let tracking = |p: &str| s.tracking(Path::new(p));
        assert_eq!(tracking(r"D:\p\Source\Game\A.cpp"), Tracking::Tracked);
        assert_eq!(tracking(r"D:\p\Source\Game\B.cpp"), Tracking::Tracked);
        assert_eq!(tracking(r"D:\p\Scratch"), Tracking::Untracked);
        assert_eq!(tracking(r"d:\P\scratch\deep\x.txt"), Tracking::Untracked);
        assert_eq!(
            tracking(r"D:\p\Intermediate\Build\x.obj"),
            Tracking::Ignored
        );
        assert_eq!(tracking(r"D:\p\IntermediateNot"), Tracking::Tracked);
        // A folder's own entry answers for it however it's spelled.
        assert_eq!(tracking(r"D:/p/scratch/"), Tracking::Untracked);
        assert_eq!(
            s.of(Path::new(r"d:/P/INTERMEDIATE")),
            Some(ChangeKind::Ignored)
        );
        // Nothing tracked changed under an ignored folder.
        assert!(!s.contains_changes_under(Path::new(r"D:\p\Intermediate")));
    }
}
