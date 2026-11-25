use std::ops::Range;

use rune::{El, ScrollSpec, TextHighlight, Tx, col, row, text};
use rune_kit::{Badge, Kit, Markdown, markdown::Highlight};

use crate::{markdown_decor, prefs::UiPrefs, search::FindState, state::Editor, theme};

const PREVIEW_BODY: &str = "preview-body";
const PREVIEW_FRONTMATTER: &str = "preview-frontmatter";

pub(crate) fn reveal_find_match(source: &str, start: usize, index: usize, tx: &mut Tx) {
    tx.scroll_into_view_in(find_scope(source, start), Markdown::highlight_key(index));
}

fn find_scope(source: &str, start: usize) -> &'static str {
    if markdown_decor::frontmatter_end(source).is_some_and(|end| start < end) {
        PREVIEW_FRONTMATTER
    } else {
        PREVIEW_BODY
    }
}

pub(crate) fn build(source: &str, prefs: &UiPrefs, kit: &Kit, find: Option<&FindState>) -> El {
    let end = markdown_decor::frontmatter_end(source).unwrap_or(0);
    let (frontmatter, body) = source.split_at(end);
    let find = find.filter(|find| find.open);
    let mut column = col().w_full().gap(16.0);
    if let Some(find) = find.filter(|find| find.matches.first().is_some_and(|hit| hit.start < end))
    {
        // Pills summarize YAML and omit parts such as delimiters and comments.
        // Reveal the exact source while it has find hits so each hit can be shown.
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
                        .text_highlights(highlights),
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
    column = column.child(
        Markdown::new(body)
            .highlights(highlights)
            .selectable(true)
            .on_link_tx(|_: &mut Editor, url, tx| {
                let _ = tx.open_url(url);
            })
            .build(kit),
    );
    if let Some(width) = theme::measure_max_width(prefs.measure) {
        column = column.max_w(width);
    }
    col()
        .key(PREVIEW_BODY)
        .grow(1.0)
        .w_full()
        .min_h(0.0)
        .items_center()
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .px(32.0)
        .py(16.0)
        .child(column)
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
            assert_eq!(find_scope(source, hit.start), PREVIEW_FRONTMATTER);
            assert_eq!(&frontmatter[section_range(hit, 0..end).unwrap()], "Café");
            assert_eq!(section_range(hit, end..source.len()), None);
        }
        assert_eq!(
            &body[section_range(&hits[2], end..source.len()).unwrap()],
            "Café"
        );
        assert_eq!(section_range(&hits[2], 0..end), None);
        assert_eq!(find_scope(source, hits[2].start), PREVIEW_BODY);
        assert_eq!(find_scope(source, end), PREVIEW_BODY);
        assert_eq!(find_scope("# Plain Markdown", 0), PREVIEW_BODY);

        let across = end - 1..end + 1;
        assert_eq!(section_range(&across, 0..end), Some(end - 1..end));
        assert_eq!(section_range(&across, end..source.len()), Some(0..1));
    }
}
