use crate::{
    bridge::{Check, Rule},
    state::{ALL_SECTIONS, Decision, Editor, ReviewKind},
};
use rune::{El, ScrollSpec, col, row, text};
use rune_kit::{
    Badge, BadgeVariant, Button, ButtonVariant, ControlSize, Kit, SegmentedControl, Select,
};
use std::collections::BTreeSet;

pub(super) fn build(editor: &Editor, kit: &Kit, overlays: &mut Vec<El>) -> El {
    let mut heading = row().w_full().center_y().gap(8.0).flex_wrap().child(
        text("Voice Rules & Checks")
            .font(kit.ty.ui_medium)
            .color(kit.c.text_base)
            .key("voice-studio-heading")
            .focusable(true),
    );
    if let Some(rules) = &editor.studio.rules {
        heading = heading.child(
            Badge::new(format!("Minimum support: {}", rules.minimum_support))
                .variant(BadgeVariant::Subtle)
                .build(kit),
        );
    }
    heading = heading
        .child(row().grow(1.0))
        .child(
            Button::new(if editor.studio.building() {
                "Rebuilding…"
            } else {
                "Rebuild Voice Guide"
            })
            .variant(ButtonVariant::Outline)
            .disabled(editor.studio.building())
            .on_press(Editor::request_voice_build)
            .build(kit),
        )
        .child(
            Button::new("Close")
                .variant(ButtonVariant::Ghost)
                .on_press_tx(Editor::close_voice_studio)
                .build(kit),
        );
    let mut layout = col()
        .key("voice-studio")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .padding(18.0)
        .gap(10.0)
        .child(heading)
        .child(
            SegmentedControl::new(&["Voice Rules", "Edit Checks"], Some(editor.studio.tab))
                .label("Voice review")
                .on_pick(|editor: &mut Editor, tab, _| {
                    editor.studio.tab = tab;
                    editor.studio.scope_picker = None;
                })
                .build(kit),
        );
    if editor.studio.needs_rebuild {
        layout = layout.child(muted("Rebuild the guide to apply your decisions.", kit));
    }
    if let Some(error) = &editor.studio.error {
        layout = layout.child(
            text(error.to_string())
                .font(kit.ty.ui)
                .color(kit.c.text_base)
                .wrap(),
        );
    }

    let sections: BTreeSet<String> = editor
        .active_judge_report()
        .into_iter()
        .flat_map(|report| report.available_sections.iter().cloned())
        .chain(editor.studio.sections.iter().cloned())
        .chain(
            editor
                .studio
                .rules
                .iter()
                .flat_map(|rules| rules.hypotheses.iter().chain(&rules.confirmed))
                .flat_map(|rule| rule.sections.iter().cloned()),
        )
        .chain(
            editor
                .studio
                .checks
                .iter()
                .flat_map(|checks| checks.pending.iter().chain(&checks.confirmed))
                .filter_map(|check| check.section.clone()),
        )
        .collect();
    let content = if editor.studio.tab == 0 {
        rules_list(editor, kit, overlays, &sections)
    } else {
        checks_list(editor, kit, overlays, &sections)
    };
    layout.child(
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(content),
    )
}

fn rules_list(
    editor: &Editor,
    kit: &Kit,
    overlays: &mut Vec<El>,
    sections: &BTreeSet<String>,
) -> El {
    let Some(rules) = &editor.studio.rules else {
        return muted("Loading voice rules…", kit);
    };
    let mut body = section(
        "Hypotheses",
        rules.hypotheses.len(),
        "No hypotheses need a decision.",
        kit,
    );
    for rule in &rules.hypotheses {
        let mut card = card(&rule.id, kit).child(muted(
            format!(
                "{} pieces · {} counterexample(s) · {}",
                rule.support,
                rule.counterexamples.len(),
                if rule.sections.is_empty() {
                    ALL_SECTIONS.into()
                } else {
                    rule.sections.join(", ")
                }
            ),
            kit,
        ));
        for (index, example) in rule.examples.iter().take(2).enumerate() {
            card = card.child(
                col()
                    .key(format!("evidence-{index}"))
                    .gap(3.0)
                    .child(muted(format!("Before · {}", example.before_snippet), kit))
                    .child(muted(format!("After · {}", example.after_snippet), kit)),
            );
        }
        body = body.child(card.child(rule_controls(editor, kit, overlays, sections, rule, false)));
    }
    body = body.child(section(
        "Confirmed",
        rules.confirmed.len(),
        "No confirmed rules.",
        kit,
    ));
    for rule in &rules.confirmed {
        body = body.child(
            card(&rule.id, kit)
                .child(muted(
                    format!(
                        "{} · {} receipt(s){}",
                        rule.scope.as_deref().unwrap_or(ALL_SECTIONS),
                        rule.receipts.len(),
                        rule.confirmed_at
                            .as_ref()
                            .map_or_else(String::new, |date| format!(" · Confirmed {date}"))
                    ),
                    kit,
                ))
                .child(rule_controls(editor, kit, overlays, sections, rule, true)),
        );
    }
    body = body.child(history_header(
        editor,
        0,
        "Rejected and retired",
        rules.rejected.len() + rules.retired.len(),
        kit,
    ));
    if editor.studio.show_history[0] {
        for (name, items) in [("Rejected", &rules.rejected), ("Retired", &rules.retired)] {
            for rule in items {
                body = body.child(muted(format!("{name} · {}", rule.id), kit));
            }
        }
    }
    body
}

fn checks_list(
    editor: &Editor,
    kit: &Kit,
    overlays: &mut Vec<El>,
    sections: &BTreeSet<String>,
) -> El {
    let Some(checks) = &editor.studio.checks else {
        return muted("Loading edit checks…", kit);
    };
    let mut body = section(
        "Pending",
        checks.pending.len(),
        "No edit checks need a decision.",
        kit,
    );
    for check in &checks.pending {
        body = body.child(check_card(editor, kit, overlays, sections, check, false));
    }
    body = body.child(section(
        "Confirmed",
        checks.confirmed.len(),
        "No confirmed checks.",
        kit,
    ));
    for check in &checks.confirmed {
        body = body.child(check_card(editor, kit, overlays, sections, check, true));
    }
    body = body.child(history_header(
        editor,
        1,
        "Rejected",
        checks.rejected.len(),
        kit,
    ));
    if editor.studio.show_history[1] {
        for check in &checks.rejected {
            let label = if check.text.is_empty() {
                &check.id
            } else {
                &check.text
            };
            body = body.child(muted(label, kit));
        }
    }
    body
}

fn card(title: &str, kit: &Kit) -> El {
    col()
        .w_full()
        .padding(12.0)
        .gap(6.0)
        .radius(6.0)
        .fill(kit.c.layer_01)
        .border(1.0, kit.c.border_muted)
        .child(
            text(title)
                .font(kit.ty.ui_medium)
                .color(kit.c.text_base)
                .wrap(),
        )
}

fn check_card(
    editor: &Editor,
    kit: &Kit,
    overlays: &mut Vec<El>,
    sections: &BTreeSet<String>,
    check: &Check,
    confirmed: bool,
) -> El {
    let span = if check.polarity == "forbid" {
        "Cut"
    } else {
        "Added"
    };
    card(&check.text, kit)
        .child(muted(
            format!(
                "{span} “{}” · {} · {}",
                check.span,
                check.pair.as_deref().unwrap_or_default(),
                check.section.as_deref().unwrap_or(ALL_SECTIONS)
            ),
            kit,
        ))
        .child(
            row()
                .center_y()
                .gap(6.0)
                .flex_wrap()
                .child(scope_selector(
                    editor,
                    kit,
                    overlays,
                    sections,
                    &check.id,
                    check.section.as_deref(),
                ))
                .child(decision_button(
                    editor,
                    kit,
                    if confirmed { "Apply Scope" } else { "Confirm" },
                    ReviewKind::Checks,
                    Decision::Confirm,
                    &check.id,
                    check.section.as_deref(),
                ))
                .child(decision_button(
                    editor,
                    kit,
                    "Reject",
                    ReviewKind::Checks,
                    Decision::Reject,
                    &check.id,
                    None,
                )),
        )
}

fn rule_controls(
    editor: &Editor,
    kit: &Kit,
    overlays: &mut Vec<El>,
    sections: &BTreeSet<String>,
    rule: &Rule,
    confirmed: bool,
) -> El {
    row()
        .center_y()
        .gap(6.0)
        .flex_wrap()
        .child(scope_selector(
            editor,
            kit,
            overlays,
            sections,
            &rule.id,
            rule.scope.as_deref(),
        ))
        .child(decision_button(
            editor,
            kit,
            if confirmed { "Apply Scope" } else { "Confirm" },
            ReviewKind::Rules,
            Decision::Confirm,
            &rule.id,
            rule.scope.as_deref(),
        ))
        .child(decision_button(
            editor,
            kit,
            if confirmed { "Retire" } else { "Reject" },
            ReviewKind::Rules,
            if confirmed {
                Decision::Retire
            } else {
                Decision::Reject
            },
            &rule.id,
            None,
        ))
}

fn scope_selector(
    editor: &Editor,
    kit: &Kit,
    overlays: &mut Vec<El>,
    sections: &BTreeSet<String>,
    id: &str,
    current: Option<&str>,
) -> El {
    let selected = editor.studio.scope(id, current);
    let mut options = vec![ALL_SECTIONS.to_owned()];
    options.extend(
        sections
            .iter()
            .filter(|section| section.as_str() != ALL_SECTIONS)
            .cloned(),
    );
    if !options.contains(&selected) {
        options.push(selected.clone());
    }
    let toggle = id.to_owned();
    let choose = id.to_owned();
    Select::new(format!("review-scope-{id}"), &options)
        .label(format!("Section scope for {id}"))
        .width(150.0)
        .current(Some(&selected))
        .disabled(editor.studio.deciding())
        .open(editor.studio.scope_picker.as_deref() == Some(id))
        .on_toggle(move |editor: &mut Editor| {
            editor.studio.scope_picker =
                (editor.studio.scope_picker.as_ref() != Some(&toggle)).then(|| toggle.clone());
        })
        .on_close(|editor: &mut Editor| editor.studio.scope_picker = None)
        .on_choose(move |editor: &mut Editor, section, _| {
            editor
                .studio
                .chosen_scopes
                .insert(choose.clone(), section.into());
            editor.studio.scope_picker = None;
        })
        .build(kit, overlays)
}

fn decision_button(
    editor: &Editor,
    kit: &Kit,
    label: &str,
    kind: ReviewKind,
    decision: Decision,
    id: &str,
    scope: Option<&str>,
) -> El {
    let id = id.to_owned();
    let scope = scope.map(ToOwned::to_owned);
    Button::new(label)
        .control_size(ControlSize::Small)
        .disabled(editor.studio.deciding())
        .variant(if decision == Decision::Confirm {
            ButtonVariant::Submit
        } else {
            ButtonVariant::Outline
        })
        .on_press(move |editor: &mut Editor| {
            editor.decide_voice_item(kind, decision, &id, scope.as_deref())
        })
        .build(kit)
}

fn section(title: &str, count: usize, empty: &str, kit: &Kit) -> El {
    let mut body = col().gap(6.0).w_full().child(
        text(format!("{title} ({count})"))
            .font(kit.ty.ui_medium)
            .color(kit.c.text_base),
    );
    if count == 0 {
        body = body.child(muted(empty, kit));
    }
    body
}

fn history_header(editor: &Editor, tab: usize, title: &str, count: usize, kit: &Kit) -> El {
    Button::new(format!(
        "{} {title} ({count})",
        if editor.studio.show_history[tab] {
            "▾"
        } else {
            "▸"
        }
    ))
    .variant(ButtonVariant::Ghost)
    .disabled(count == 0)
    .on_press(move |editor: &mut Editor| {
        editor.studio.show_history[tab] = !editor.studio.show_history[tab]
    })
    .build(kit)
}

fn muted(value: impl Into<String>, kit: &Kit) -> El {
    text(value)
        .font(kit.ty.small)
        .color(kit.c.text_muted)
        .wrap()
}
