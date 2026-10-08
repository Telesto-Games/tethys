//! Line diffs: what changed between two versions of a text file.

use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One line of a unified diff. Line numbers are 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    /// The text without its line ending.
    pub text: String,
}

/// A run of changes with surrounding context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based first line in the old and new file.
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileDiff {
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
}

impl FileDiff {
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }
}

/// Diffs `old` against `new` by line, keeping `context` unchanged lines around
/// each change. Line endings (`\n` or `\r\n`) are ignored.
pub fn diff_lines(old: &str, new: &str, context: usize) -> FileDiff {
    let old = normalize(old);
    let new = normalize(new);
    let diff = TextDiff::from_lines(&old, &new);

    let mut result = FileDiff::default();
    for group in diff.grouped_ops(context) {
        let (Some(first), Some(_)) = (group.first(), group.last()) else {
            continue;
        };
        let mut hunk = Hunk {
            old_start: first.old_range().start + 1,
            new_start: first.new_range().start + 1,
            lines: Vec::new(),
        };
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Insert => {
                        result.added += 1;
                        LineKind::Added
                    }
                    ChangeTag::Delete => {
                        result.removed += 1;
                        LineKind::Removed
                    }
                };
                hunk.lines.push(DiffLine {
                    kind,
                    old_line: change.old_index().map(|i| i + 1),
                    new_line: change.new_index().map(|i| i + 1),
                    text: change.value().trim_end_matches('\n').to_string(),
                });
            }
        }
        result.hunks.push(hunk);
    }
    result
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_hunks() {
        assert!(diff_lines("a\nb\n", "a\r\nb\r\n", 3).is_empty());
    }

    #[test]
    fn reports_changes_with_context_and_line_numbers() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n";
        let new = "1\n2\n3\nfour\n5\n6\n7\n8\nnine\n";
        let d = diff_lines(old, new, 1);
        assert_eq!((d.added, d.removed), (2, 1));
        assert_eq!(d.hunks.len(), 2);

        let first = &d.hunks[0];
        assert_eq!((first.old_start, first.new_start), (3, 3));
        let kinds: Vec<_> = first.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Context,
                LineKind::Removed,
                LineKind::Added,
                LineKind::Context
            ]
        );
        assert_eq!(first.lines[1].text, "4");
        assert_eq!(first.lines[1].old_line, Some(4));
        assert_eq!(first.lines[2].new_line, Some(4));

        let last = d.hunks[1].lines.last().unwrap();
        assert_eq!((last.kind, last.new_line), (LineKind::Added, Some(9)));
    }
}
