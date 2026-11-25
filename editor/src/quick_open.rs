use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{search::match_ranges, workspace_tree::WorkspaceEntry};

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
    pub preview: Arc<str>,
    source_line: Arc<str>,
    byte_column: usize,
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

impl FolderMatch {
    /// Return the match index and whether it was found on the original line.
    /// Identical text elsewhere can be a duplicate rather than a moved hit.
    pub(crate) fn locate(&self, text: &str, matches: &[Range<usize>]) -> Option<(usize, bool)> {
        let mut offset = 0;
        let mut best = None;
        for (line_index, line) in text.split_inclusive('\n').enumerate() {
            if line.trim_end_matches(['\r', '\n']) == self.source_line.as_ref() {
                let start = offset + self.byte_column;
                if let Ok(index) = matches.binary_search_by_key(&start, |range| range.start) {
                    let distance = (line_index + 1).abs_diff(self.line_no);
                    if distance == 0 {
                        return Some((index, true));
                    }
                    if best.is_none_or(|(best_distance, _)| distance < best_distance) {
                        best = Some((distance, index));
                    }
                }
            }
            offset += line.len();
        }
        if let Some((_, index)) = best {
            return Some((index, false));
        }
        matches
            .iter()
            .enumerate()
            .min_by_key(|(_, range)| range.start.abs_diff(self.byte_offset))
            .map(|(index, _)| (index, false))
    }
}

pub(crate) const MAX_SEARCH_FILE_BYTES: usize = 2 * 1024 * 1024;

const SEARCH_READ_CHUNK_BYTES: usize = 32 * 1024;

fn read_search_file(path: &Path, cancelled: &impl Fn() -> bool) -> Option<String> {
    if !fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_SEARCH_FILE_BYTES as u64)
        || cancelled()
    {
        return None;
    }
    read_search_text(fs::File::open(path).ok()?, cancelled)
}

fn read_search_text(reader: impl Read, cancelled: &impl Fn() -> bool) -> Option<String> {
    let mut reader = reader.take(MAX_SEARCH_FILE_BYTES as u64 + 1);
    let mut chunk = [0; SEARCH_READ_CHUNK_BYTES];
    let mut bytes = Vec::new();
    while !cancelled() {
        match reader.read(&mut chunk) {
            Ok(0) => {
                // A cancellation can arrive during the final blocking read.
                return if cancelled() {
                    None
                } else {
                    String::from_utf8(bytes).ok()
                };
            }
            Ok(len) => {
                if bytes.len() + len > MAX_SEARCH_FILE_BYTES || chunk[..len].contains(&0) {
                    return None;
                }
                bytes.extend_from_slice(&chunk[..len]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Search the visible files, using open buffers in place of their disk copies.
pub(crate) fn search_workspace_files(
    files: &[WorkspaceEntry],
    buffers: &HashMap<PathBuf, Option<String>>,
    query: &str,
    max_results: usize,
    cancelled: impl Fn() -> bool,
) -> Vec<FolderMatch> {
    let limit = max_results.min(200);
    if query.chars().count() < 2 || limit == 0 {
        return Vec::new();
    }
    let mut results = Vec::new();
    for file in files {
        if cancelled() {
            break;
        }
        let disk_text;
        let text = match buffers.get(&file.abs_path) {
            Some(Some(text)) => text,
            Some(None) => continue,
            None => {
                let Some(text) = read_search_file(&file.abs_path, &cancelled) else {
                    continue;
                };
                disk_text = text;
                &disk_text
            }
        };
        if text.len() > MAX_SEARCH_FILE_BYTES || text.contains('\0') {
            continue;
        }
        let mut offset = 0;
        for (line_index, line) in text.split_inclusive('\n').enumerate() {
            if cancelled() {
                return results;
            }
            let mut found = match_ranges(line, query, false, false, &cancelled).peekable();
            if found.peek().is_some() {
                let source_line = Arc::<str>::from(line.trim_end_matches(['\r', '\n']));
                let preview = Arc::<str>::from(line.trim().chars().take(240).collect::<String>());
                for range in found {
                    results.push(FolderMatch {
                        rel_path: file.rel_path.clone(),
                        abs_path: file.abs_path.clone(),
                        line_no: line_index + 1,
                        byte_offset: offset + range.start,
                        preview: Arc::clone(&preview),
                        source_line: Arc::clone(&source_line),
                        byte_column: range.start,
                    });
                    if results.len() == limit {
                        return results;
                    }
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
    fn cancelled_file_read_stops_before_the_next_chunk() {
        use std::cell::Cell;

        let reads = Cell::new(0);
        let mut file = io::repeat(b'x').take((SEARCH_READ_CHUNK_BYTES * 3) as u64);
        let reader = ReaderFn(|buffer: &mut [u8]| {
            reads.set(reads.get() + 1);
            file.read(buffer)
        });
        assert!(read_search_text(reader, &|| reads.get() > 0).is_none());
        assert_eq!(reads.get(), 1);
        assert_eq!(file.limit(), (SEARCH_READ_CHUNK_BYTES * 2) as u64);
    }

    struct ReaderFn<F>(F);

    impl<F: FnMut(&mut [u8]) -> io::Result<usize>> Read for ReaderFn<F> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            (self.0)(buffer)
        }
    }

    #[test]
    fn file_read_keeps_utf8_across_chunks_and_rejects_excess_bytes() {
        let source = format!("{}✨", "x".repeat(SEARCH_READ_CHUNK_BYTES - 1));
        assert_eq!(read_search_text(source.as_bytes(), &|| false), Some(source));
        assert!(read_search_text(io::repeat(b'x'), &|| false).is_none());
    }

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
        let matches = search_workspace_files(&entries, &HashMap::new(), "café", 200, || false);
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
        assert_eq!(
            search_workspace_files(&entries, &HashMap::new(), "café", 1, || false).len(),
            1
        );
        assert!(search_workspace_files(&entries, &HashMap::new(), "c", 200, || false).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
