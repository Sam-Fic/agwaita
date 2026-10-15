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

        let mut collect = |pred: &dyn Fn(&Candidate) -> bool| -> Vec<DesktopEntry> {
            let mut list: Vec<Candidate> = candidates
                .iter()
                .filter(|c| pred(c) && seen_ids.insert(c.entry.id.as_str()))
                .cloned()
                .collect();
            list.sort_by(|a, b| a.name_norm.cmp(&b.name_norm));
            list.into_iter().map(|c| c.entry).collect()
        };

        let mut results = Vec::new();
        results.extend(collect(&|c| c.name_norm.contains(&query_normalized)));
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

        // Stable partition favorites first (no per-compare allocation).
        results.sort_by(|a, b| match (a.is_favorite, b.is_favorite) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        });

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

impl Default for AppSearcher {
    fn default() -> Self {
        Self::new()
    }
}
