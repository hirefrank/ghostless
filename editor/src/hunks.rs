//! Line hunks between a session baseline and the working draft.
//!
//! Ranges stay on UTF-8 boundaries because they follow newline splits.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextHunk {
    pub before_bytes: Range<usize>,
    pub after_bytes: Range<usize>,
}

pub(crate) fn diff_hunks(before: &str, after: &str) -> Vec<TextHunk> {
    let before_lines = split_lines(before);
    let after_lines = split_lines(after);
    let ops = if before_lines.len().saturating_mul(after_lines.len()) > 640_000 {
        coarse_ops(before, after, &before_lines, &after_lines)
    } else {
        lcs_ops(before, after, &before_lines, &after_lines)
    };
    group_hunks(before, after, &before_lines, &after_lines, &ops)
}

pub(crate) fn revert_hunk_text(before: &str, after: &str, index: usize) -> Option<String> {
    let hunk = diff_hunks(before, after).get(index)?.clone();
    let replacement = before.get(hunk.before_bytes)?;
    let mut next = after.to_owned();
    next.replace_range(hunk.after_bytes, replacement);
    Some(next)
}

fn split_lines(text: &str) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            lines.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < text.len() {
        lines.push((start, text.len()));
    }
    lines
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

fn lcs_ops(
    before_text: &str,
    after_text: &str,
    before: &[(usize, usize)],
    after: &[(usize, usize)],
) -> Vec<Op> {
    let n = before.len();
    let m = after.len();
    let mut scores = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            scores[at(i, j)] = if line_eq(before_text, after_text, before, after, i, j) {
                scores[at(i + 1, j + 1)] + 1
            } else {
                scores[at(i + 1, j)].max(scores[at(i, j + 1)])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if line_eq(before_text, after_text, before, after, i, j) {
            ops.push(Op::Equal);
            i += 1;
            j += 1;
        } else if scores[at(i + 1, j)] >= scores[at(i, j + 1)] {
            ops.push(Op::Delete);
            i += 1;
        } else {
            ops.push(Op::Insert);
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete);
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert);
        j += 1;
    }
    ops
}

fn coarse_ops(
    before_text: &str,
    after_text: &str,
    before: &[(usize, usize)],
    after: &[(usize, usize)],
) -> Vec<Op> {
    let shared = before.len().min(after.len());
    let mut ops = Vec::new();
    let mut prefix = 0;
    while prefix < shared && line_eq(before_text, after_text, before, after, prefix, prefix) {
        ops.push(Op::Equal);
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < shared - prefix
        && line_eq(
            before_text,
            after_text,
            before,
            after,
            before.len() - 1 - suffix,
            after.len() - 1 - suffix,
        )
    {
        suffix += 1;
    }
    for _ in prefix..(before.len() - suffix) {
        ops.push(Op::Delete);
    }
    for _ in prefix..(after.len() - suffix) {
        ops.push(Op::Insert);
    }
    for _ in 0..suffix {
        ops.push(Op::Equal);
    }
    ops
}

fn line_eq(
    before_text: &str,
    after_text: &str,
    before: &[(usize, usize)],
    after: &[(usize, usize)],
    i: usize,
    j: usize,
) -> bool {
    before_text.get(before[i].0..before[i].1) == after_text.get(after[j].0..after[j].1)
}

fn group_hunks(
    before_text: &str,
    after_text: &str,
    before_lines: &[(usize, usize)],
    after_lines: &[(usize, usize)],
    ops: &[Op],
) -> Vec<TextHunk> {
    let mut hunks = Vec::new();
    let (mut before_index, mut after_index) = (0, 0);
    let mut cursor = 0;
    while cursor < ops.len() {
        if ops[cursor] == Op::Equal {
            before_index += 1;
            after_index += 1;
            cursor += 1;
            continue;
        }
        let before_start = before_index;
        let after_start = after_index;
        while cursor < ops.len() && ops[cursor] != Op::Equal {
            match ops[cursor] {
                Op::Delete => before_index += 1,
                Op::Insert => after_index += 1,
                Op::Equal => {}
            }
            cursor += 1;
        }
        let before_bytes = span(before_lines, before_start, before_index, before_text.len());
        let after_bytes = span(after_lines, after_start, after_index, after_text.len());
        if before_text.get(before_bytes.clone()) != after_text.get(after_bytes.clone()) {
            hunks.push(TextHunk {
                before_bytes,
                after_bytes,
            });
        }
    }
    hunks
}

fn span(lines: &[(usize, usize)], start: usize, end: usize, len: usize) -> Range<usize> {
    let from = lines.get(start).map(|line| line.0).unwrap_or(len);
    let to = if end == 0 {
        from
    } else {
        lines.get(end - 1).map(|line| line.1).unwrap_or(len)
    };
    from..to
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverting_the_middle_hunk_leaves_the_others() {
        let before = "Alpha paragraph.\n\nMiddle paragraph.\n\nOmega paragraph.\n";
        let after = "Alpha changed.\n\nMiddle edited.\n\nOmega changed.\n";
        let hunks = diff_hunks(before, after);
        assert_eq!(hunks.len(), 3, "{hunks:?}");
        let reverted = revert_hunk_text(before, after, 1).unwrap();
        assert_eq!(
            reverted,
            "Alpha changed.\n\nMiddle paragraph.\n\nOmega changed.\n"
        );
        assert_eq!(diff_hunks(before, &reverted).len(), 2);
    }

    #[test]
    fn hunks_keep_emoji_and_emdash_intact_and_full_revert_matches() {
        let before = "Café — draft.\n\nSecond line ✨.\n";
        let after = "Café — revision.\n\nSecond line 🌟.\n";
        let hunks = diff_hunks(before, after);
        assert_eq!(hunks.len(), 2);
        let mut current = after.to_owned();
        for index in (0..hunks.len()).rev() {
            current = revert_hunk_text(before, &current, index).unwrap();
        }
        assert_eq!(current, before);
        assert!(diff_hunks(before, before).is_empty());
    }
}
