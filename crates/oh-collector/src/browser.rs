//! Recognising browsers and their private windows.
//!
//! Private-mode detection reads the window title, because no browser exposes an
//! "am I incognito" flag to an outside process. The heuristic is deliberately biased
//! towards false positives: a page whose own title contains the word "Incognito"
//! will be treated as private and therefore not recorded. Losing one title is the
//! cheap mistake; recording a private session is the expensive one.

/// Browsers whose address bar and private-mode markers we know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Edge,
    Firefox,
    Brave,
    Opera,
    Vivaldi,
    Arc,
}

impl Browser {
    /// Identify a browser from its executable file stem, case-insensitively.
    pub fn from_exe_stem(stem: &str) -> Option<Browser> {
        match stem.to_ascii_lowercase().as_str() {
            "chrome" => Some(Browser::Chrome),
            "msedge" => Some(Browser::Edge),
            "firefox" => Some(Browser::Firefox),
            "brave" => Some(Browser::Brave),
            "opera" | "opera_gx" => Some(Browser::Opera),
            "vivaldi" => Some(Browser::Vivaldi),
            "arc" => Some(Browser::Arc),
            _ => None,
        }
    }

    /// Markers that appear in the window title of a private window.
    fn private_markers(self) -> &'static [&'static str] {
        match self {
            Browser::Chrome => &["incognito"],
            Browser::Edge => &["inprivate"],
            Browser::Firefox => &["private browsing"],
            Browser::Brave => &["private window", "private browsing", "tor"],
            Browser::Opera => &["private"],
            Browser::Vivaldi => &["private window"],
            Browser::Arc => &["incognito"],
        }
    }

    /// True when the title indicates a private or incognito window.
    pub fn title_indicates_private(self, title: &str) -> bool {
        let lowered = title.to_ascii_lowercase();
        self.private_markers().iter().any(|m| lowered.contains(m))
    }

    /// Accessible names the address bar is known by, lowercased for comparison.
    ///
    /// Chromium-family browsers all inherit the same omnibox name; Firefox differs
    /// and changes wording between locales, so the substrings are kept loose.
    pub fn address_bar_names(self) -> &'static [&'static str] {
        match self {
            Browser::Firefox => &["search with", "enter address", "address bar"],
            _ => &["address and search bar", "address bar", "search or enter"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_browsers_by_executable() {
        assert_eq!(Browser::from_exe_stem("chrome"), Some(Browser::Chrome));
        assert_eq!(Browser::from_exe_stem("CHROME"), Some(Browser::Chrome));
        assert_eq!(Browser::from_exe_stem("msedge"), Some(Browser::Edge));
        assert_eq!(Browser::from_exe_stem("firefox"), Some(Browser::Firefox));
        assert_eq!(Browser::from_exe_stem("Code"), None);
        assert_eq!(Browser::from_exe_stem("explorer"), None);
    }

    #[test]
    fn detects_private_windows_from_real_title_formats() {
        let cases = [
            (Browser::Chrome, "Gmail - Google Chrome - Incognito", true),
            (Browser::Chrome, "Gmail - Google Chrome (Incognito)", true),
            (Browser::Chrome, "Gmail - Google Chrome", false),
            (
                Browser::Edge,
                "News - InPrivate - Microsoft\u{200b} Edge",
                true,
            ),
            (Browser::Edge, "News - Microsoft\u{200b} Edge", false),
            (
                Browser::Firefox,
                "Docs \u{2014} Mozilla Firefox Private Browsing",
                true,
            ),
            (Browser::Firefox, "Docs - Mozilla Firefox", false),
            (Browser::Brave, "Search - Private Window - Brave", true),
            (Browser::Brave, "Search - Brave", false),
            (Browser::Vivaldi, "Home - Private Window - Vivaldi", true),
        ];

        for (browser, title, expected) in cases {
            assert_eq!(
                browser.title_indicates_private(title),
                expected,
                "{browser:?} misread {title:?}"
            );
        }
    }

    #[test]
    fn private_detection_errs_towards_privacy() {
        // A page genuinely about incognito mode is treated as private. Recording
        // nothing here is the intended trade.
        assert!(Browser::Chrome.title_indicates_private("How to use Incognito - Google Chrome"));
    }
}
