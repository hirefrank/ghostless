use rune::{col, row, El, ScrollSpec};
use rune_kit::{Badge, Kit, Markdown};
use crate::{markdown_decor, prefs::UiPrefs, state::Editor, theme};

pub(crate) fn build(source: &str, prefs: &UiPrefs, kit: &Kit, find: Option<&crate::search::FindState>) -> El {
    let (body, fields) = preview_parts(source);
    let mut column = col().w_full().gap(16.0);
    if !fields.is_empty() {
        let mut pills = row().w_full().gap(6.0);
        for (key, value) in fields { pills = pills.child(Badge::new(format!("{key}: {value}")).build(kit)); }
        column = column.child(pills);
    }
    let end = source.len() - body.len();
    let highlights: Vec<_> = find.filter(|f| f.open).into_iter().flat_map(|f| f.matches.iter().enumerate().filter(move |(_, range)| range.start >= end).map(move |(index, range)| {
        (range.start - end..range.end - end, rune_kit::markdown::Highlight::new().current_match(f.current == Some(index)).key(Markdown::highlight_key(index)))
    })).collect();
    column = column.child(Markdown::new(body).highlights(highlights).selectable(true)
        .on_link_tx(|_: &mut Editor, url, tx| { let _ = tx.open_url(url); }).build(kit));
    if let Some(width) = theme::measure_max_width(prefs.measure) { column = column.max_w(width); }
    col().key("preview-body").grow(1.0).w_full().min_h(0.0).items_center()
        .scroll(ScrollSpec { stick_bottom: false }).px(32.0).py(16.0).child(column)
}

fn preview_parts(source: &str) -> (&str, Vec<(String, String)>) {
    let end = markdown_decor::frontmatter_end(source).unwrap_or(0);
    (&source[end..], markdown_decor::parse_frontmatter_fields(source))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn strips_frontmatter_and_keeps_body() {
        let (body, fields) = preview_parts("---\nstage: seeded\n---\n# Title\n");
        assert_eq!(body, "# Title\n");
        assert_eq!(fields, [("stage".into(), "seeded".into())]);
        assert_eq!(preview_parts("# Title").0, "# Title");
    }
}
