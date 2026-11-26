use rune::{Key, KeyEvent, TextEdit};

fn wrap(edit: &mut TextEdit, marker: &str) {
    let (a, b) = edit.range();
    let n = marker.len();
    if b - a >= n * 2 && edit.text[a..b].starts_with(marker) && edit.text[a..b].ends_with(marker) {
        let selected = edit.text[a + n..b - n].to_owned();
        edit.insert(&selected);
        edit.set_selection(a, b - n * 2);
    } else if a >= n && edit.text[..a].ends_with(marker) && edit.text[b..].starts_with(marker) {
        let selected = edit.selected().to_owned();
        edit.set_selection(a - n, b + n);
        edit.insert(&selected);
        edit.set_selection(a - n, b - n);
    } else {
        let selected = edit.selected().to_owned();
        edit.insert(&format!("{marker}{selected}{marker}"));
        edit.set_selection(a + n, b + n);
    }
}
fn marker(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let tail = &line[indent..];
    for prefix in ["- ", "* ", "+ ", "> "] {
        if tail.starts_with(prefix) {
            return Some((
                indent + prefix.len(),
                format!("{}{}", " ".repeat(indent), prefix),
            ));
        }
    }
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && tail[digits..].starts_with(". ") {
        let number = tail[..digits].parse::<u64>().ok()?;
        return Some((
            indent + digits + 2,
            format!(
                "{}{next}. ",
                " ".repeat(indent),
                next = number.saturating_add(1)
            ),
        ));
    }
    None
}
pub(crate) fn handle(edit: &mut TextEdit, key: &KeyEvent) -> bool {
    if !key.mods.shift && !key.mods.alt {
        if key.is_command('b') {
            wrap(edit, "**");
            return true;
        }
        if key.is_command('i') {
            wrap(edit, "*");
            return true;
        }
        if key.is_command('k') {
            let (a, _) = edit.range();
            let selected = edit.selected().to_owned();
            edit.insert(&format!("[{selected}](url)"));
            let url = a + selected.len() + 3;
            edit.set_selection(url, url + 3);
            return true;
        }
    }
    if key.mods.cmd || key.mods.ctrl || key.mods.alt {
        return false;
    }
    let (a, b) = edit.range();
    let start = edit.text[..a].rfind('\n').map_or(0, |i| i + 1);
    let end = edit.text[a..].find('\n').map_or(edit.text.len(), |i| a + i);
    let line = &edit.text[start..end];
    let Some((marker_end, next)) = marker(line) else {
        return false;
    };
    if key.key == Key::Enter && !key.mods.shift && a == b {
        if line[marker_end..].trim().is_empty() {
            edit.set_selection(start, end);
            edit.insert("");
        } else {
            edit.insert(&format!("\n{next}"));
        }
        return true;
    }
    if key.key == Key::Tab {
        if key.mods.shift {
            let remove = line.bytes().take_while(|c| *c == b' ').count().min(2);
            if remove > 0 {
                edit.set_selection(start, start + remove);
                edit.insert("");
                edit.set_selection(a.saturating_sub(remove), b.saturating_sub(remove));
            }
        } else {
            edit.set_selection(start, start);
            edit.insert("  ");
            edit.set_selection(a + 2, b + 2);
        }
        return true;
    }
    false
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_unicode_and_unwrapping() {
        let mut e = TextEdit::new("Café");
        e.set_selection(0, 5);
        wrap(&mut e, "**");
        assert_eq!(e.text, "**Café**");
        wrap(&mut e, "**");
        assert_eq!(e.text, "Café");
    }
    #[test]
    fn list_markers() {
        assert_eq!(marker("1. Café").unwrap().1, "2. ");
        assert_eq!(marker("  - hi").unwrap().1, "  - ");
    }
    fn key(key: Key, shift: bool) -> KeyEvent {
        KeyEvent {
            key,
            mods: rune::Mods {
                shift,
                ..Default::default()
            },
        }
    }
    #[test]
    fn lists_continue_exit_and_indent() {
        let mut e = TextEdit::new("1. Café");
        e.set_caret(e.text.len());
        assert!(handle(&mut e, &key(Key::Enter, false)));
        assert_eq!(e.text, "1. Café\n2. ");
        assert!(handle(&mut e, &key(Key::Enter, false)));
        assert_eq!(e.text, "1. Café\n");
        let mut e = TextEdit::new("- こんにちは");
        e.set_caret(e.text.len());
        assert!(handle(&mut e, &key(Key::Tab, false)));
        assert_eq!(e.text, "  - こんにちは");
        assert!(handle(&mut e, &key(Key::Tab, true)));
        assert_eq!(e.text, "- こんにちは");
    }

    #[test]
    fn italic_and_link() {
        let mut e = TextEdit::new("Café");
        e.set_selection(0, 5);
        wrap(&mut e, "*");
        assert_eq!(e.text, "*Café*");
        wrap(&mut e, "*");
        assert_eq!(e.text, "Café");
        let command = KeyEvent {
            key: Key::Command('k'),
            mods: Default::default(),
        };
        assert!(handle(&mut e, &command));
        assert_eq!(e.text, "[Café](url)");
        assert_eq!(e.selected(), "url");
    }
}
