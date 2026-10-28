use crate::model::DesktopEntry;
use nucleo_matcher::chars::normalize;
use std::{cell::RefCell, collections::HashSet};

/// One desktop entry plus its precomputed normalized search fields.
///
/// Normalization used to run on every keystroke for every entry (four
/// strings per entry, each `to_lowercase` + per-char mapping + allocation).
/// The entry list only changes when the desktop scanner rescans, so all of
/// that work now happens once in [`AppSearcher::set_entries`]; typing only
/// compares pre-normalized substrings.
#[derive(Clone)]
struct SearchEntry {
    entry: DesktopEntry,
    name_norm: String,
    generic_norm: Option<String>,
    categories_norm: Vec<String>,
    comment_norm: Option<String>,
}

impl SearchEntry {
    fn new(entry: DesktopEntry) -> Self {
        Self {
            name_norm: normalize_str(&entry.name),
            generic_norm: entry.generic_name.as_deref().map(normalize_str),
            categories_norm: entry.categories.iter().map(|c| normalize_str(c)).collect(),
            comment_norm: entry.comment.as_deref().map(normalize_str),
            entry,
        }
    }
}

/// Searcher for desktop entries with prioritized filtering
pub struct AppSearcher {
    entries: RefCell<Vec<SearchEntry>>,
}

impl AppSearcher {
    pub fn new() -> Self {
        Self {
            entries: RefCell::new(Vec::new()),
        }
    }

    pub fn set_entries(&self, entries: Vec<DesktopEntry>) {
        *self.entries.borrow_mut() = entries.into_iter().map(SearchEntry::new).collect();
    }

    /// Search with priority: name > generic_name > categories > comment
    pub fn search(&self, query: &str) -> Vec<DesktopEntry> {
        let all = self.entries.borrow();
        if query.is_empty() {
            return all.iter().map(|c| c.entry.clone()).collect();
        }

        let query_normalized = normalize_str(query);

        let mut seen_ids: HashSet<&str> = HashSet::new();
        let mut results = Vec::new();

        // Name matches are ranked by match quality — exact > prefix >
        // word-start > contains — so typing "setting" puts "Settings"
        // above "Qt6 Settings" even though the latter sorts earlier.
        {
            let mut named: Vec<&SearchEntry> = all
                .iter()
                .filter(|c| {
                    c.name_norm.contains(&query_normalized)
                        && seen_ids.insert(c.entry.id.as_str())
                })
                .collect();
            named.sort_by(|a, b| {
                name_tier(&a.name_norm, &query_normalized)
                    .cmp(&name_tier(&b.name_norm, &query_normalized))
                    .then(b.entry.is_favorite.cmp(&a.entry.is_favorite))
                    .then(a.name_norm.cmp(&b.name_norm))
            });
            results.extend(named.into_iter().map(|c| c.entry.clone()));
        }

        let mut collect = |pred: &dyn Fn(&SearchEntry) -> bool| -> Vec<DesktopEntry> {
            let mut list: Vec<&SearchEntry> = all
                .iter()
                .filter(|c| pred(c) && seen_ids.insert(c.entry.id.as_str()))
                .collect();
            // Favorites break ties within a bucket, but never above a
            // better-matching field.
            list.sort_by(|a, b| {
                b.entry
                    .is_favorite
                    .cmp(&a.entry.is_favorite)
                    .then(a.name_norm.cmp(&b.name_norm))
            });
            list.into_iter().map(|c| c.entry.clone()).collect()
        };

        results.extend(collect(&|c| {
            c.generic_norm
                .as_ref()
                .is_some_and(|s| s.contains(&query_normalized))
        }));
        results.extend(collect(&|c| {
            c.categories_norm
                .iter()
                .any(|s| s.contains(&query_normalized))
        }));
        results.extend(collect(&|c| {
            c.comment_norm
                .as_ref()
                .is_some_and(|s| s.contains(&query_normalized))
        }));

        results
    }
}

fn normalize_str(string: &str) -> String {
    string
        .to_lowercase()
        .chars()
        .map(|c| normalize(c))
        .collect::<String>()
}

/// Match-quality tier of a normalized name against a normalized query:
/// 0 = exact, 1 = prefix, 2 = word-start, 3 = contains anywhere.
/// Only called for names that already contain the query.
fn name_tier(name_norm: &str, query: &str) -> u8 {
    if name_norm == query {
        0
    } else if name_norm.starts_with(query) {
        1
    } else if name_norm.split_whitespace().any(|w| w.starts_with(query)) {
        2
    } else {
        3
    }
}

impl Default for AppSearcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> DesktopEntry {
        DesktopEntry {
            id: id.to_string(),
            name: name.to_string(),
            generic_name: None,
            comment: None,
            icon: None,
            exec: String::new(),
            categories: Vec::new(),
            path: std::path::PathBuf::new(),
            is_favorite: false,
            no_display: false,
            terminal: false,
            rev: 0,
        }
    }

    #[test]
    fn name_matches_outrank_other_fields() {
        let searcher = AppSearcher::new();
        let mut by_comment = entry("comment.desktop", "Unrelated Tool");
        by_comment.comment = Some("Settings dialog for the system".to_string());
        searcher.set_entries(vec![by_comment, entry("settings.desktop", "Settings")]);

        let results = searcher.search("settings");
        assert_eq!(results[0].id, "settings.desktop");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn name_tiers_rank_exact_prefix_and_substring() {
        let searcher = AppSearcher::new();
        searcher.set_entries(vec![
            entry("sub.desktop", "Qt6 Settings"),
            entry("exact.desktop", "settings"),
            entry("prefix.desktop", "Settings Editor"),
        ]);

        let results = searcher.search("settings");
        let ids: Vec<_> = results.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["exact.desktop", "prefix.desktop", "sub.desktop"]);
    }

    #[test]
    fn favorites_break_ties_within_a_bucket() {
        let searcher = AppSearcher::new();
        let mut favorite = entry("fav.desktop", "Zeta Settings");
        favorite.is_favorite = true;
        searcher.set_entries(vec![entry("plain.desktop", "Alpha Settings"), favorite]);

        let results = searcher.search("settings");
        assert_eq!(results[0].id, "fav.desktop");
    }

    #[test]
    fn empty_query_returns_entries_in_order() {
        let searcher = AppSearcher::new();
        searcher.set_entries(vec![entry("b.desktop", "B"), entry("a.desktop", "A")]);

        let results = searcher.search("");
        let ids: Vec<_> = results.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids, ["b.desktop", "a.desktop"]);
    }
}
