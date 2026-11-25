use rune::{col, text_input, Cx, El, ScrollSpec, Tx};
use rune_kit::Kit;
use crate::{state::Editor, theme};

pub(crate) fn build(editor: &Editor, cx: &Cx<Editor>, kit: &Kit, locked: bool) -> El {
    let height = cx.layout_size("working-body").map(|s| s[1]).unwrap_or(80.0).max(80.0);
    let doc = editor.active_doc_ref().expect("canvas requires an active document");
    let key = format!("{:?}:{}:{}:{:?}", editor.store.ui.theme, cx.system_dark(), editor.store.ui.font_size.to_bits(), editor.placeholder_token);
    let mut cache = doc.decoration_cache.borrow_mut();
    if cache.as_ref().is_none_or(|c| c.generation != doc.generation || c.key != key) {
        let mut headings = crate::markdown_decor::extract_headings(&doc.edit.text);
        headings.sort_by_key(|heading| heading.byte_offset);
        let fields = crate::markdown_decor::parse_frontmatter_fields(&doc.edit.text);
        let outline = headings.iter().map(|heading| format!("level {} {}", heading.level, heading.title)).collect::<Vec<_>>().join(", ");
        let metadata = fields.iter().map(|(key, value)| format!("{key}: {value}")).collect::<Vec<_>>().join(", ");
        let description = format!("Headings: {outline}. Frontmatter: {metadata}");
        *cache = Some(crate::documents::DecorationCache {
            generation: doc.generation, key,
            spans: crate::markdown_decor::compute_markdown_spans(&doc.edit.text, editor.store.ui.font_size.clamp(13.0, 24.0), editor.placeholder_token.as_deref(), kit),
            description,
        });
    }
    let cached = cache.as_ref().expect("decoration cache initialized");
    let mut spans = cached.spans.clone();
    if editor.find.open {
        spans.extend(editor.find.matches.iter().enumerate().map(|(index, range)| {
            let color = if editor.find.current == Some(index) { kit.c.warning.fg } else { kit.c.warning.bg };
            let span = rune::Span::new(range.start, range.end).bg(color, 1.0);
            if editor.find.current == Some(index) { span.color(kit.c.warning.bg) } else { span }
        }));
    }
    let mut field = text_input(editor.active_working_edit(), theme::prose_text_style(&editor.store.ui))
        .key("prose-input").label("Working draft").description(cached.description.clone()).decorations(spans).w_full().min_h((height - 32.0).max(80.0))
        .color(kit.c.text_base).caret(kit.c.text_base, kit.selection())
        .placeholder("Start writing…", kit.c.text_faint).spellcheck(editor.store.ui.spellcheck)
        .disabled(locked);
    if editor.find.open && doc.edit.preedit().is_none() {
        // The transparent mark gives Rune a scroll target without taking focus
        // from Find. After the last replacement it follows the collapsed caret.
        let target = editor.find.current_range().cloned()
            .map(|range| (range, crate::search::SOURCE_MATCH_KEY))
            .or_else(|| {
                crate::search::caret_reveal_range(&doc.edit.text, doc.edit.focus)
                    .map(|range| (range, crate::search::SOURCE_CARET_KEY))
            });
        if let Some((range, key)) = target {
            field = field.text_highlight(
                rune::TextHighlight::new(range, rune::Color::TRANSPARENT).key(key),
            );
        }
    }
    if !locked {
        field = field.on_edit(|e: &mut Editor| e.active_working_edit_mut())
            .on_input_key(|e: &mut Editor, key, tx: &mut Tx| {
                let handled = crate::markdown_edit::handle(e.active_working_edit_mut(), key);
                if handled { e.changed_active(tx); }
                handled
            })
            .on_input_change(|e: &mut Editor, tx: &mut Tx| e.changed_active(tx))
            .on_submit(|e: &mut Editor, tx: &mut Tx| { e.active_working_edit_mut().insert("\n"); e.changed_active(tx); });
    }
    let mut inner = col().w_full().child(field);
    if let Some(width) = theme::measure_max_width(editor.store.ui.measure) { inner = inner.max_w(width); }
    let body = col().key("working-body").grow(1.0).w_full().min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false }).items_center().px(32.0).py(16.0).child(inner);
    if locked {
        body.focusable(true).role("region").label("Draft canvas")
    } else {
        body
    }
}
