use crate::{
    bridge::{BannedHit, ConfirmedRuleHit, JudgeReport, StructureHit},
    fs_io::is_contained_in,
    hunks::{diff_hunks, revert_hunk_text},
    markdown_decor::{compute_markdown_spans, non_prose_ranges},
    session::valid_session_id,
    test_util::{Seeded, TestDir},
    voice_lens,
};
use std::fs;

const SEEDS: [u64; 3] = [1, 0x92c4_051b, 0xfe01_ddac];

#[test]
fn existing_path_segments_follow_a_containment_model() {
    let tmp = TestDir::new("containment-property");
    let root = tmp.join("voice");
    for name in ["voice", "voice-other"] {
        fs::create_dir_all(tmp.join(name).join("é").join("drafts")).unwrap();
    }
    for seed in SEEDS {
        let mut rng = Seeded::new(seed);
        for case in 0..128 {
            let mut path = root.clone();
            let mut model = vec!["voice"];
            for _ in 0..rng.index(18) {
                match rng.index(3) {
                    0 => path.push("."),
                    1 if !model.is_empty() => {
                        path.push("..");
                        model.pop();
                    }
                    _ => {
                        let child = match model.len() {
                            0 => rng.pick(&["voice", "voice-other"]),
                            1 => &"é",
                            2 => &"drafts",
                            _ => continue,
                        };
                        path.push(child);
                        model.push(child);
                    }
                }
            }
            path.push("new.md");
            assert_eq!(
                is_contained_in(&root, &path),
                model.first() == Some(&"voice"),
                "seed {seed:#x}, case {case}, {path:?}"
            );
        }
    }
}

#[test]
fn generated_session_ids_accept_only_ascii_date_and_slug_segments() {
    for seed in SEEDS {
        let mut rng = Seeded::new(seed);
        for case in 0..128 {
            let mut id = format!(
                "{:04}-{:02}-{:02}",
                rng.index(10_000),
                rng.index(100),
                rng.index(100)
            );
            for _ in 0..rng.index(5) {
                id.push('-');
                for _ in 0..=rng.index(12) {
                    id.push(*rng.pick(&['a', 'z', '0', '9']));
                }
            }
            assert!(valid_session_id(&id), "seed {seed:#x}, case {case}, {id:?}");
            let place = rng.index(id.len() + 1);
            let bad = rng.pick(&['É', '大', '０', '/', '\\', '\n', 'A', '_', '\0']);
            id.insert(place, *bad);
            assert!(
                !valid_session_id(&id),
                "seed {seed:#x}, case {case}, {id:?}"
            );
        }
    }
}

fn markdown(rng: &mut Seeded) -> String {
    let mut text = String::new();
    if rng.index(3) == 0 {
        text.push_str("---\r\nsection: Café\r\nsummary: delve\r\n---\r\n");
    }
    for _ in 0..rng.index(26) {
        text.push_str(rng.pick(&[
            "# 🦀日本語",
            "**élève**",
            "[WRITER: café]",
            "`delve`",
            "```\ndelve\n```\n",
            "\r\n",
            "\n",
            " delve ",
            " ÉLAN ",
            " ’— \u{0301}",
            "<!-- ÉLAN -->",
        ]));
        if let Some(ch) = char::from_u32(rng.index(0x110000) as u32) {
            text.push(ch);
        }
    }
    text
}

#[test]
fn generated_decoration_spans_and_hunks_preserve_utf8_and_original_bytes() {
    let kit = rune_kit::Kit::neutral(false);
    let report = JudgeReport {
        banned: vec![BannedHit {
            tell: "delve".into(),
            count: 1,
        }],
        confirmed_rules: vec![ConfirmedRuleHit {
            id: "cut:élan".into(),
            word: "élan".into(),
            scope: None,
            count: 1,
        }],
        structure: vec![StructureHit {
            kind: "contrast".into(),
            excerpt: "élève".into(),
        }],
        ..Default::default()
    };
    for seed in SEEDS {
        let mut rng = Seeded::new(seed);
        for case in 0..128 {
            let original = markdown(&mut rng);
            let mut draft = markdown(&mut rng);
            let context = format!("seed {seed:#x}, case {case}, {original:?}, {draft:?}");
            let spans = compute_markdown_spans(&draft, 16.5, None, &kit)
                .into_iter()
                .chain(voice_lens::spans(&draft, &report, &kit));
            let offsets = spans
                .map(|span| span.start as usize..span.end as usize)
                .chain(non_prose_ranges(&draft));
            for range in offsets {
                assert!(draft.get(range).is_some(), "{context}");
            }

            // Recompute between reverts because byte offsets and neighboring
            // hunks can change when one hunk is replaced.
            let limit = original.lines().count() + draft.lines().count();
            for _ in 0..limit {
                if draft == original {
                    break;
                }
                for hunk in diff_hunks(&original, &draft) {
                    assert!(original.get(hunk.before_bytes).is_some(), "{context}");
                    assert!(draft.get(hunk.after_bytes).is_some(), "{context}");
                }
                draft = revert_hunk_text(&original, &draft, 0)
                    .unwrap_or_else(|| panic!("no hunk for {context}"));
            }
            assert_eq!(draft.as_bytes(), original.as_bytes(), "{context}");
        }
    }
}
