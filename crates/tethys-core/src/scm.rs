//! Source control: what changed in a working copy, independent of the tool.

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

    /// Whether a diff against the repository makes sense.
    pub fn has_base(self) -> bool {
        matches!(
            self,
            ChangeKind::Modified | ChangeKind::Replaced | ChangeKind::Conflicted
        )
    }
}

/// One changed file. `path` is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatus {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// The changed files of a working copy, for quick lookup by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingCopyStatus {
    files: Vec<FileStatus>,
}

impl WorkingCopyStatus {
    pub fn new(mut files: Vec<FileStatus>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self { files }
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
        self.files
            .iter()
            .find(|f| same_path(&f.path, path))
            .map(|f| f.kind)
    }

    /// Whether anything under directory `dir` changed (for marking folders).
    pub fn contains_changes_under(&self, dir: &Path) -> bool {
        self.files
            .iter()
            .any(|f| f.kind != ChangeKind::Unversioned && starts_with_ci(&f.path, dir))
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
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
}
