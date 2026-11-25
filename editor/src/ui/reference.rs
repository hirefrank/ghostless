use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReferenceSource {
    File(PathBuf),
    SessionOriginal,
}

pub(crate) struct ReferencePane {
    pub open: bool,
    pub select_open: bool,
    pub source: Option<ReferenceSource>,
    pub rendered: bool,
    pub cached_title: String,
    pub cached_content: String,
}

impl Default for ReferencePane {
    fn default() -> Self {
        Self {
            open: false,
            select_open: false,
            source: None,
            rendered: true,
            cached_title: String::new(),
            cached_content: String::new(),
        }
    }
}

/// Choose a useful companion without selecting the active file itself.
pub(crate) fn default_source(
    is_session: bool,
    active_path: &Path,
    style_path: Option<&Path>,
) -> Option<ReferenceSource> {
    if is_session {
        return Some(ReferenceSource::SessionOriginal);
    }
    let sibling = active_path.parent().and_then(|dir| {
        ["questions.md", "skeleton.md", "pre-answers.md"]
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path != active_path && path.is_file())
    });
    sibling
        .or_else(|| {
            style_path
                .filter(|path| *path != active_path && path.is_file())
                .map(Path::to_path_buf)
        })
        .map(ReferenceSource::File)
}

pub(crate) fn build(editor: &crate::state::Editor, kit: &rune_kit::Kit, overlays: &mut Vec<rune::El>) -> rune::El {
    use rune::{col, row, text};
    use rune_kit::{Button, ButtonVariant, Select};
    let mut choices: Vec<(String, ReferenceSource)> = Vec::new();
    if editor.active_is_session() { choices.push(("Original (before.md)".into(), ReferenceSource::SessionOriginal)); }
    if let Some(dir) = editor.active_doc_ref().and_then(|d| d.path.parent())
        && let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).filter(|p| p.is_file()).collect();
        paths.sort();
        for path in paths {
            let label = path.display().to_string();
            choices.push((label, ReferenceSource::File(path)));
        }
    }
    if let Some(voice) = &editor.voice_project {
        for name in ["STYLE.md", "AUDIENCE.md"] {
            let path = voice.join(name);
            if path.is_file() && !choices.iter().any(|(_, source)| *source == ReferenceSource::File(path.clone())) {
                choices.push((path.display().to_string(), ReferenceSource::File(path)));
            }
        }
    }
    let labels: Vec<String> = choices.iter().map(|(label, _)| label.clone()).collect();
    let current = choices.iter().find(|(_, source)| Some(source) == editor.reference.source.as_ref()).map(|(label, _)| label.as_str());
    let select = Select::new("reference-picker", &labels).label("Reference file").width(200.0)
        .placeholder("Choose a reference").current(current).open(editor.reference.select_open)
        .on_toggle(|e: &mut crate::state::Editor| e.reference.select_open = !e.reference.select_open)
        .on_close(|e: &mut crate::state::Editor| e.reference.select_open = false)
        .on_choose(move |e: &mut crate::state::Editor, value, _| {
            if let Some((_, source)) = choices.iter().find(|(label, _)| label == value) { e.set_reference(source.clone()); }
            e.reference.select_open = false;
        }).build(kit, overlays);
    let header = row().w_full().h(34.0).gap(4.0).center_y().px(8.0).child(select)
         .child(text(editor.reference.cached_title.clone()).font(kit.ty.small).color(kit.c.text_muted).truncate().shrink())
        .child(row().grow(1.0))
        .child(Button::new(if editor.reference.rendered { "Source" } else { "Preview" }).variant(ButtonVariant::GhostMuted)
            .on_press(|e: &mut crate::state::Editor| e.reference.rendered = !e.reference.rendered).build(kit))
        .child(Button::new("Edit in Tab").variant(ButtonVariant::GhostMuted)
            .disabled(!matches!(editor.reference.source, Some(ReferenceSource::File(_))))
            .on_press(|e: &mut crate::state::Editor| {
                if let Some(ReferenceSource::File(path)) = e.reference.source.clone() { e.open_workspace_file(path, true); }
            }).build(kit))
        .child(Button::new("×").variant(ButtonVariant::GhostMuted).on_press(|e: &mut crate::state::Editor| e.reference.open = false).build(kit));
    let content = if editor.reference.source == Some(ReferenceSource::SessionOriginal) {
        editor.active_doc_ref().and_then(|doc| doc.original.as_deref()).unwrap_or("")
    } else { &editor.reference.cached_content };
    let body = if editor.reference.source.is_none() {
        col().grow(1.0).padding(24.0).child(text("Choose a file to use as a reference").font(kit.ty.ui).color(kit.c.text_muted))
    } else if editor.reference.rendered {
        super::preview::build(content, &editor.store.ui, kit)
    } else {
        col().grow(1.0).w_full().min_h(0.0).scroll(rune::ScrollSpec { stick_bottom: false }).padding(24.0)
            .child(text(content.to_owned()).font(crate::theme::prose_text_style(&editor.store.ui)).color(kit.c.text_base).wrap())
    };
    col().key("reference-pane").w_full().grow(1.0).min_h(0.0).child(header).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn chooses_session_then_siblings_then_style() {
        let dir = std::env::temp_dir().join(format!(
            "ghostless-reference-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let draft = dir.join("draft.md");
        let style = dir.join("STYLE.md");
        let questions = dir.join("questions.md");
        let skeleton = dir.join("skeleton.md");
        let pre = dir.join("pre-answers.md");
        for path in [&style, &questions, &skeleton, &pre] {
            std::fs::write(path, "reference").unwrap();
        }
        assert_eq!(
            default_source(true, &draft, Some(&style)),
            Some(ReferenceSource::SessionOriginal)
        );
        assert_eq!(
            default_source(false, &draft, Some(&style)),
            Some(ReferenceSource::File(questions.clone()))
        );
        assert_eq!(
            default_source(false, &questions, Some(&style)),
            Some(ReferenceSource::File(skeleton.clone()))
        );
        std::fs::remove_file(&questions).unwrap();
        std::fs::remove_file(&skeleton).unwrap();
        assert_eq!(
            default_source(false, &draft, Some(&style)),
            Some(ReferenceSource::File(pre.clone()))
        );
        std::fs::remove_file(&pre).unwrap();
        assert_eq!(
            default_source(false, &draft, Some(&style)),
            Some(ReferenceSource::File(style.clone()))
        );
        assert_eq!(default_source(false, &style, Some(&style)), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
