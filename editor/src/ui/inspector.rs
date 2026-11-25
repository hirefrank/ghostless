//! Right inspector: voice, notes, reference files, and the document outline.

use std::path::Path;

use rune::{Cx, El, ScrollSpec, Tx, col, el, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, Kit, Markdown, SegmentedControl, Select, Switch, TextArea,
    TextInput,
};

use crate::{
    bridge::JudgeReport,
    markdown_decor,
    state::{Editor, section_for_document},
};

const TABS: [&str; 4] = ["Voice", "Notes", "Ref", "Outline"];

pub(crate) fn build(editor: &Editor, cx: &Cx<Editor>, kit: &Kit, overlays: &mut Vec<El>) -> El {
    let tab = editor.inspector_tab.min(TABS.len() - 1);
    let tabs = SegmentedControl::new(&TABS, Some(tab))
        .width(Some(280.0))
        .label("Inspector")
        .on_pick(|editor: &mut Editor, index, _tx: &mut Tx| editor.inspector_tab = index)
        .build(kit);
    let body = match tab {
        0 => voice_tab(editor, kit, overlays),
        1 => notes_tab(editor, cx, kit),
        2 => ref_tab(editor, kit),
        _ => outline_tab(editor, kit),
    };
    col()
        .key("inspector")
        .w_full()
        .h_full()
        .min_h(0.0)
        .padding(12.0)
        .gap(10.0)
        .child(tabs)
        .child(
            col()
                .key("inspector-body")
                .grow(1.0)
                .w_full()
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .gap(8.0)
                .child(body),
        )
}

fn voice_tab(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) -> El {
    let mut column = col().gap(10.0).w_full();
    column = column.child(section_select(editor, kit, overlays));
    column = column.child(
        Switch::new(editor.store.ui.voice_lens)
            .label("Voice lens")
            .on_toggle(|editor: &mut Editor| editor.toggle_voice_lens())
            .build(kit)
            .key("voice-lens-switch"),
    );
    column = column.child(
        Button::new("Next placeholder (F8)")
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press_tx(|editor: &mut Editor, tx| editor.jump_next_placeholder(tx))
            .build(kit)
            .key("next-placeholder"),
    );
    match editor.active_judge_report() {
        Some(report) if report.needs_build => {
            column = column.child(quiet("No voice guide yet.", kit));
            column = column.child(build_button(kit));
        }
        Some(report) => column = column.child(report_body(editor, report, kit)),
        None => {
            column = column.child(quiet("Voice score appears after the draft settles.", kit));
            if editor.voice_project.is_some() {
                column = column.child(build_button(kit));
            }
        }
    }
    column
}

fn section_select(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) -> El {
    let mut options = vec!["all".to_owned()];
    if let Some(report) = editor.active_judge_report() {
        for section in &report.available_sections {
            if !options.iter().any(|option| option == section) {
                options.push(section.clone());
            }
        }
    }
    let current = editor
        .active_doc_ref()
        .map(section_for_document)
        .unwrap_or_else(|| "all".into());
    if !options.iter().any(|option| option == &current) {
        options.push(current.clone());
    }
    let before = overlays.len();
    let trigger = Select::new("judge-section", &options)
        .label("Section")
        .width(220.0)
        .current(Some(&current))
        .open(editor.section_menu_open)
        .on_toggle(|editor: &mut Editor| editor.section_menu_open = !editor.section_menu_open)
        .on_close(|editor: &mut Editor| {
            editor.section_menu_open = false;
            editor.pending_document_focus = true;
        })
        .on_choose(|editor: &mut Editor, value, tx| {
            editor.section_menu_open = false;
            editor.choose_judge_section(value.to_owned(), tx);
            editor.focus_document(tx);
        })
        .build(kit, overlays);
    if let Some(key) = editor.document_focus_key()
        && overlays.len() > before
        && let Some(layer) = overlays.pop()
    {
        overlays.push(layer.focus_return(key));
    }
    col().gap(4.0).child(quiet("Section", kit)).child(trigger)
}

fn report_body(editor: &Editor, report: &JudgeReport, kit: &Kit) -> El {
    let mut column = col().gap(8.0).w_full();
    let gaps = report.gap_count();
    let summary = match gaps {
        0 => format!("Score {}", report.score),
        1 => format!("Score {} · 1 gap", report.score),
        count => format!("Score {score} · {count} gaps", score = report.score),
    };
    column = column.child(text(summary).font(kit.ty.ui_medium).color(kit.c.text_base));
    if let Some(confidence) = &report.confidence {
        column = column.child(
            text(format!("Confidence: {}", confidence.level))
                .font(kit.ty.small)
                .color(kit.c.text_muted),
        );
    }
    if report.stale {
        column = column.child(
            col()
                .key("stale-guide")
                .gap(6.0)
                .padding(8.0)
                .radius(6.0)
                .fill(kit.c.warning.bg)
                .child(
                    text("Voice guide is stale. Rebuild before trusting this score.")
                        .font(kit.ty.small)
                        .color(kit.c.text_base)
                        .wrap(),
                )
                .child(build_button(kit)),
        );
    }
    if let Some(error) = &report.error {
        column = column.child(
            text(error.clone())
                .font(kit.ty.small)
                .color(kit.c.danger.fg)
                .wrap(),
        );
    }
    for check in &report.checks {
        column = column.child(range_bar(check, kit));
    }
    column = column.child(placeholder_list(editor, kit));
    column = column.child(word_list(
        "Banned tells",
        &report
            .banned
            .iter()
            .map(|hit| hit.tell.clone())
            .collect::<Vec<_>>(),
        kit,
    ));
    let rules = report
        .confirmed_rules
        .iter()
        .map(|hit| hit.word.clone())
        .collect::<Vec<_>>();
    column = column.child(word_list("Confirmed rules", &rules, kit));
    column = column.child(structure_list(report, kit));
    column = column.child(live_checks(report, kit));
    column
}

fn range_bar(check: &crate::bridge::JudgeCheckMetric, kit: &Kit) -> El {
    let values = [check.low, check.high, check.actual, 0.0];
    let min = values.into_iter().fold(f64::INFINITY, f64::min);
    let max = values.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1e-6);
    let x = |value: f64| {
        if !value.is_finite() {
            return 0.0;
        }
        (((value - min) / span) * 220.0) as f32
    };
    let band_lo = check.low.min(check.high);
    let band_hi = check.low.max(check.high);
    let band_x = x(band_lo);
    let band_w = (x(band_hi) - band_x).max(2.0);
    let dot_x = (x(check.actual) - 4.0).clamp(0.0, 212.0);
    let ok = check.status == "ok";
    let band = if ok {
        kit.c.success.fg
    } else {
        kit.c.warning.fg
    };
    let dot = if ok {
        kit.c.success.fg
    } else {
        kit.c.danger.fg
    };
    let actual = if check.actual.is_finite() {
        format!("{:.1}", check.actual)
    } else {
        "—".into()
    };
    col()
        .gap(4.0)
        .w_full()
        .child(
            row()
                .w_full()
                .center_y()
                .child(
                    text(check.label.clone())
                        .font(kit.ty.small)
                        .color(kit.c.text_base)
                        .grow(1.0)
                        .shrink(),
                )
                .child(text(actual).font(kit.ty.small).color(kit.c.text_muted)),
        )
        .child(
            el().w(220.0)
                .h(10.0)
                .child(
                    el().absolute(0.0, 2.0)
                        .w(220.0)
                        .h(6.0)
                        .radius(3.0)
                        .fill(kit.c.border_muted),
                )
                .child(
                    el().absolute(band_x, 2.0)
                        .w(band_w)
                        .h(6.0)
                        .radius(3.0)
                        .fill(band),
                )
                .child(
                    el().absolute(dot_x, 1.0)
                        .size(8.0, 8.0)
                        .radius(4.0)
                        .fill(dot),
                ),
        )
}

fn placeholder_list(editor: &Editor, kit: &Kit) -> El {
    let text = editor
        .active_doc_ref()
        .map(|doc| doc.edit.text.clone())
        .unwrap_or_default();
    let ranges =
        markdown_decor::find_placeholder_ranges(&text, editor.placeholder_token.as_deref());
    let mut column = col().gap(4.0).child(quiet("Placeholders", kit));
    if ranges.is_empty() {
        return column.child(quiet("None in this draft.", kit));
    }
    for (index, range) in ranges.iter().cloned().enumerate() {
        let label = text.get(range.clone()).unwrap_or("").to_owned();
        column = column.child(
            Button::new(label)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |editor: &mut Editor, tx| {
                    editor.jump_to_range(range.start, range.end, tx);
                })
                .build(kit)
                .key(format!("jump-placeholder-{index}")),
        );
    }
    column
}

fn word_list(title: &str, words: &[String], kit: &Kit) -> El {
    let mut column = col().gap(4.0).child(quiet(title, kit));
    if words.is_empty() {
        return column.child(quiet("None.", kit));
    }
    for (index, word) in words.iter().cloned().enumerate() {
        let query = word.clone();
        column = column.child(
            Button::new(word)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |editor: &mut Editor, tx| editor.jump_to_query(&query, tx))
                .build(kit)
                .key(format!("jump-{title}-{index}")),
        );
    }
    column
}

fn structure_list(report: &JudgeReport, kit: &Kit) -> El {
    let mut column = col().gap(4.0).child(quiet("Structure", kit));
    if report.structure.is_empty() {
        return column.child(quiet("None.", kit));
    }
    for (index, hit) in report.structure.iter().cloned().enumerate() {
        if hit.kind == "em-dash" {
            column = column.child(
                text(format!("Em dash · {}", hit.excerpt))
                    .font(kit.ty.small)
                    .color(kit.c.text_muted)
                    .wrap(),
            );
            continue;
        }
        let excerpt = hit.excerpt.clone();
        let label = if excerpt.is_empty() {
            hit.kind.clone()
        } else {
            excerpt.clone()
        };
        column = column.child(
            Button::new(label)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |editor: &mut Editor, tx| editor.jump_to_excerpt(&excerpt, tx))
                .build(kit)
                .key(format!("jump-structure-{index}")),
        );
    }
    column
}

fn live_checks(report: &JudgeReport, kit: &Kit) -> El {
    let mut column = col().gap(4.0).child(quiet("Live checks", kit));
    if report.confirmed_checks.is_empty() {
        return column.child(quiet("None confirmed.", kit));
    }
    for (index, check) in report.confirmed_checks.iter().cloned().enumerate() {
        let mark = if check.pass { "pass" } else { "fail" };
        let label = format!("{mark} · {}", check.text);
        let query = if check.span.is_empty() {
            check.text.clone()
        } else {
            check.span.clone()
        };
        let detail = check.text.clone();
        column = column.child(
            Button::new(label)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |editor: &mut Editor, tx| {
                    editor.show_live_check(&query, &detail, tx)
                })
                .build(kit)
                .key(format!("jump-check-{index}")),
        );
    }
    column
}

fn notes_tab(editor: &Editor, cx: &Cx<Editor>, kit: &Kit) -> El {
    let mut column = col().key("inspector-notes").gap(8.0).w_full();
    if editor
        .active_doc_ref()
        .is_some_and(|doc| doc.notes.is_some())
    {
        let max_h = cx
            .layout_size("inspector-notes")
            .map(|size| (size[1] - 64.0).max(120.0))
            .unwrap_or(220.0);
        column = column.child(quiet("Why did you change it?", kit)).child(
            TextArea::new(editor.active_notes_edit())
                .label("Edit notes")
                .placeholder("Optional context for your future self")
                .width(None)
                .rows(6)
                .max_height(max_h)
                .disabled(editor.active_is_locked())
                .on_edit(|editor: &mut Editor| editor.active_notes_edit_mut())
                .on_change(|editor: &mut Editor, tx: &mut Tx| editor.changed_active(tx))
                .build(kit),
        );
    } else {
        column = column.child(
            text("Start an edit session to keep notes with this draft.")
                .font(kit.ty.ui)
                .color(kit.c.text_faint)
                .wrap(),
        );
    }
    column = column.child(quiet("Save a durable rule", kit)).child(
        TextInput::new(&editor.rule_draft)
            .label("Rule")
            .placeholder("A rule worth keeping")
            .width(None)
            .on_edit(|editor: &mut Editor| &mut editor.rule_draft)
            .build(kit)
            .key("rule-draft"),
    );
    column = column.child(
        row()
            .gap(6.0)
            .child(rule_button("+ STYLE.md", "style", kit))
            .child(rule_button("+ AUDIENCE.md", "audience", kit)),
    );
    column
}

fn ref_tab(editor: &Editor, kit: &Kit) -> El {
    col()
        .gap(12.0)
        .w_full()
        .child(note_file(editor, "STYLE.md", kit))
        .child(note_file(editor, "AUDIENCE.md", kit))
}

fn note_file(editor: &Editor, name: &'static str, kit: &Kit) -> El {
    let mut column = col().gap(6.0).w_full().child(quiet(name, kit));
    match read_voice_note(editor, name) {
        Some(markdown) => {
            column = column
                .child(Markdown::new(markdown).build(kit))
                .child(edit_note_button(name, kit));
        }
        None => {
            column = column
                .child(
                    text(format!("{name} is not in this voice project yet."))
                        .font(kit.ty.small)
                        .color(kit.c.text_faint)
                        .wrap(),
                )
                .child(edit_note_button(name, kit));
        }
    }
    column
}

fn outline_tab(editor: &Editor, kit: &Kit) -> El {
    let text = editor
        .active_doc_ref()
        .map(|doc| doc.edit.text.clone())
        .unwrap_or_default();
    let headings = markdown_decor::extract_headings(&text);
    let mut column = col().gap(4.0).child(quiet("Document outline", kit));
    if headings.is_empty() {
        return column.child(quiet("No headings yet.", kit));
    }
    for (index, heading) in headings.iter().enumerate() {
        let offset = heading.byte_offset;
        let indent = "  ".repeat(heading.level.saturating_sub(1) as usize);
        column = column.child(
            Button::new(format!("{indent}{}", heading.title))
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |editor: &mut Editor, tx| {
                    editor.jump_to_range(offset, offset, tx);
                })
                .build(kit)
                .key(format!("jump-heading-{index}")),
        );
    }
    column
}

fn build_button(kit: &Kit) -> El {
    Button::new("Build voice guide")
        .variant(ButtonVariant::Neutral)
        .control_size(ControlSize::Small)
        .on_press(|editor: &mut Editor| editor.request_voice_build())
        .build(kit)
        .key("build-voice")
}

fn rule_button(label: &str, kind: &'static str, kit: &Kit) -> El {
    Button::new(label)
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press(move |editor: &mut Editor| editor.save_durable_rule(kind))
        .build(kit)
}

fn edit_note_button(name: &'static str, kit: &Kit) -> El {
    Button::new(format!("Open {name}"))
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press(move |editor: &mut Editor| editor.open_voice_note(name))
        .build(kit)
}

fn quiet(label: &str, kit: &Kit) -> El {
    text(label)
        .font(kit.ty.text_xsmall_medium)
        .color(kit.c.text_muted)
}

fn read_voice_note(editor: &Editor, name: &str) -> Option<String> {
    let voice = editor.voice_project.as_deref()?;
    let path = voice.join(name);
    if !path.is_file() || !note_is_writable(voice, &path) {
        return None;
    }
    crate::fs_io::read_utf8(&path).ok()
}

fn note_is_writable(voice: &Path, path: &Path) -> bool {
    crate::fs_io::is_contained_in(voice, path)
        && !crate::workspace_tree::is_protected_voice_path(path)
}
