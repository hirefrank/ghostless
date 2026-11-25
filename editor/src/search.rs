use std::ops::Range;

use rune::TextEdit;

pub(crate) const SOURCE_MATCH_KEY: &str = "find-source-match";

#[derive(Clone, Debug, Default)]
pub(crate) struct FindState {
    pub open: bool,
    pub show_replace: bool,
    pub query: TextEdit,
    pub replacement: TextEdit,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub matches: Vec<Range<usize>>,
    pub current: Option<usize>,
    pub notice: Option<String>,
    source: String,
    resume_at: Option<usize>,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

pub(crate) fn find_matches(
    text: &str,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Vec<Range<usize>> {
    match_ranges(text, query, case_sensitive, whole_word, || false).collect()
}

pub(crate) fn match_ranges<'a>(
    mut text: &'a str,
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    cancelled: impl Fn() -> bool + 'a,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let needle = if case_sensitive {
        query.to_owned()
    } else {
        query.chars().flat_map(char::to_lowercase).collect()
    };
    if needle.is_empty() {
        text = "";
    }
    let mut next_start = 0;
    text.char_indices()
        .enumerate()
        // A long paragraph may have no hits to yield back to the workspace scanner.
        .take_while(move |(index, _)| index % 1024 != 0 || !cancelled())
        .filter_map(move |(_, (start, _))| {
            if start < next_start
                || (whole_word && text[..start].chars().next_back().is_some_and(word_char))
            {
                return None;
            }
            let tail = &text[start..];
            let len = if case_sensitive {
                tail.starts_with(&needle).then_some(needle.len())?
            } else {
                folded_prefix_len(tail, &needle)?
            };
            let end = start + len;
            if whole_word && text[end..].chars().next().is_some_and(word_char) {
                return None;
            }
            next_start = end;
            Some(start..end)
        })
}

fn folded_prefix_len(text: &str, mut needle: &str) -> Option<usize> {
    for (offset, character) in text.char_indices() {
        for lower in character.to_lowercase() {
            needle = needle.strip_prefix(lower)?;
        }
        if needle.is_empty() {
            return Some(offset + character.len_utf8());
        }
    }
    None
}

impl FindState {
    pub(crate) fn current_range(&self) -> Option<&Range<usize>> {
        self.current.and_then(|index| self.matches.get(index))
    }

    pub(crate) fn navigation_anchor(&self) -> Option<usize> {
        self.current_range()
            .map(|range| range.start)
            .or(self.resume_at)
    }

    pub(crate) fn refresh(&mut self, text: &str, anchor: usize) {
        let matches = find_matches(text, &self.query.text, self.case_sensitive, self.whole_word);
        self.set_matches(text, matches, None);
        self.select_at_or_after(anchor);
    }

    pub(crate) fn set_matches(
        &mut self,
        text: &str,
        matches: Vec<Range<usize>>,
        current: Option<usize>,
    ) {
        self.matches = matches;
        self.current = current;
        self.resume_at = None;
        self.notice = None;
        if self.source != text {
            self.source.clear();
            self.source.push_str(text);
        }
    }

    pub(crate) fn document_changed(&mut self, text: &str, caret: usize) {
        if self.source == text {
            return;
        }
        let previous = self
            .current_range()
            .cloned()
            .or_else(|| self.resume_at.map(|offset| offset..offset));
        let Some(previous) = previous else {
            self.refresh(text, caret);
            return;
        };

        let anchor = remap_match_start(&self.source, text, previous);
        let matches = find_matches(text, &self.query.text, self.case_sensitive, self.whole_word);
        let current = matches
            .binary_search_by_key(&anchor, |range| range.start)
            .ok();
        self.set_matches(text, matches, current);
        // If the edit broke the match, keep its place for Next, Previous, and
        // undo. Only an explicit navigation command moves to another occurrence.
        if current.is_none() {
            self.resume_at = Some(anchor);
        }
    }

    fn select_at_or_after(&mut self, anchor: usize) {
        let index = self.matches.partition_point(|range| range.end <= anchor);
        self.current = (!self.matches.is_empty()).then_some(if index == self.matches.len() {
            0
        } else {
            index
        });
    }

    pub(crate) fn select_after_replacement(&mut self, replaced: Range<usize>) {
        let next = self
            .matches
            .partition_point(|range| range.start < replaced.end);
        self.current = if next < self.matches.len() {
            Some(next)
        } else {
            self.matches
                .first()
                .filter(|range| range.end <= replaced.start)
                .map(|_| 0)
        };
        self.resume_at = self.current.is_none().then_some(replaced.end);
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.current = None;
        self.resume_at = None;
        self.notice = None;
        self.source = String::new();
    }
}

fn remap_match_start(before: &str, after: &str, range: Range<usize>) -> usize {
    let prefix = common_bytes(before.bytes(), after.bytes());
    if range.end <= prefix {
        return range.start;
    }
    let suffix = common_bytes(
        before.as_bytes()[prefix..].iter().rev().copied(),
        after.as_bytes()[prefix..].iter().rev().copied(),
    );
    if range.start >= before.len() - suffix {
        after.len() - (before.len() - range.start)
    } else {
        range.start.min(prefix)
    }
}

fn common_bytes(a: impl Iterator<Item = u8>, b: impl Iterator<Item = u8>) -> usize {
    a.zip(b).take_while(|(a, b)| a == b).count()
}

pub(crate) fn next_match(
    matches: &[Range<usize>],
    cursor_byte: usize,
    forward: bool,
) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    if forward {
        Some(
            matches
                .iter()
                .position(|r| r.start > cursor_byte)
                .unwrap_or(0),
        )
    } else {
        Some(
            matches
                .iter()
                .rposition(|r| r.start < cursor_byte)
                .unwrap_or(matches.len() - 1),
        )
    }
}

fn valid_range(text: &str, range: &Range<usize>) -> bool {
    range.start < range.end
        && range.end <= text.len()
        && text.is_char_boundary(range.start)
        && text.is_char_boundary(range.end)
}

pub(crate) fn replace_one(edit: &mut TextEdit, range: Range<usize>, replacement: &str) -> bool {
    if !valid_range(&edit.text, &range) || edit.text.get(range.clone()) == Some(replacement) {
        return false;
    }
    edit.set_selection(range.start, range.end);
    edit.insert(replacement);
    true
}

pub(crate) fn replace_all(
    edit: &mut TextEdit,
    matches: &[Range<usize>],
    replacement: &str,
) -> usize {
    let mut ranges: Vec<_> = matches
        .iter()
        .filter(|r| valid_range(&edit.text, r))
        .cloned()
        .collect();
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut end = 0;
    ranges.retain(|r| {
        let keep = r.start >= end;
        if keep {
            end = r.end;
        }
        keep
    });
    edit.undo_group(|edit| {
        let mut changed = 0;
        for range in ranges.into_iter().rev() {
            changed += usize::from(replace_one(edit, range, replacement));
        }
        changed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_word_boundaries() {
        let text = "Café — “smart quotes” こんにちは ✨ café DELVE delved delve's delve";
        for query in ["café", "—", "“smart quotes”", "こんにちは", "✨"] {
            let ranges = find_matches(text, query, false, false);
            assert!(!ranges.is_empty());
            assert!(
                ranges
                    .iter()
                    .all(|r| text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
            );
        }
        assert_eq!(find_matches(text, "café", false, false).len(), 2);
        assert_eq!(find_matches(text, "delve", false, true).len(), 2);
        assert_eq!(find_matches("İ", "i\u{307}", false, false), vec![0..2]);
    }

    #[test]
    fn sparse_search_checks_cancellation_inside_a_long_line() {
        use std::cell::Cell;

        let text = format!("{}needle", "x".repeat(4096));
        let checks = Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() >= 2
        };
        assert_eq!(
            match_ranges(&text, "needle", false, false, cancelled).next(),
            None
        );
        assert_eq!(checks.get(), 2);
    }

    #[test]
    fn replacements_undo_in_one_step() {
        let mut edit = TextEdit::default();
        edit.replace_text("Café café ✨");
        assert!(replace_one(&mut edit, 0..5, "Tea"));
        assert_eq!(edit.text, "Tea café ✨");
        edit.undo();
        assert_eq!(edit.text, "Café café ✨");
        let ranges = find_matches(&edit.text, "café", false, false);
        assert_eq!(replace_all(&mut edit, &ranges, "Tea"), 2);
        assert_eq!(edit.text, "Tea Tea ✨");
        edit.undo();
        assert_eq!(edit.text, "Café café ✨");
    }
}
