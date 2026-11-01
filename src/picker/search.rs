//! Search: fuzzy match on previews, then the storage's full-content (FTS5)
//! search for entries whose preview didn't match.

use super::data::PickerData;
use crate::storage::EntryId;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::collections::{HashMap, HashSet};

/// Where the search match was found
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MatchLocation {
    Preview, // Match found in preview (fast path)
    Content, // Match found in full content (FTS search)
}

/// Entry with search metadata for filtered results
#[derive(Debug)]
pub struct FilteredEntry {
    pub index: usize,                  // Index into original entries list
    pub score: i64,                    // Fuzzy match score (higher = better)
    pub match_location: MatchLocation, // Where match was found
}

/// The current filter's results.
#[derive(Default)]
pub struct SearchState {
    /// Indices into `PickerData::entries`, in display order
    pub filtered: Vec<usize>,
    /// Search results with match metadata (empty without a query)
    pub results: Vec<FilteredEntry>,
    pub matcher: SkimMatcherV2,
}

/// Two-phase search: fuzzy match on previews, then the storage's
/// full-content search for entries whose preview didn't match.
pub fn filter_entries(data: &PickerData, matcher: &SkimMatcherV2, query: &str) -> Vec<FilteredEntry> {
    /// Most full-content hits taken from storage per query
    const MAX_CONTENT_RESULTS: usize = 200;

    let mut results: Vec<FilteredEntry> = Vec::new();

    // Phase 1: Search previews (always available, fast)
    for (idx, entry) in data.entries.iter().enumerate() {
        if let Some(score) = matcher.fuzzy_match(&entry.preview, query) {
            results.push(FilteredEntry {
                index: idx,
                score,
                match_location: MatchLocation::Preview,
            });
        }
    }

    // Phase 2: full-content search (substring per token, case-insensitive)
    // for entries whose preview didn't match. They rank after preview hits.
    let preview_matched: HashSet<usize> = results.iter().map(|r| r.index).collect();
    let index_of: HashMap<EntryId, usize> =
        data.entries.iter().enumerate().map(|(idx, e)| (e.id, idx)).collect();
    let hits = data.storage.search(query, MAX_CONTENT_RESULTS).unwrap_or_default();
    for hit in hits {
        if let Some(&idx) = index_of.get(&hit.id)
            && !preview_matched.contains(&idx)
        {
            results.push(FilteredEntry {
                index: idx,
                score: 0,
                match_location: MatchLocation::Content,
            });
        }
    }

    // Sort by score descending (best matches first)
    results.sort_by_key(|r| std::cmp::Reverse(r.score));
    results
}

/// Highlight matched characters in preview text
pub fn highlight_matches(matcher: &SkimMatcherV2, text: &str, query: &str) -> Vec<Span<'static>> {
    // Get match indices from fuzzy matcher
    if let Some(indices) = matcher.fuzzy_indices(text, query) {
        let (_, positions) = indices;
        let mut spans = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let mut last_pos = 0;

        for &pos in &positions {
            if pos > last_pos {
                // Non-matched portion
                let segment: String = chars[last_pos..pos].iter().collect();
                spans.push(Span::raw(segment));
            }
            // Matched character
            let matched: String = chars[pos..=pos].iter().collect();
            spans.push(Span::styled(
                matched,
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
            last_pos = pos + 1;
        }

        // Remaining non-matched portion
        if last_pos < chars.len() {
            let segment: String = chars[last_pos..].iter().collect();
            spans.push(Span::raw(segment));
        }

        spans
    } else {
        vec![Span::raw(text.to_string())]
    }
}

#[cfg(test)]
mod tests {
    use super::super::Picker;
    use super::super::tests::create_test_storage;
    use super::*;

    // ======== FilteredEntry Type Tests ========

    #[test]
    fn test_filtered_entry_creation() {
        let fe = FilteredEntry {
            index: 5,
            score: 100,
            match_location: MatchLocation::Preview,
        };
        assert_eq!(fe.index, 5);
        assert_eq!(fe.score, 100);
        assert_eq!(fe.match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_match_location_equality() {
        assert_eq!(MatchLocation::Preview, MatchLocation::Preview);
        assert_eq!(MatchLocation::Content, MatchLocation::Content);
        assert_ne!(MatchLocation::Preview, MatchLocation::Content);
    }

    // ======== Search Algorithm Tests ========

    #[test]
    fn test_preview_match_found() {
        // Entry with "hello world" preview should match "hello"
        let (_temp, storage) = create_test_storage(&["hello world content here"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_location, MatchLocation::Preview);
    }

    #[test]
    fn test_content_search_finds_non_preview_match() {
        // Create entry with content longer than preview limit (100 chars)
        // where the search term is only in the non-preview portion
        let long_content = format!(
            "{}\nTHIS_UNIQUE_KEYWORD_NOT_IN_PREVIEW",
            "x".repeat(150) // Preview is only 100 chars
        );
        let (_temp, storage) = create_test_storage(&[&long_content]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("THIS_UNIQUE_KEYWORD");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_location, MatchLocation::Content);
    }

    #[test]
    fn test_search_results_sorted_by_score() {
        // Multiple matches with different relevance
        let (_temp, storage) = create_test_storage(&[
            "hello", // Short, high relevance for "hello"
            "hello world is a greeting phrase hello", // More matches
            "say hello to everyone",
        ]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 3);

        // Verify sorted by score descending
        for i in 1..results.len() {
            assert!(results[i - 1].score >= results[i].score);
        }
    }

    #[test]
    fn test_empty_query_returns_all_via_update_filter() {
        let (_temp, storage) = create_test_storage(&["one", "two", "three"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.query = "".to_string();
        picker.update_filter();

        // All entries should be in filtered list
        assert_eq!(picker.search.filtered.len(), 3);
    }

    #[test]
    fn test_no_matches_returns_empty() {
        let (_temp, storage) = create_test_storage(&["apple", "banana", "cherry"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("xyz_nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn test_preview_match_not_duplicated() {
        // Entry where search term appears in both preview and content
        // Should only appear once with Preview location (not searched twice)
        let (_temp, storage) = create_test_storage(&["hello world"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].match_location, MatchLocation::Preview);
    }

    // ======== Search UI State Tests ========

    #[test]
    fn test_get_match_location_with_search() {
        let (_temp, storage) = create_test_storage(&["apple", "banana"]);
        let mut picker = Picker::new(storage).unwrap();

        picker.view.query = "apple".to_string();
        picker.update_filter();

        // Should have match location for first result
        let location = picker.get_match_location(0);
        assert!(location.is_some());
        assert_eq!(location.unwrap(), MatchLocation::Preview);
    }

    #[test]
    fn test_get_match_location_without_search() {
        let (_temp, storage) = create_test_storage(&["apple", "banana"]);
        let picker = Picker::new(storage).unwrap();

        // No search query = no match location
        let location = picker.get_match_location(0);
        assert!(location.is_none());
    }

    #[test]
    fn test_selection_resets_when_filter_shrinks() {
        let (_temp, storage) = create_test_storage(&["apple", "apricot", "banana", "cherry"]);
        let mut picker = Picker::new(storage).unwrap();

        // Select last item
        picker.view.selected.select(Some(3));

        // Search for "ap" - only 2 results
        picker.view.query = "ap".to_string();
        picker.update_filter();

        // Selection should reset if out of bounds
        let selected = picker.view.selected.selected().unwrap_or(0);
        assert!(selected < picker.search.filtered.len());
    }

    #[test]
    fn test_fuzzy_matching_works() {
        // Test that fuzzy matching finds partial matches
        let (_temp, storage) = create_test_storage(&["hello_world_function"]);
        let picker = Picker::new(storage).unwrap();

        // "hef" should fuzzy match "hello_world_function" (h-e-llo_world_f-unction)
        let results = picker.filter_entries("hwf");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_case_insensitive_search() {
        let (_temp, storage) = create_test_storage(&["Hello World", "HELLO", "hello"]);
        let picker = Picker::new(storage).unwrap();

        let results = picker.filter_entries("hello");
        assert_eq!(results.len(), 3);
    }
}
