use std::ops::Range;

use rune::{TextEdit, Tx};

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
    if query.is_empty() {
        return Vec::new();
    }
    let needle = if case_sensitive {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    let mut result = Vec::new();
    let mut next_start = 0;
    for (start, _) in text.char_indices() {
        if start < next_start {
            continue;
        }
        let mut folded = String::new();
        for (offset, c) in text[start..].char_indices() {
            if case_sensitive {
                folded.push(c);
            } else {
                folded.extend(c.to_lowercase());
            }
            if !needle.starts_with(&folded) {
                break;
            }
            if folded == needle {
                let end = start + offset + c.len_utf8();
                let boundary = !whole_word
                    || (!text[..start].chars().next_back().is_some_and(word_char)
                        && !text[end..].chars().next().is_some_and(word_char));
                if boundary {
                    result.push(start..end);
                    next_start = end;
                }
                break;
            }
        }
    }
    result
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
    if !valid_range(&edit.text, &range) {
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
        for range in ranges.iter().rev() {
            replace_one(edit, range.clone(), replacement);
        }
    });
    ranges.len()
}

pub(crate) fn open_find_with_query(find: &mut FindState, query: &str, text: &str, tx: &mut Tx) {
    find.open = true;
    find.query.replace_text(query);
    find.matches = find_matches(text, query, find.case_sensitive, find.whole_word);
    find.current = (!find.matches.is_empty()).then_some(0);
    tx.focus("find-query", false);
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
