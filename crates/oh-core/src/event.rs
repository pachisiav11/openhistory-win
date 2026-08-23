//! The `ActivityEvent` schema.
//!
//! This is a frozen contract. The JSON written here is byte-compatible with the
//! macOS build of OpenHistory, so history files move between the two without
//! conversion. Field names are camelCase, absent data is omitted rather than
//! serialized as null, and `version` is always 1.
//!
//! Changing a field name or its presence rules is a breaking change to every
//! recorded file on disk. `tests::schema_shape_is_frozen` exists to make that
//! impossible to do by accident.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Schema version stamped into every event.
pub const SCHEMA_VERSION: u8 = 1;

/// Every kind of observation the collector can report.
///
/// Not all of these are produced on Windows. See `docs/ARCHITECTURE.md`, AD-2, for
/// which are emitted today and why the rest are deferred; they are defined here so
/// that files written by the macOS build deserialize without loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    CollectorStarted,
    ApplicationActivated,
    WindowChanged,
    FocusedElementChanged,
    SelectionChanged,
    TextInput,
    DocumentChanged,
    PointerClick,
    UrlChanged,
    DocumentContextChanged,
    UiSnapshot,
    ApplicationTerminated,
    ScreenSlept,
    ScreenWoke,
    SessionLocked,
    SessionUnlocked,
    PrivacyBoundary,
}

impl EventKind {
    /// True for the kinds the Windows collector actually produces.
    pub fn emitted_on_windows(self) -> bool {
        matches!(
            self,
            EventKind::CollectorStarted
                | EventKind::ApplicationActivated
                | EventKind::WindowChanged
                | EventKind::UrlChanged
                | EventKind::ApplicationTerminated
                | EventKind::ScreenSlept
                | EventKind::ScreenWoke
                | EventKind::SessionLocked
                | EventKind::SessionUnlocked
                | EventKind::PrivacyBoundary
        )
    }
}

/// The application that owned the foreground window when the event fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationDescriptor {
    /// Display name, taken from the executable's version resource where available
    /// and from the file stem otherwise.
    pub name: String,
    /// Full path to the executable.
    pub path: String,
    pub pid: u32,
    /// Always absent on Windows; present in files written by the macOS build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
}

/// What the collector could learn about a browser's current tab.
///
/// When `is_private` is true the URL is never populated, whatever the accessibility
/// tree reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub is_private: bool,
}

/// A single node from the accessibility tree.
///
/// Reserved for the deferred element-level event kinds. Nothing on Windows populates
/// this yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticElement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Set when the element is a password field. Such elements are never read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_sensitive: Option<bool>,
}

/// An edit made to a text field. Reserved; not produced on Windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inserted: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<String>,
}

/// The document open in the foreground window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl DocumentObservation {
    /// What a person would call this document.
    ///
    /// The title when the application published one, and otherwise the last segment
    /// of the path: `C:\work\budget-2026.xlsx` is a location, `budget-2026.xlsx` is
    /// what they were working on. Only the label is ever shown, summarized or sent —
    /// the path names the machine's layout and stays in the event log.
    pub fn label(&self) -> Option<String> {
        if let Some(title) = self
            .title
            .as_ref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            return Some(title.to_owned());
        }
        let path = self.path.as_ref()?.trim().trim_end_matches(['/', '\\']);
        let name = path.rsplit(['/', '\\']).next()?.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    pub fn is_empty(&self) -> bool {
        self.path.is_none() && self.title.is_none()
    }
}

/// One observation of what the user was doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub version: u8,
    /// UUID v4.
    pub id: String,
    /// ISO 8601 with millisecond precision, always UTC.
    pub timestamp: String,
    pub kind: EventKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<ApplicationDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessibility_trusted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer_capture_available: Option<bool>,

    /// Set when the event was observed while a password field held focus. Consumers
    /// must drop these rather than display or transmit them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_sensitive: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<SemanticElement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_elements: Option<Vec<SemanticElement>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_change: Option<TextChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<DocumentObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_text: Option<Vec<String>>,
}

impl ActivityEvent {
    /// A new event stamped with the current time and a fresh identifier.
    pub fn new(kind: EventKind) -> Self {
        Self::at(kind, Utc::now())
    }

    /// A new event at a caller-supplied instant. Used by tests to build fixed input.
    pub fn at(kind: EventKind, when: DateTime<Utc>) -> Self {
        ActivityEvent {
            version: SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            timestamp: when.to_rfc3339_opts(SecondsFormat::Millis, true),
            kind,
            application: None,
            window_title: None,
            accessibility_trusted: None,
            pointer_capture_available: None,
            is_sensitive: None,
            element: None,
            selected_elements: None,
            text_change: None,
            browser: None,
            document: None,
            visible_text: None,
        }
    }

    pub fn with_application(mut self, application: ApplicationDescriptor) -> Self {
        self.application = Some(application);
        self
    }

    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = Some(title.into());
        self
    }

    pub fn with_browser(mut self, browser: BrowserObservation) -> Self {
        self.browser = Some(browser);
        self
    }

    /// Attach the document the window is on. Absent when there is none to report.
    pub fn with_document(mut self, document: DocumentObservation) -> Self {
        self.document = Some(document);
        self
    }

    /// Attach the text the window was displaying. An empty list is left absent, so a
    /// window that showed nothing readable is indistinguishable from one that was
    /// never read — which is true, and is the safer of the two to record.
    pub fn with_visible_text(mut self, lines: Vec<String>) -> Self {
        if !lines.is_empty() {
            self.visible_text = Some(lines);
        }
        self
    }

    pub fn with_accessibility_trusted(mut self, trusted: bool) -> Self {
        self.accessibility_trusted = Some(trusted);
        self
    }

    pub fn mark_sensitive(mut self) -> Self {
        self.is_sensitive = Some(true);
        self
    }

    /// Parsed timestamp. Events are only constructed with valid stamps, but files on
    /// disk may have been edited, so this returns an `Option` rather than panicking.
    pub fn time(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.timestamp)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    /// True when this event must not be shown, summarized or transmitted.
    pub fn is_private(&self) -> bool {
        self.is_sensitive == Some(true)
            || self.browser.as_ref().is_some_and(|b| b.is_private)
            || self.kind == EventKind::PrivacyBoundary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixed() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-21T09:30:00.000Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn schema_shape_is_frozen() {
        let mut event = ActivityEvent::at(EventKind::ApplicationActivated, fixed())
            .with_application(ApplicationDescriptor {
                name: "Visual Studio Code".into(),
                path: r"C:\Program Files\Microsoft VS Code\Code.exe".into(),
                pid: 4242,
                bundle_id: None,
            })
            .with_window_title("event.rs - openhistory-win")
            .with_accessibility_trusted(true);
        event.id = "8f14e45f-ea19-4d4c-9d4e-1b2c3d4e5f60".into();

        let value = serde_json::to_value(&event).unwrap();

        assert_eq!(
            value,
            json!({
                "version": 1,
                "id": "8f14e45f-ea19-4d4c-9d4e-1b2c3d4e5f60",
                "timestamp": "2026-08-21T09:30:00.000Z",
                "kind": "applicationActivated",
                "application": {
                    "name": "Visual Studio Code",
                    "path": r"C:\Program Files\Microsoft VS Code\Code.exe",
                    "pid": 4242
                },
                "windowTitle": "event.rs - openhistory-win",
                "accessibilityTrusted": true
            }),
            "absent fields must be omitted, not null, and names must stay camelCase"
        );
    }

    #[test]
    fn every_event_kind_round_trips_through_its_wire_name() {
        let names = [
            (EventKind::CollectorStarted, "collectorStarted"),
            (EventKind::ApplicationActivated, "applicationActivated"),
            (EventKind::WindowChanged, "windowChanged"),
            (EventKind::FocusedElementChanged, "focusedElementChanged"),
            (EventKind::SelectionChanged, "selectionChanged"),
            (EventKind::TextInput, "textInput"),
            (EventKind::DocumentChanged, "documentChanged"),
            (EventKind::PointerClick, "pointerClick"),
            (EventKind::UrlChanged, "urlChanged"),
            (EventKind::DocumentContextChanged, "documentContextChanged"),
            (EventKind::UiSnapshot, "uiSnapshot"),
            (EventKind::ApplicationTerminated, "applicationTerminated"),
            (EventKind::ScreenSlept, "screenSlept"),
            (EventKind::ScreenWoke, "screenWoke"),
            (EventKind::SessionLocked, "sessionLocked"),
            (EventKind::SessionUnlocked, "sessionUnlocked"),
            (EventKind::PrivacyBoundary, "privacyBoundary"),
        ];

        for (kind, wire) in names {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<EventKind>(json!(wire)).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn macos_fields_deserialize_without_loss() {
        // A record as the macOS build would have written it, including bundleId and
        // element data that Windows never produces.
        let raw = json!({
            "version": 1,
            "id": "0f0e0d0c-0b0a-4908-8706-050403020100",
            "timestamp": "2026-08-21T09:30:00.000Z",
            "kind": "focusedElementChanged",
            "application": {
                "name": "Safari",
                "path": "/Applications/Safari.app",
                "pid": 501,
                "bundleId": "com.apple.Safari"
            },
            "element": { "role": "AXTextField", "isSensitive": true },
            "visibleText": ["one", "two"]
        });

        let event: ActivityEvent = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            event.application.as_ref().unwrap().bundle_id.as_deref(),
            Some("com.apple.Safari")
        );
        assert_eq!(event.element.as_ref().unwrap().is_sensitive, Some(true));
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            raw,
            "re-serialization must be lossless"
        );
    }

    #[test]
    fn a_document_is_labelled_by_its_name_rather_than_its_location() {
        let titled = DocumentObservation {
            path: Some(r"C:\Users\someone\Documents\budget-2026.xlsx".into()),
            title: Some("Budget 2026".into()),
        };
        assert_eq!(titled.label().as_deref(), Some("Budget 2026"));

        let untitled = DocumentObservation {
            path: Some(r"C:\Users\someone\Documents\budget-2026.xlsx".into()),
            title: None,
        };
        assert_eq!(untitled.label().as_deref(), Some("budget-2026.xlsx"));

        let web = DocumentObservation {
            path: Some("https://docs.example.com/spec/overview".into()),
            title: None,
        };
        assert_eq!(web.label().as_deref(), Some("overview"));

        let nothing = DocumentObservation {
            path: None,
            title: None,
        };
        assert_eq!(nothing.label(), None);
        assert!(nothing.is_empty());
    }

    #[test]
    fn a_document_and_visible_text_serialize_under_the_frozen_names() {
        let event = ActivityEvent::at(EventKind::WindowChanged, fixed())
            .with_document(DocumentObservation {
                path: Some(r"C:\work\notes.md".into()),
                title: Some("notes.md".into()),
            })
            .with_visible_text(vec!["Preview".into(), "Outline".into()]);

        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["document"]["path"], json!(r"C:\work\notes.md"));
        assert_eq!(value["document"]["title"], json!("notes.md"));
        assert_eq!(value["visibleText"], json!(["Preview", "Outline"]));
    }

    #[test]
    fn a_window_that_showed_nothing_readable_records_no_visible_text_field() {
        let event = ActivityEvent::at(EventKind::WindowChanged, fixed()).with_visible_text(vec![]);
        assert_eq!(event.visible_text, None);
        assert!(
            !serde_json::to_string(&event)
                .unwrap()
                .contains("visibleText"),
            "an empty read must be absent rather than an empty list"
        );
    }

    #[test]
    fn private_events_are_recognized() {
        let plain = ActivityEvent::at(EventKind::WindowChanged, fixed());
        assert!(!plain.is_private());

        let sensitive = ActivityEvent::at(EventKind::WindowChanged, fixed()).mark_sensitive();
        assert!(sensitive.is_private());

        let incognito =
            ActivityEvent::at(EventKind::UrlChanged, fixed()).with_browser(BrowserObservation {
                url: None,
                is_private: true,
            });
        assert!(incognito.is_private());

        assert!(ActivityEvent::at(EventKind::PrivacyBoundary, fixed()).is_private());
    }

    #[test]
    fn timestamps_parse_back() {
        let event = ActivityEvent::at(EventKind::CollectorStarted, fixed());
        assert_eq!(event.time().unwrap(), fixed());
    }
}
