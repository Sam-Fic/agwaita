use crate::model::DesktopEntry;
use nucleo_matcher::chars::normalize;
use std::{cell::RefCell, collections::HashSet};

/// Searcher for desktop entries with prioritized filtering
pub struct AppSearcher {
    all_entries: RefCell<Vec<DesktopEntry>>,
}

impl AppSearcher {
    pub fn new() -> Self {
        Self {
            all_entries: RefCell::new(Vec::new()),
        }
    }

    pub fn set_entries(&self, entries: Vec<DesktopEntry>) {
        *self.all_entries.borrow_mut() = entries;
    }

    /// Search with priority: name > generic_name > categories > comment
    pub fn search(&self, query: &str) -> Vec<DesktopEntry> {
        let all = self.all_entries.borrow();
        if query.is_empty() {
            return all.clone();
        }

        let query_normalized = self.normalize_str(query);

        // Precompute normalized strings once per entry; the old code called
        // normalize_str inside every sort comparator (O(n log n) allocs)
        // and rebuilt 4 full clones of the entry list per keystroke.
        #[derive(Clone)]
        struct Candidate {
            entry: DesktopEntry,
            name_norm: String,
            generic_norm: Option<String>,
            categories_norm: Vec<String>,
            comment_norm: Option<String>,
        }

        let candidates: Vec<Candidate> = all
            .iter()
            .map(|entry| Candidate {
                entry: entry.clone(),
                name_norm: self.normalize_str(&entry.name),
                generic_norm: entry.generic_name.as_ref().map(|s| self.normalize_str(s)),
                categories_norm: entry
                    .categories
                    .iter()
                    .map(|c| self.normalize_str(c))
                    .collect(),
                comment_norm: entry.comment.as_ref().map(|s| self.normalize_str(s)),
            })
            .collect();

        let mut seen_ids: HashSet<&str> = HashSet::new();
        let mut results = Vec::new();

        // Name matches are ranked by match quality — exact > prefix >
        // word-start > contains — so typing "setting" puts "Settings"
        // above "Qt6 Settings" even though the latter sorts earlier.
        {
            let mut named: Vec<Candidate> = candidates
                .iter()
                .filter(|c| {
                    c.name_norm.contains(&query_normalized)
                        && seen_ids.insert(c.entry.id.as_str())
                })
                .cloned()
                .collect();
            named.sort_by(|a, b| {
                name_tier(&a.name_norm, &query_normalized)
                    .cmp(&name_tier(&b.name_norm, &query_normalized))
                    .then(
                        b.entry
                            .is_favorite
                            .cmp(&a.entry.is_favorite),
                    )
                    .then(a.name_norm.cmp(&b.name_norm))
            });
            results.extend(named.into_iter().map(|c| c.entry));
        }

        let mut collect = |pred: &dyn Fn(&Candidate) -> bool| -> Vec<DesktopEntry> {
            let mut list: Vec<Candidate> = candidates
                .iter()
                .filter(|c| pred(c) && seen_ids.insert(c.entry.id.as_str()))
                .cloned()
                .collect();
            // Favorites break ties within a bucket, but never above a
            // better-matching field.
            list.sort_by(|a, b| {
                b.entry
                    .is_favorite
                    .cmp(&a.entry.is_favorite)
                    .then(a.name_norm.cmp(&b.name_norm))
            });
            list.into_iter().map(|c| c.entry).collect()
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

    fn normalize_str(&self, string: &str) -> String {
        string
            .to_lowercase()
            .chars()
            .map(|c| normalize(c))
            .collect::<String>()
    }
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
