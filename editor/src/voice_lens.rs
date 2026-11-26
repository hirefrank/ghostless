//! Inline underlines for the latest voice report.
//!
//! Marks are derived from the same non-prose ranges as placeholders, so a tell
//! inside YAML, a fence, or inline code stays unmarked.

use std::ops::Range;

use rune::Span;
use rune_kit::Kit;

use crate::{bridge::JudgeReport, markdown_decor};

pub(crate) fn spans(text: &str, report: &JudgeReport, kit: &Kit) -> Vec<Span> {
    let excluded = markdown_decor::non_prose_ranges(text);
    let mut marks = Vec::new();
    for hit in &report.banned {
        for range in word_ranges(text, &hit.tell, &excluded) {
            marks.push(Span::new(range.start, range.end).underline(kit.c.danger.fg));
        }
    }
    for hit in &report.confirmed_rules {
        for range in word_ranges(text, &hit.word, &excluded) {
            marks.push(Span::new(range.start, range.end).underline(kit.c.warning.fg));
        }
    }
    for hit in &report.structure {
        if hit.kind == "em-dash" || hit.excerpt.is_empty() {
            continue;
        }
        for range in excerpt_ranges(text, &hit.excerpt, &excluded) {
            marks.push(Span::new(range.start, range.end).underline(kit.c.warning.fg));
        }
    }
    marks
}

fn overlaps(excluded: &[Range<usize>], range: &Range<usize>) -> bool {
    excluded
        .iter()
        .any(|other| other.start < range.end && other.end > range.start)
}

fn word_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
}

fn word_ranges(text: &str, word: &str, excluded: &[Range<usize>]) -> Vec<Range<usize>> {
    let needle: Vec<char> = word.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index + needle.len() <= chars.len() {
        let matched = needle
            .iter()
            .zip(chars[index..].iter())
            .all(|(want, (_, got))| got.to_lowercase().eq(want.to_lowercase()));
        if matched {
            let start = chars[index].0;
            let end_index = index + needle.len();
            let end = chars
                .get(end_index)
                .map(|(offset, _)| *offset)
                .unwrap_or(text.len());
            let range = start..end;
            let before = index.checked_sub(1).map(|at| chars[at].1);
            let after = chars.get(end_index).map(|(_, ch)| *ch);
            if word_boundary(before) && word_boundary(after) && !overlaps(excluded, &range) {
                out.push(range);
            }
            index += needle.len();
        } else {
            index += 1;
        }
    }
    out
}

fn excerpt_ranges(text: &str, excerpt: &str, excluded: &[Range<usize>]) -> Vec<Range<usize>> {
    if excerpt.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = text[pos..].find(excerpt) {
        let start = pos + rel;
        let end = start + excerpt.len();
        let range = start..end;
        if !overlaps(excluded, &range) {
            out.push(range);
        }
        pos = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{BannedHit, ConfirmedRuleHit, JudgeReport, StructureHit};

    fn report() -> JudgeReport {
        JudgeReport {
            banned: vec![BannedHit {
                tell: "delve".into(),
                count: 1,
            }],
            confirmed_rules: vec![ConfirmedRuleHit {
                id: "cut-1".into(),
                word: "elevate".into(),
                scope: Some("confirmed".into()),
                count: 1,
            }],
            structure: vec![
                StructureHit {
                    kind: "not-x-its-y".into(),
                    excerpt: "not a draft, it's a letter".into(),
                },
                StructureHit {
                    kind: "em-dash".into(),
                    excerpt: "1.5 per 1k vs target 2".into(),
                },
            ],
            ..JudgeReport::default()
        }
    }

    #[test]
    fn underlines_whole_words_and_skips_non_prose_and_em_dash_summaries() {
        let text = "\
---
tell: delve
---

Please delve, then delved, and elevate.

```
delve
```

Use `elevate` in code. not a draft, it's a letter — ✨
The rate is 1.5 per 1k vs target 2.
";
        let kit = rune_kit::Kit::neutral(false);
        let marks = spans(text, &report(), &kit);
        let marked = |start: usize, needle: &str| {
            marks.iter().any(|span| {
                span.start as usize == start && span.end as usize == start + needle.len()
            })
        };
        let prose = text.find("Please delve").unwrap() + "Please ".len();
        assert!(marked(prose, "delve"));
        let elevate = text.find("and elevate").unwrap() + "and ".len();
        assert!(marked(elevate, "elevate"));
        let letter = text.find("not a draft, it's a letter").unwrap();
        assert!(marked(letter, "not a draft, it's a letter"));
        assert!(
            !marks
                .iter()
                .any(|span| span.start as usize == text.find("delved").unwrap())
        );
        assert!(
            !marks
                .iter()
                .any(|span| span.start as usize
                    == text.find("tell: delve").unwrap() + "tell: ".len())
        );
        let fenced = text.find("```\ndelve").unwrap() + 4;
        assert!(!marks.iter().any(|span| span.start as usize == fenced));
        let coded = text.find("`elevate`").unwrap() + 1;
        assert!(!marks.iter().any(|span| span.start as usize == coded));
        let summary = text.find("1.5 per 1k vs target 2").unwrap();
        assert!(!marks.iter().any(|span| span.start as usize == summary));
    }
}
