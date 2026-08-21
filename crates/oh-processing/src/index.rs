//! The search index.
//!
//! An inverted index from terms to episodes, held in memory and persisted as one JSON
//! file. Nothing here is clever: at a few hundred episodes a day, a map of terms to
//! identifier sets answers a query in microseconds and can be rebuilt from the event
//! log at any time, which matters more than compactness for an index that is not the
//! source of truth.
//!
//! Private episodes are indexed by nothing but their application, because nothing else
//! about them was ever recorded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::episode::Episode;

/// Bumped when the index format changes. An index at a different version is discarded
/// and rebuilt rather than migrated, because it can always be regenerated.
pub const INDEX_VERSION: u8 = 1;

/// Terms shorter than this are not indexed. One-letter terms match nearly everything
/// and cost more than they are worth.
const MIN_TERM_LENGTH: usize = 2;

/// What a search returns: enough to render a result without opening the day's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedEpisode {
    pub id: String,
    pub date: String,
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub start: String,
    pub end: String,
    pub active_ms: i64,
    pub is_private: bool,
}

impl From<&Episode> for IndexedEpisode {
    fn from(episode: &Episode) -> Self {
        IndexedEpisode {
            id: episode.id.clone(),
            date: episode.date.clone(),
            app: episode.app.clone(),
            title: episode.title.clone(),
            start: episode.start.clone(),
            end: episode.end.clone(),
            active_ms: episode.active_ms,
            is_private: episode.is_private,
        }
    }
}

/// One search result and why it matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    #[serde(flatten)]
    pub episode: IndexedEpisode,
    /// How many of the query's terms this episode matched.
    pub matched_terms: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndex {
    pub version: u8,
    /// Term to the episodes containing it.
    terms: BTreeMap<String, BTreeSet<String>>,
    /// Everything needed to render a hit.
    episodes: BTreeMap<String, IndexedEpisode>,
    /// Which episodes belong to which day, so one day can be reindexed in place.
    days: BTreeMap<String, Vec<String>>,
}

impl SearchIndex {
    pub fn new() -> Self {
        SearchIndex {
            version: INDEX_VERSION,
            ..SearchIndex::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }

    pub fn episode_count(&self) -> usize {
        self.episodes.len()
    }

    pub fn term_count(&self) -> usize {
        self.terms.len()
    }

    pub fn indexed_days(&self) -> Vec<String> {
        self.days.keys().cloned().collect()
    }

    /// Replace everything indexed for one date.
    ///
    /// Reprocessing a day must not leave the previous run's episodes behind, and
    /// episode identifiers are deterministic, so this removes the old entries first
    /// rather than merging.
    pub fn index_day(&mut self, date: &str, episodes: &[Episode]) {
        self.forget_day(date);

        let mut ids = Vec::with_capacity(episodes.len());
        for episode in episodes {
            for term in terms_of(episode) {
                self.terms
                    .entry(term)
                    .or_default()
                    .insert(episode.id.clone());
            }
            self.episodes
                .insert(episode.id.clone(), IndexedEpisode::from(episode));
            ids.push(episode.id.clone());
        }
        self.days.insert(date.to_owned(), ids);
    }

    /// Drop everything indexed for one date.
    pub fn forget_day(&mut self, date: &str) {
        let Some(ids) = self.days.remove(date) else {
            return;
        };

        let dropped: BTreeSet<String> = ids.into_iter().collect();
        for id in &dropped {
            self.episodes.remove(id);
        }
        self.terms.retain(|_, postings| {
            postings.retain(|id| !dropped.contains(id));
            !postings.is_empty()
        });
    }

    /// Episodes matching every term in the query, most recent first.
    ///
    /// All terms must match. Searching "chrome docs" for episodes that mention only
    /// one of the two is not what anyone means by it.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let wanted = tokenize(query);
        if wanted.is_empty() {
            return Vec::new();
        }

        let mut matches: Option<BTreeSet<String>> = None;
        for term in &wanted {
            let postings = self.postings_for(term);
            matches = Some(match matches {
                None => postings,
                Some(current) => current.intersection(&postings).cloned().collect(),
            });
            if matches.as_ref().is_some_and(BTreeSet::is_empty) {
                return Vec::new();
            }
        }

        let mut hits: Vec<SearchHit> = matches
            .unwrap_or_default()
            .iter()
            .filter_map(|id| self.episodes.get(id))
            .map(|episode| SearchHit {
                episode: episode.clone(),
                matched_terms: wanted.len(),
            })
            .collect();

        // Most recent first: a history is nearly always searched for something recent.
        hits.sort_by(|a, b| {
            b.episode
                .start
                .cmp(&a.episode.start)
                .then_with(|| a.episode.id.cmp(&b.episode.id))
        });
        hits.truncate(limit);
        hits
    }

    /// Episodes for one term, including those where it is a prefix.
    ///
    /// Prefix matching is what makes typing feel responsive: "openhis" should find
    /// "openhistory" before the user finishes the word.
    fn postings_for(&self, term: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        for (indexed, postings) in self.terms.range(term.to_owned()..) {
            if !indexed.starts_with(term) {
                break;
            }
            found.extend(postings.iter().cloned());
        }
        found
    }

    /// Load an index from disk, returning an empty one if it is missing, unreadable or
    /// written by a different version.
    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return SearchIndex::new();
        };
        match serde_json::from_str::<SearchIndex>(&text) {
            Ok(index) if index.version == INDEX_VERSION => index,
            Ok(index) => {
                tracing::info!(
                    found = index.version,
                    expected = INDEX_VERSION,
                    "discarding a search index from a different version"
                );
                SearchIndex::new()
            }
            Err(error) => {
                tracing::warn!(%error, "search index could not be read; rebuilding");
                SearchIndex::new()
            }
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            oh_core::paths::ensure_dir(parent)?;
        }
        let text = serde_json::to_string(self).context("could not serialize the search index")?;
        let temporary = path.with_extension("json.writing");
        std::fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }
}

/// Everything about an episode that should be searchable.
fn terms_of(episode: &Episode) -> BTreeSet<String> {
    let mut terms = tokenize(&episode.app).into_iter().collect::<BTreeSet<_>>();
    if episode.is_private {
        // The application is all that was ever recorded. There is nothing else to index.
        return terms;
    }

    for title in &episode.titles {
        terms.extend(tokenize(title));
    }
    for url in &episode.urls {
        terms.extend(tokenize_url(url));
    }
    terms
}

/// Split text into lowercase searchable terms.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= MIN_TERM_LENGTH)
        .map(|word| word.to_lowercase())
        .collect();
    terms.dedup();
    terms
}

/// Terms for a URL: the host and each path segment, but never the query string.
///
/// Query strings carry session tokens, search phrases and identifiers. Indexing them
/// would make those searchable, which is not a thing this application should do.
fn tokenize_url(url: &str) -> Vec<String> {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path_only = without_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(without_scheme);
    tokenize(path_only)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(id: &str, app: &str, title: &str, start: &str) -> Episode {
        Episode {
            id: id.to_owned(),
            date: "2026-08-21".into(),
            app: app.to_owned(),
            app_path: None,
            title: Some(title.to_owned()),
            titles: vec![title.to_owned()],
            urls: Vec::new(),
            start: start.to_owned(),
            end: start.to_owned(),
            duration_ms: 0,
            active_ms: 0,
            event_count: 1,
            is_private: false,
        }
    }

    fn sample() -> SearchIndex {
        let episodes = [
            episode(
                "a",
                "Visual Studio Code",
                "episode.rs - openhistory-win",
                "2026-08-21T09:00:00.000Z",
            ),
            episode(
                "b",
                "Google Chrome",
                "Win32 accessibility - Google Chrome",
                "2026-08-21T10:00:00.000Z",
            ),
            episode(
                "c",
                "Slack",
                "#engineering - Slack",
                "2026-08-21T11:00:00.000Z",
            ),
        ];

        let mut index = SearchIndex::new();
        index.index_day("2026-08-21", &episodes);
        index
    }

    #[test]
    fn finds_an_episode_by_a_word_in_its_title() {
        let hits = sample().search("accessibility", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].episode.id, "b");
    }

    #[test]
    fn finds_an_episode_by_its_application() {
        let hits = sample().search("slack", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].episode.app, "Slack");
    }

    #[test]
    fn every_term_must_match() {
        let index = sample();
        assert_eq!(index.search("google chrome", 10).len(), 1);
        assert!(
            index.search("google slack", 10).is_empty(),
            "an episode matching only one term is not a result"
        );
    }

    #[test]
    fn matching_ignores_case_and_punctuation() {
        let index = sample();
        assert_eq!(index.search("EPISODE.RS", 10).len(), 1);
        assert_eq!(index.search("#engineering", 10).len(), 1);
    }

    #[test]
    fn a_prefix_matches_so_typing_feels_responsive() {
        let index = sample();
        assert_eq!(index.search("openhis", 10).len(), 1);
        assert_eq!(index.search("access", 10).len(), 1);
    }

    #[test]
    fn results_are_most_recent_first() {
        let hits = sample().search("s", 10);
        // "s" is below the minimum term length, so it finds nothing at all.
        assert!(hits.is_empty());

        let index = sample();
        let hits = index.search("2026", 10);
        assert!(hits.is_empty(), "dates are not part of a title here");

        let mut wide = SearchIndex::new();
        wide.index_day(
            "2026-08-21",
            &[
                episode("a", "Code", "notes", "2026-08-21T09:00:00.000Z"),
                episode("b", "Code", "notes", "2026-08-21T15:00:00.000Z"),
            ],
        );
        let hits = wide.search("notes", 10);
        assert_eq!(hits[0].episode.id, "b", "the later episode comes first");
    }

    #[test]
    fn a_limit_is_honoured() {
        let mut index = SearchIndex::new();
        let episodes: Vec<Episode> = (0..20)
            .map(|n| {
                episode(
                    &format!("e{n}"),
                    "Code",
                    "notes",
                    &format!("2026-08-21T{:02}:00:00.000Z", n % 24),
                )
            })
            .collect();
        index.index_day("2026-08-21", &episodes);

        assert_eq!(index.search("notes", 5).len(), 5);
    }

    #[test]
    fn an_empty_query_returns_nothing_rather_than_everything() {
        let index = sample();
        assert!(index.search("", 10).is_empty());
        assert!(index.search("   ", 10).is_empty());
        assert!(index.search("!", 10).is_empty());
    }

    #[test]
    fn reindexing_a_day_replaces_it_rather_than_duplicating_it() {
        let mut index = sample();
        assert_eq!(index.episode_count(), 3);

        index.index_day(
            "2026-08-21",
            &[episode(
                "a",
                "Visual Studio Code",
                "rollup.rs",
                "2026-08-21T09:00:00.000Z",
            )],
        );

        assert_eq!(index.episode_count(), 1);
        assert!(
            index.search("episode", 10).is_empty(),
            "the old terms must be gone"
        );
        assert_eq!(index.search("rollup", 10).len(), 1);
    }

    #[test]
    fn forgetting_a_day_leaves_other_days_intact() {
        let mut index = sample();
        index.index_day(
            "2026-08-22",
            &[episode(
                "d",
                "Firefox",
                "release notes",
                "2026-08-22T09:00:00.000Z",
            )],
        );

        index.forget_day("2026-08-21");
        assert_eq!(index.episode_count(), 1);
        assert_eq!(index.indexed_days(), vec!["2026-08-22".to_string()]);
        assert_eq!(index.search("release", 10).len(), 1);
        assert!(index.search("slack", 10).is_empty());
    }

    #[test]
    fn a_private_episode_is_findable_by_application_and_nothing_else() {
        let mut private = episode(
            "p",
            "Google Chrome",
            "should never be here",
            "2026-08-21T12:00:00.000Z",
        );
        private.is_private = true;
        private.title = None;

        let mut index = SearchIndex::new();
        index.index_day("2026-08-21", &[private]);

        assert_eq!(index.search("chrome", 10).len(), 1);
        assert!(
            index.search("never", 10).is_empty(),
            "a private episode must not be searchable by anything but its application"
        );
    }

    #[test]
    fn a_url_is_indexed_by_host_and_path_but_never_by_its_query() {
        let mut visit = episode("u", "Google Chrome", "Docs", "2026-08-21T12:00:00.000Z");
        visit.urls =
            vec!["https://learn.microsoft.com/windows/win32/?q=secret-phrase&sid=abc123".into()];

        let mut index = SearchIndex::new();
        index.index_day("2026-08-21", &[visit]);

        assert_eq!(index.search("microsoft", 10).len(), 1);
        assert_eq!(index.search("win32", 10).len(), 1);
        assert!(
            index.search("secret", 10).is_empty(),
            "query strings carry search phrases and tokens and must not be indexed"
        );
        assert!(index.search("abc123", 10).is_empty());
    }

    #[test]
    fn an_index_survives_a_save_and_load() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("search-index.json");

        sample().save_to(&path).unwrap();
        let loaded = SearchIndex::load_from(&path);

        assert_eq!(loaded.episode_count(), 3);
        assert_eq!(loaded.search("accessibility", 10).len(), 1);
    }

    #[test]
    fn a_missing_or_unreadable_index_is_rebuilt_rather_than_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("search-index.json");
        assert!(SearchIndex::load_from(&missing).is_empty());

        std::fs::write(&missing, "{ not json").unwrap();
        assert!(SearchIndex::load_from(&missing).is_empty());

        std::fs::write(
            &missing,
            r#"{"version":99,"terms":{},"episodes":{},"days":{}}"#,
        )
        .unwrap();
        let stale = SearchIndex::load_from(&missing);
        assert!(stale.is_empty());
        assert_eq!(stale.version, INDEX_VERSION);
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("search-index.json");
        sample().save_to(&path).unwrap();

        let leftovers: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec!["search-index.json".to_string()]);
    }
}
