use crate::prefs::{FontChoice, MeasurePreset, ThemeMode, UiPrefs};
use rune::{TextStyle, color::hex};
use rune_kit::Kit;

pub(crate) fn build_kit(mode: ThemeMode, system_dark: bool) -> Kit {
    let mut kit = Kit::neutral(match mode {
        ThemeMode::System => system_dark,
        ThemeMode::Dark => true,
        _ => false,
    });
    if mode == ThemeMode::Paper {
        kit.c.bg_base = hex("#FAF6EE").unwrap();
        kit.c.page = hex("#F4EFE6").unwrap();
        kit.c.bg_deep = hex("#EFE9DD").unwrap();
        kit.c.layer_01 = hex("#F7F2E8").unwrap();
        kit.c.layer_02 = hex("#ECE5D8").unwrap();
        kit.c.text_base = hex("#2B251F").unwrap();
        kit.c.text_muted = hex("#6A6055").unwrap();
        kit.c.text_faint = hex("#9A8F82").unwrap();
        kit.c.border_muted = hex("#E3DACB").unwrap();
        kit.c.border_base = hex("#D5CABB").unwrap();
    }
    kit
}
pub(crate) fn prose_text_style(prefs: &UiPrefs) -> TextStyle {
    let size = prefs.font_size.clamp(13.0, 24.0);
    let style = TextStyle::new(size, 400.0).lh((size * 1.65).round());
    if prefs.font_family == FontChoice::Mono {
        style.mono()
    } else {
        style
    }
}
pub(crate) fn measure_max_width(preset: MeasurePreset) -> Option<f32> {
    match preset {
        MeasurePreset::Narrow => Some(620.0),
        MeasurePreset::Medium => Some(720.0),
        MeasurePreset::Wide => Some(860.0),
        MeasurePreset::Full => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_produce_valid_styles() {
        let prefs = UiPrefs {
            font_size: 99.0,
            ..UiPrefs::default()
        };
        assert_eq!(prose_text_style(&prefs).size, 24.0);
        assert_eq!(measure_max_width(MeasurePreset::Medium), Some(720.0));
        assert_eq!(measure_max_width(MeasurePreset::Full), None);
        let _ = build_kit(ThemeMode::Paper, true);
    }
}
