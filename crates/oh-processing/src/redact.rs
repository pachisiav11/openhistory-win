//! What an episode looks like once it leaves the machine's own memory.
//!
//! Two consumers need the same reduction: the inference layer, which may send an
//! episode to a model, and the MCP server, which serves it to another program. Both
//! must show the same thing, so the reduction lives here once rather than twice.
//!
//! The rules are:
//!
//! - A private episode keeps its application, its times, and nothing else. The
//!   collector recorded nothing else about it and no consumer may invent anything.
//! - A URL keeps its host and path and loses its query string and fragment. Search
//!   terms, session tokens and tracking parameters all live there, and none of them
//!   describe what someone was doing.
//! - Executable paths never leave. They name the machine's layout, and the display
//!   name already says which application it was.
//! - A document leaves by its name and never by its location, for the same reason.
//! - Interface text leaves as the collector redacted and bounded it, cut again to a
//!   handful of lines. It is what made the window recognisable, not what was in it.

use serde::{Deserialize, Serialize};

use crate::episode::Episode;

/// The most URLs to carry on one episode. A browsing session can visit dozens of
/// pages; a summary that lists all of them is a log, not a summary.
pub const MAX_URLS: usize = 8;

/// The most alternative titles to carry on one episode.
pub const MAX_TITLES: usize = 5;

/// The most documents to carry on one episode.
pub const MAX_DOCUMENTS: usize = 5;

/// The most lines of interface text to carry on one episode.
///
/// Smaller than what the episode keeps. A model writing about an hour needs enough to
/// say what was in the window, not a transcript of it, and every line sent is a line
/// the person did not separately agree to.
pub const MAX_VISIBLE_TEXT: usize = 14;

/// An episode reduced to what may be shown outside the application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicEpisode {
    pub id: String,
    pub date: String,
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
    /// Documents worked on, by name. Never by location.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
    /// A few lines of what the window was showing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_text: Vec<String>,
    pub start: String,
    pub end: String,
    pub active_ms: i64,
    pub duration_ms: i64,
    pub is_private: bool,
}

impl PublicEpisode {
    /// A one-line description, safe to put in a prompt or an API response.
    pub fn describe(&self) -> String {
        if self.is_private {
            return format!("{} (private session, nothing recorded)", self.app);
        }
        match &self.title {
            Some(title) => format!("{} — {title}", self.app),
            None => self.app.clone(),
        }
    }
}

impl From<&Episode> for PublicEpisode {
    fn from(episode: &Episode) -> Self {
        if episode.is_private {
            return PublicEpisode {
                id: episode.id.clone(),
                date: episode.date.clone(),
                app: episode.app.clone(),
                title: None,
                titles: Vec::new(),
                urls: Vec::new(),
                documents: Vec::new(),
                visible_text: Vec::new(),
                start: episode.start.clone(),
                end: episode.end.clone(),
                active_ms: episode.active_ms,
                duration_ms: episode.duration_ms,
                is_private: true,
            };
        }

        let mut urls: Vec<String> = Vec::new();
        for url in &episode.urls {
            let trimmed = strip_query(url);
            if !trimmed.is_empty() && !urls.contains(&trimmed) {
                urls.push(trimmed);
            }
            if urls.len() == MAX_URLS {
                break;
            }
        }

        // The representative title is carried separately, so listing it again among
        // the alternatives is noise.
        let titles: Vec<String> = episode
            .titles
            .iter()
            .filter(|title| Some(*title) != episode.title.as_ref())
            .take(MAX_TITLES)
            .cloned()
            .collect();

        PublicEpisode {
            id: episode.id.clone(),
            date: episode.date.clone(),
            app: episode.app.clone(),
            title: episode.title.clone(),
            titles,
            urls,
            // Already labels rather than paths by the time an episode holds them, and
            // already redacted and bounded by the collector. Both are cut again here,
            // because what an episode keeps for its own use is more than what is
            // worth handing to somebody else.
            documents: episode
                .documents
                .iter()
                .take(MAX_DOCUMENTS)
                .cloned()
                .collect(),
            visible_text: episode
                .visible_text
                .iter()
                .take(MAX_VISIBLE_TEXT)
                .cloned()
                .collect(),
            start: episode.start.clone(),
            end: episode.end.clone(),
            active_ms: episode.active_ms,
            duration_ms: episode.duration_ms,
            is_private: false,
        }
    }
}

/// Reduce every episode in a slice.
pub fn public_episodes(episodes: &[Episode]) -> Vec<PublicEpisode> {
    episodes.iter().map(PublicEpisode::from).collect()
}

/// A URL with its query string and fragment removed.
///
/// Parsing is deliberately shallow — find the first `?` or `#` after the scheme and
/// cut. A real URL parser would be a dependency to answer a question that has one
/// correct answer either way, and a string that is not a URL is returned as it was
/// rather than being rejected: the collector reports what the browser displayed, and
/// what the browser displayed is what the user saw.
pub fn strip_query(url: &str) -> String {
    let trimmed = url.trim();
    let cut = trimmed.find(['?', '#']).unwrap_or(trimmed.len());
    trimmed[..cut].trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode() -> Episode {
        Episode {
            id: "2026-08-22#1".into(),
            date: "2026-08-22".into(),
            app: "Google Chrome".into(),
            app_path: Some(r"C:\Users\someone\AppData\Local\Chrome\chrome.exe".into()),
            title: Some("Win32 accessibility — Google Chrome".into()),
            titles: vec![
                "Win32 accessibility — Google Chrome".into(),
                "UIAutomation — Google Chrome".into(),
            ],
            urls: vec![
                "https://learn.microsoft.com/windows/win32/winauto/?search=uia&token=secret".into(),
                "https://learn.microsoft.com/windows/win32/winauto/#overview".into(),
            ],
            documents: Vec::new(),
            visible_text: Vec::new(),
            start: "2026-08-22T09:00:00.000Z".into(),
            end: "2026-08-22T09:30:00.000Z".into(),
            duration_ms: 1_800_000,
            active_ms: 900_000,
            event_count: 12,
            is_private: false,
        }
    }

    #[test]
    fn query_strings_and_fragments_are_cut() {
        assert_eq!(
            strip_query("https://example.com/a/b?q=secret&token=abc"),
            "https://example.com/a/b"
        );
        assert_eq!(
            strip_query("https://example.com/a/b#section"),
            "https://example.com/a/b"
        );
        assert_eq!(strip_query("https://example.com/"), "https://example.com");
        assert_eq!(
            strip_query("  https://example.com/a  "),
            "https://example.com/a"
        );
    }

    #[test]
    fn a_string_that_is_not_a_url_survives_unchanged() {
        assert_eq!(strip_query("about:blank"), "about:blank");
        assert_eq!(strip_query(""), "");
    }

    #[test]
    fn the_executable_path_never_appears_in_the_reduced_form() {
        let public = PublicEpisode::from(&episode());
        let rendered = serde_json::to_string(&public).unwrap();
        assert!(
            !rendered.contains("chrome.exe") && !rendered.contains("AppData"),
            "the executable path leaked: {rendered}"
        );
    }

    #[test]
    fn the_two_urls_reduce_to_one_because_they_differ_only_by_query() {
        let public = PublicEpisode::from(&episode());
        assert_eq!(
            public.urls,
            vec!["https://learn.microsoft.com/windows/win32/winauto".to_string()]
        );
    }

    #[test]
    fn the_representative_title_is_not_repeated_among_the_alternatives() {
        let public = PublicEpisode::from(&episode());
        assert_eq!(
            public.titles,
            vec!["UIAutomation — Google Chrome".to_string()]
        );
    }

    #[test]
    fn a_private_episode_keeps_its_application_and_its_times_and_nothing_else() {
        let mut source = episode();
        source.is_private = true;
        // Even if a title or URL somehow reached the episode, it must not survive.
        let public = PublicEpisode::from(&source);

        assert_eq!(public.app, "Google Chrome");
        assert_eq!(public.active_ms, 900_000);
        assert_eq!(public.title, None);
        assert!(public.titles.is_empty());
        assert!(public.urls.is_empty());
        assert_eq!(
            public.describe(),
            "Google Chrome (private session, nothing recorded)"
        );

        let rendered = serde_json::to_string(&public).unwrap();
        assert!(
            !rendered.contains("accessibility"),
            "a title leaked: {rendered}"
        );
        assert!(!rendered.contains("microsoft"), "a URL leaked: {rendered}");
    }

    #[test]
    fn documents_and_screen_text_are_carried_and_capped() {
        let mut source = episode();
        source.documents = (0..20).map(|n| format!("chapter-{n}.md")).collect();
        source.visible_text = (0..20).map(|n| format!("Heading {n}")).collect();

        let public = PublicEpisode::from(&source);
        assert_eq!(public.documents.len(), MAX_DOCUMENTS);
        assert_eq!(public.documents[0], "chapter-0.md");
        assert_eq!(public.visible_text.len(), MAX_VISIBLE_TEXT);
        assert_eq!(public.visible_text[0], "Heading 0");
    }

    #[test]
    fn a_private_episode_carries_no_document_and_no_screen_text() {
        let mut source = episode();
        source.is_private = true;
        source.documents = vec!["severance-agreement.docx".into()];
        source.visible_text = vec!["Confidential".into()];

        let rendered = serde_json::to_string(&PublicEpisode::from(&source)).unwrap();
        assert!(!rendered.contains("severance"), "{rendered}");
        assert!(!rendered.contains("Confidential"), "{rendered}");
    }

    #[test]
    fn long_url_lists_are_capped() {
        let mut source = episode();
        source.urls = (0..40)
            .map(|n| format!("https://example.com/page/{n}"))
            .collect();

        assert_eq!(PublicEpisode::from(&source).urls.len(), MAX_URLS);
    }
}
