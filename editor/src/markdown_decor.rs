use std::ops::Range;

use rune::Span;
use rune_kit::Kit;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeadingEntry {
    pub level: u8,
    pub title: String,
    pub byte_offset: usize,
}

pub(crate) fn frontmatter_end(text: &str) -> Option<usize> {
    let first = text.find('\n')? + 1;
    if text[..first].trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let mut offset = first;
    for line in text[first..].split_inclusive('\n') {
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(offset);
        }
    }
    None
}

pub(crate) fn parse_frontmatter_fields(text: &str) -> Vec<(String, String)> {
    let Some(end) = frontmatter_end(text) else {
        return Vec::new();
    };
    text[..end]
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            (!key.is_empty()).then(|| (key.to_string(), value.trim().to_string()))
        })
        .collect()
}

// Return normal prose lines and fenced lines with their original byte offsets.
fn lines(text: &str) -> Vec<(usize, &str, u8)> {
    let fm = frontmatter_end(text).unwrap_or(0);
    let mut offset = fm;
    let mut fence: Option<u8> = None;
    let mut result = Vec::new();
    for line in text[fm..].split_inclusive('\n') {
        let trimmed = line.trim_start();
        let bytes = trimmed.as_bytes();
        let marker = bytes.first().copied().filter(|b| *b == b'`' || *b == b'~');
        let is_fence = marker.is_some_and(|b| bytes.iter().take_while(|c| **c == b).count() >= 3);
        let kind = if is_fence && (fence.is_none() || fence == marker) {
            fence = if fence.is_some() { None } else { marker };
            1
        } else if fence.is_some() {
            2
        } else {
            0
        };
        result.push((offset, line, kind));
        offset += line.len();
    }
    result
}

fn heading(line: &str) -> Option<(usize, usize)> {
    let count = line.bytes().take_while(|b| *b == b'#').count();
    (count > 0 && count <= 6 && line.as_bytes().get(count) == Some(&b' '))
        .then_some((count, count + 1))
}

pub(crate) fn extract_headings(text: &str) -> Vec<HeadingEntry> {
    lines(text)
        .into_iter()
        .filter_map(|(offset, line, kind)| {
            if kind != 0 {
                return None;
            }
            let (level, start) = heading(line)?;
            Some(HeadingEntry {
                level: level as u8,
                title: line[start..].trim().to_string(),
                byte_offset: offset,
            })
        })
        .collect()
}

fn inline_code(line: &str) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut pos = 0;
    while let Some(start) = line[pos..].find('`').map(|n| pos + n) {
        let Some(end) = line[start + 1..].find('`').map(|n| start + 1 + n) else {
            break;
        };
        result.push(start..end + 1);
        pos = end + 1;
    }
    result
}

/// YAML frontmatter, fenced code, and inline code. Voice marks and placeholders both skip these.
pub(crate) fn non_prose_ranges(text: &str) -> Vec<Range<usize>> {
    let mut excluded = Vec::new();
    if let Some(end) = frontmatter_end(text) {
        excluded.push(0..end);
    }
    for (offset, line, kind) in lines(text) {
        if kind != 0 {
            excluded.push(offset..offset + line.len());
        } else {
            excluded.extend(
                inline_code(line)
                    .into_iter()
                    .map(|range| offset + range.start..offset + range.end),
            );
        }
    }
    excluded
}

pub(crate) fn find_placeholder_ranges(
    text: &str,
    placeholder_token: Option<&str>,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let excluded = non_prose_ranges(text);
    let mut pos = 0;
    while let Some(start) = text[pos..].find('[').map(|n| pos + n) {
        let Some(end) = text[start + 1..].find(']').map(|n| start + 1 + n) else { break; };
        let candidate = start..end + 1;
        if !excluded.iter().any(|r| r.start < candidate.end && r.end > candidate.start)
            && let Some((name, _)) = text[start + 1..end].split_once(':') {
            let mut chars = name.bytes();
            let generic = name.len() >= 2
                && chars.next().is_some_and(|b| b.is_ascii_uppercase())
                && chars.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b" _-".contains(&b));
            if generic || placeholder_token.is_some_and(|token| name.to_lowercase() == token.to_lowercase()) {
                ranges.push(candidate);
                pos = end + 1;
                continue;
            }
        }
        pos = start + 1;
    }
    ranges
}

pub(crate) fn compute_markdown_spans(
    text: &str,
    base_size: f32,
    placeholder_token: Option<&str>,
    kit: &Kit,
) -> Vec<Span> {
    let mut spans = Vec::new();
    if let Some(end) = frontmatter_end(text) {
        spans.push(
            Span::new(0, end)
                .mono()
                .size((base_size - 2.0).max(12.0))
                .color(kit.c.text_muted)
                .bg(kit.c.layer_02, 1.0),
        );
    }
    for (offset, line, kind) in lines(text) {
        let end = offset + line.trim_end_matches(['\r', '\n']).len();
        if kind != 0 {
            spans.push(Span::new(offset, end).mono().color(if kind == 1 {
                kit.c.text_faint
            } else {
                kit.c.text_muted
            }));
            continue;
        }
        if let Some((level, start)) = heading(line) {
            spans.push(Span::new(offset, offset + start).color(kit.c.text_faint));
            let scale = match level {
                1 => 1.45,
                2 => 1.25,
                3 => 1.12,
                _ => 1.0,
            };
            spans.push(
                Span::new(offset + start, end)
                    .bold()
                    .size(base_size * scale),
            );
        } else if line.starts_with("> ") {
            spans.push(Span::new(offset, offset + 2).color(kit.c.text_faint));
            spans.push(Span::new(offset + 2, end).italic().color(kit.c.text_muted));
        } else {
            let leading = line.len() - line.trim_start_matches(' ').len();
            let rest = &line[leading..];
            let marker = if ["- ", "* ", "+ "].iter().any(|m| rest.starts_with(m)) {
                2
            } else {
                let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 && rest[digits..].starts_with(". ") {
                    digits + 2
                } else {
                    0
                }
            };
            if marker > 0 {
                spans.push(
                    Span::new(offset + leading, offset + leading + marker).color(kit.c.text_faint),
                );
            }
        }
        let codes = inline_code(line);
        for code in &codes {
            spans.push(
                Span::new(offset + code.start, offset + code.start + 1).color(kit.c.text_faint),
            );
            spans.push(
                Span::new(offset + code.start + 1, offset + code.end - 1)
                    .mono()
                    .bg(kit.c.layer_03, 1.0),
            );
            spans.push(Span::new(offset + code.end - 1, offset + code.end).color(kit.c.text_faint));
        }
        let mut pos = 0;
        while pos < line.len() {
            if let Some(code) = codes.iter().find(|r| r.contains(&pos)) {
                pos = code.end;
                continue;
            }
            let tail = &line[pos..];
            if tail.starts_with('[')
                && let Some(close) = tail.find("](")
                && let Some(end_link) = tail[close + 2..].find(')') {
                        let finish = close + 2 + end_link + 1;
                        spans.push(
                            Span::new(offset + pos, offset + pos + finish).color(kit.c.text_faint),
                        );
                        spans.push(
                            Span::new(offset + pos + 1, offset + pos + close)
                                .color(kit.c.text_accent),
                        );
                        pos += finish;
                        continue;
            }
            let marker = ["**", "__", "*", "_"]
                .into_iter()
                .find(|m| tail.starts_with(m));
            if let Some(m) = marker {
                let before_word = pos > 0
                    && line[..pos]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphanumeric);
                if !(m == "_" && before_word)
                    && let Some(close) = tail[m.len()..].find(m).map(|n| n + m.len())
                    && !(m == "_" && tail[close + m.len()..].chars().next().is_some_and(char::is_alphanumeric)) {
                        let finish = close + m.len();
                        if !codes.iter().any(|r| r.start < pos + finish && r.end > pos) {
                            spans.push(
                                Span::new(offset + pos, offset + pos + m.len())
                                    .color(kit.c.text_faint),
                            );
                            let inner = Span::new(offset + pos + m.len(), offset + pos + close);
                            spans.push(if m.len() == 2 {
                                inner.bold()
                            } else {
                                inner.italic()
                            });
                            spans.push(
                                Span::new(offset + pos + close, offset + pos + finish)
                                    .color(kit.c.text_faint),
                            );
                            pos += finish;
                            continue;
                        }
                    }
            }
            pos += tail.chars().next().unwrap().len_utf8();
        }
    }
    for range in find_placeholder_ranges(text, placeholder_token) {
        spans.push(
            Span::new(range.start, range.end)
                .bold()
                .color(kit.c.warning.fg)
                .bg(kit.c.warning.bg, 1.0),
        );
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_skip_code_and_match_custom() {
        let text = "[WRITER: your take] [FRANK: add story] [frank: x] [see p. 3] `[WRITER: code]`\n```\n[TODO: code]\n```";
        let found: Vec<_> = find_placeholder_ranges(text, Some("Frank"))
            .into_iter()
            .map(|r| &text[r])
            .collect();
        assert_eq!(
            found,
            ["[WRITER: your take]", "[FRANK: add story]", "[frank: x]"]
        );
    }

    #[test]
    fn frontmatter_and_headings() {
        let text = "---\nstage: seeded\nowner: frank\n---\n# Title\n```\n# comment\n```\n## Café";
        assert_eq!(
            parse_frontmatter_fields(text),
            [
                ("stage".into(), "seeded".into()),
                ("owner".into(), "frank".into())
            ]
        );
        let headings = extract_headings(text);
        assert_eq!(
            headings
                .iter()
                .map(|h| h.title.as_str())
                .collect::<Vec<_>>(),
            ["Title", "Café"]
        );
    }
    #[test]
    fn utf8_spans_are_on_boundaries() {
        let text = "---\nstage: seeded\n---\n# Café — “smart quotes”\n> こんにちは ✨ **bold** `code` [WRITER: your take]";
        let before = text.to_owned();
        for span in compute_markdown_spans(text, 16.5, None, &Kit::neutral(false)) {
            assert!(text.is_char_boundary(span.start as usize));
            assert!(text.is_char_boundary(span.end as usize));
            assert!(span.start <= span.end && span.end as usize <= text.len());
        }
        assert_eq!(text, before);
    }

}
