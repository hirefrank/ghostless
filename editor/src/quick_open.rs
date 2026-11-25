use std::{fs, path::PathBuf};

use crate::{search::find_matches, workspace_tree::WorkspaceEntry};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QuickTarget {
    File(PathBuf),
    Session(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuickItem {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub target: QuickTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderMatch {
    pub rel_path: String,
    pub abs_path: PathBuf,
    pub line_no: usize,
    pub byte_offset: usize,
    pub preview: String,
}

/// Score a case-insensitive subsequence. Higher scores favor basename, word starts and runs.
pub(crate) fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    if needle.is_empty() {
        return Some(0);
    }
    let haystack: Vec<char> = candidate.to_lowercase().chars().collect();
    let basename = haystack
        .iter()
        .rposition(|c| *c == '/' || *c == '\\')
        .map_or(0, |i| i + 1);
    let mut at = 0;
    let mut previous = None;
    let mut score = 0;
    for (index, c) in haystack.iter().enumerate() {
        if *c != needle[at] {
            continue;
        }
        score += 10;
        if index >= basename {
            score += 8;
        }
        if index == 0 || matches!(haystack[index - 1], '/' | '\\' | '-' | '_' | ' ') {
            score += 12;
        }
        if previous == index.checked_sub(1) && previous.is_some() {
            score += 10;
        }
        previous = Some(index);
        at += 1;
        if at == needle.len() {
            return Some(score - (haystack.len().min(i32::MAX as usize) as i32 / 16));
        }
    }
    None
}

/// Search visible workspace entries with bounded file and result sizes.
pub(crate) fn search_workspace_files(
    files: &[WorkspaceEntry],
    query: &str,
    max_results: usize,
) -> Vec<FolderMatch> {
    let limit = max_results.min(200);
    if query.chars().count() < 2 || limit == 0 {
        return Vec::new();
    }
    let mut results = Vec::new();
    for file in files {
        if !fs::metadata(&file.abs_path)
            .is_ok_and(|meta| meta.is_file() && meta.len() <= 2 * 1024 * 1024)
        {
            continue;
        }
        let Ok(bytes) = fs::read(&file.abs_path) else {
            continue;
        };
        if bytes.len() > 2 * 1024 * 1024 || bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let mut offset = 0;
        for (line_index, line) in text.split_inclusive('\n').enumerate() {
            for found in find_matches(line, query, false, false) {
                results.push(FolderMatch {
                    rel_path: file.rel_path.clone(),
                    abs_path: file.abs_path.clone(),
                    line_no: line_index + 1,
                    byte_offset: offset + found.start,
                    preview: line.trim().to_owned(),
                });
                if results.len() == limit {
                    return results;
                }
            }
            offset += line.len();
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_tree::FileKind;

    #[test]
    fn fuzzy_favors_basename_and_requires_subsequence() {
        assert!(
            fuzzy_score("ans", "seeds/update-your-story/answers.md")
                > fuzzy_score("ans", "legacy/scripts/ai_tells.py")
        );
        assert_eq!(fuzzy_score("xyz", "answers.md"), None);
        assert_eq!(fuzzy_score("", "answers.md"), Some(0));
    }

    #[test]
    fn searches_nested_utf8_files_and_limits_results() {
        let dir = std::env::temp_dir().join(format!("ghostless-quick-open-{}", std::process::id()));
        fs::create_dir_all(dir.join("nested")).unwrap();
        let a = dir.join("nested/a.md");
        let b = dir.join("b.md");
        fs::write(&a, "✨ first\nCafé and café\n").unwrap();
        fs::write(&b, "CA FÉ\nCAFÉ\n").unwrap();
        let entries: Vec<_> = [(a, "nested/a.md"), (b, "b.md")]
            .into_iter()
            .map(|(abs_path, rel_path)| WorkspaceEntry {
                abs_path,
                rel_path: rel_path.into(),
                kind: FileKind::Writing,
                read_only: false,
            })
            .collect();
        let matches = search_workspace_files(&entries, "café", 200);
        assert_eq!(matches.len(), 3);
        assert_eq!(
            (matches[0].line_no, matches[0].byte_offset),
            (2, "✨ first\n".len())
        );
        assert_eq!(matches[2].line_no, 2);
        for hit in &matches {
            let text = fs::read_to_string(&hit.abs_path).unwrap();
            assert!(text.is_char_boundary(hit.byte_offset));
        }
        assert_eq!(search_workspace_files(&entries, "café", 1).len(), 1);
        assert!(search_workspace_files(&entries, "c", 200).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
