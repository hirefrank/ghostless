use std::ops::Range;

use rune::{Color, El, ScrollSpec, TextHighlight, Tx, col, row, text};
use rune_kit::{Badge, Kit, Markdown, markdown::Highlight};

use crate::{
    markdown_decor,
    prefs::UiPrefs,
    search::{FindState, caret_reveal_range},
    state::Editor,
    theme,
};

pub(crate) const PREVIEW_BODY: &str = "preview-body";
const PREVIEW_FRONTMATTER: &str = "preview-frontmatter";
const PREVIEW_FIND_ANCHOR: &str = "preview-find-anchor";

pub(crate) fn reveal_find_position(source: &str, find: &FindState, tx: &mut Tx) {
    let Some(anchor) = find.navigation_anchor() else {
        return;
    };
    let end = markdown_decor::frontmatter_end(source).unwrap_or(0);
    if let Some(index) = find.current {
        tx.scroll_into_view_in(find_scope(end, anchor), Markdown::highlight_key(index));
        return;
    }

    // Markdown can hide a whole source line, such as an HTML comment. Try the
    // nearest match first as a fallback, then reveal the actual anchor if drawn.
    if let Some((index, hit)) = find
        .matches
        .iter()
        .enumerate()
        .min_by_key(|(_, hit)| hit.start.abs_diff(anchor))
    {
        tx.scroll_into_view_in(find_scope(end, hit.start), Markdown::highlight_key(index));
    }
    if let Some(range) = anchor_range(source, end, anchor) {
        tx.scroll_into_view_in(find_scope(end, range.start), PREVIEW_FIND_ANCHOR);
    }
}

fn anchor_range(source: &str, frontmatter_end: usize, anchor: usize) -> Option<Range<usize>> {
    let (offset, section) = if anchor < frontmatter_end {
        (0, &source[..frontmatter_end])
    } else {
        (frontmatter_end, &source[frontmatter_end..])
    };
    let range = caret_reveal_range(section, anchor - offset)?;
    Some(offset + range.start..offset + range.end)
}

fn find_scope(frontmatter_end: usize, start: usize) -> &'static str {
    if start < frontmatter_end {
        PREVIEW_FRONTMATTER
    } else {
        PREVIEW_BODY
    }
}

pub(crate) fn build(source: &str, prefs: &UiPrefs, kit: &Kit, find: Option<&FindState>) -> El {
    let end = markdown_decor::frontmatter_end(source).unwrap_or(0);
    let (frontmatter, body) = source.split_at(end);
    let is_active = find.is_some();
    let find = find.filter(|find| find.open);
    let anchor = find
        .filter(|find| find.current.is_none())
        .and_then(FindState::navigation_anchor)
        .and_then(|anchor| anchor_range(source, end, anchor));
    let yaml_anchor = anchor
        .as_ref()
        .and_then(|range| section_range(range, 0..end));
    let mut column = col().w_full().gap(16.0);
    if let Some(find) = find.filter(|find| {
        yaml_anchor.is_some() || find.matches.first().is_some_and(|hit| hit.start < end)
    }) {
        // Pills summarize YAML and omit parts such as delimiters and comments.
        // Reveal the exact source while Find needs to show hits or its last position.
        let highlights = find.matches.iter().enumerate().filter_map(|(index, hit)| {
            let range = section_range(hit, 0..end)?;
            let active = find.current == Some(index);
            let bg = if active {
                kit.c.warning.fg
            } else {
                kit.c.warning.bg
            };
            let mark = TextHighlight::new(range, bg).key(Markdown::highlight_key(index));
            Some(if active {
                mark.color(kit.c.warning.bg)
            } else {
                mark
            })
        });
        column = column.child(
            col()
                .key(PREVIEW_FRONTMATTER)
                .w_full()
                .min_w(0.0)
                .padding(12.0)
                .gap(6.0)
                .radius(6.0)
                .fill(kit.c.layer_01)
                .child(
                    text("Frontmatter")
                        .font(kit.ty.small_medium)
                        .color(kit.c.text_muted),
                )
                .child(
                    text(frontmatter)
                        .font(kit.ty.ui.mono())
                        .color(kit.c.text_base)
                        .w_full()
                        .wrap_anywhere()
                        .selectable(true)
                        .text_highlights(highlights.chain(yaml_anchor.map(|range| {
                            TextHighlight::new(range, Color::TRANSPARENT).key(PREVIEW_FIND_ANCHOR)
                        }))),
                ),
        );
    } else {
        let fields = markdown_decor::parse_frontmatter_fields(source);
        if !fields.is_empty() {
            let pills = fields
                .into_iter()
                .fold(row().w_full().gap(6.0), |pills, (key, value)| {
                    pills.child(Badge::new(format!("{key}: {value}")).build(kit))
                });
            column = column.child(pills);
        }
    }

    let highlights = find.into_iter().flat_map(|find| {
        find.matches
            .iter()
            .enumerate()
            .filter_map(move |(index, hit)| {
                let range = section_range(hit, end..source.len())?;
                let key = Markdown::highlight_key(index);
                // The fragment where the match starts owns the scroll target.
                let key = if hit.start < end {
                    format!("{key}-body")
                } else {
                    key
                };
                Some((
                    range,
                    Highlight::new()
                        .current_match(find.current == Some(index))
                        .key(key),
                ))
            })
    });
    let body_anchor = anchor
        .and_then(|range| section_range(&range, end..source.len()))
        .map(|range| {
            (
                range,
                Highlight::new()
                    .bg(Color::TRANSPARENT)
                    .key(PREVIEW_FIND_ANCHOR),
            )
        });
    column = column.child(
        Markdown::new(body)
            .highlights(highlights.chain(body_anchor))
            .selectable(true)
            .on_link_tx(|_: &mut Editor, url, tx| {
                let _ = tx.open_url(url);
            })
            .build(kit),
    );
    if let Some(width) = theme::measure_max_width(prefs.measure) {
        column = column.max_w(width);
    }
    let viewport = col()
        .key(if is_active {
            PREVIEW_BODY
        } else {
            "reference-preview-body"
        })
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .items_center()
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .px(32.0)
        .py(16.0)
        .child(column);
    if is_active {
        viewport
            .focusable(true)
            .role("region")
            .label("Markdown preview")
    } else {
        viewport
    }
}

fn section_range(hit: &Range<usize>, section: Range<usize>) -> Option<Range<usize>> {
    let start = hit.start.max(section.start);
    let end = hit.end.min(section.end);
    (start < end).then(|| start - section.start..end - section.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::find_matches;

    #[test]
    fn preview_maps_matches_on_both_sides_of_frontmatter() {
        let source = "---\r\nstage: Café\r\n# Café note\r\n---\r\n# Café title\r\n";
        let end = markdown_decor::frontmatter_end(source).unwrap();
        let (frontmatter, body) = source.split_at(end);
        let hits = find_matches(source, "café", false, false);
        assert_eq!(hits.len(), 3);
        for hit in &hits[..2] {
            assert_eq!(find_scope(end, hit.start), PREVIEW_FRONTMATTER);
            assert_eq!(&frontmatter[section_range(hit, 0..end).unwrap()], "Café");
            assert_eq!(section_range(hit, end..source.len()), None);
        }
        assert_eq!(
            &body[section_range(&hits[2], end..source.len()).unwrap()],
            "Café"
        );
        assert_eq!(section_range(&hits[2], 0..end), None);
        assert_eq!(find_scope(end, hits[2].start), PREVIEW_BODY);
        assert_eq!(find_scope(end, end), PREVIEW_BODY);
        assert_eq!(find_scope(0, 0), PREVIEW_BODY);

        // A remapped byte may land between bytes of é. Reveal the whole character.
        let yaml_anchor = anchor_range(source, end, hits[0].end - 1).unwrap();
        let body_anchor = anchor_range(source, end, hits[2].end - 1).unwrap();
        assert_eq!(find_scope(end, yaml_anchor.start), PREVIEW_FRONTMATTER);
        assert_eq!(find_scope(end, body_anchor.start), PREVIEW_BODY);
        assert_eq!(&source[yaml_anchor], "é");
        assert_eq!(&source[body_anchor], "é");

        let across = end - 1..end + 1;
        assert_eq!(section_range(&across, 0..end), Some(end - 1..end));
        assert_eq!(section_range(&across, end..source.len()), Some(0..1));
    }
}
