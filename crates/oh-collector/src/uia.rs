//! UIAutomation reads: the browser address bar, and whether focus sits in a
//! password field.
//!
//! COM objects here are apartment-threaded, so an `Automation` must be created and
//! used on the same thread. The collector satisfies that by doing all of its work on
//! one dedicated thread.
//!
//! Every read is best-effort. Elevated windows, DRM-protected players and
//! applications that refuse automation all fail these calls, and the correct
//! response is to record less rather than to stop collecting.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
    IUIAutomationValuePattern, TreeScope_Children, TreeScope_Descendants, UIA_ButtonControlTypeId,
    UIA_CONTROLTYPE_ID, UIA_CheckBoxControlTypeId, UIA_ComboBoxControlTypeId,
    UIA_ControlTypePropertyId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
    UIA_HeaderControlTypeId, UIA_HeaderItemControlTypeId, UIA_ImageControlTypeId,
    UIA_MenuBarControlTypeId, UIA_MenuControlTypeId, UIA_MenuItemControlTypeId,
    UIA_PaneControlTypeId, UIA_ProgressBarControlTypeId, UIA_RadioButtonControlTypeId,
    UIA_ScrollBarControlTypeId, UIA_SeparatorControlTypeId, UIA_SliderControlTypeId,
    UIA_SpinnerControlTypeId, UIA_SplitButtonControlTypeId, UIA_StatusBarControlTypeId,
    UIA_TextControlTypeId, UIA_TextPatternId, UIA_ThumbControlTypeId, UIA_TitleBarControlTypeId,
    UIA_ToolBarControlTypeId, UIA_ToolTipControlTypeId, UIA_ValuePatternId,
    UIA_WindowControlTypeId,
};
use windows::core::Interface;

use crate::browser::Browser;
use crate::text::{self, Surface, TextBudget};

/// How many `Edit` elements to inspect before giving up on finding the address bar.
///
/// A browser window exposes a handful: the omnibox, the find bar, and occasionally a
/// form field. Walking further costs real time on a large accessibility tree and
/// never finds anything useful.
const MAX_EDIT_CANDIDATES: i32 = 12;

/// How many direct children of a window to read names from when checking for a
/// private-browsing marker. A browser window has a handful.
const MAX_WINDOW_CHILDREN: i32 = 8;

/// The most characters of an editing surface's value to look at.
///
/// A `Value` on a document element is the writing itself, and Word will hand over a
/// whole page of it. Cutting here bounds the work of splitting it into lines; the text
/// budget then decides how much of the result is written down.
const MAX_VALUE_CHARS: usize = 4_000;

/// How far one read of a window may go.
///
/// Every step is a cross-process call, so each of these is a time budget as much as a
/// size one, and the clock is what makes the cost predictable rather than merely
/// bounded: the collector thread pumps the WinEvent hooks, so time spent inside an
/// automation call is time the next window change waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadBudget {
    /// How many *named* levels of the tree to descend. Unnamed containers are not
    /// counted; see the walk in `read_window`.
    pub depth: u32,
    /// How many elements to visit.
    pub elements: usize,
    /// How many children of any one element to queue.
    pub children: i32,
    /// How long the whole read may take, whatever the other budgets allow.
    pub time: Duration,
    /// How much of what it finds may be written down.
    pub text: TextBudget,
    /// Read the value of an editing surface as well as its name.
    ///
    /// The name of Word's editing surface is "Page 1 content". The value is the page.
    pub values: bool,
    /// How many of the window's own child windows to start a walk from as well.
    ///
    /// Zero for an ordinary read. See [`crate::win::child_windows`]: an embedded
    /// browser answers for its page only when its own window is asked.
    pub child_windows: usize,
}

impl ReadBudget {
    /// What every application gets: the tab strip, the headings and the name of the
    /// thing being edited, which is what makes a summary specific.
    pub const GLANCE: ReadBudget = ReadBudget {
        depth: 4,
        elements: 80,
        children: 24,
        time: Duration::from_millis(120),
        text: TextBudget::GLANCE,
        values: false,
        child_windows: 0,
    };

    /// What the applications named in `recording.deepReadApps` get.
    ///
    /// Reaching a chat message in an Electron window means descending six named levels
    /// past a sidebar several hundred elements wide, so both budgets are large. The
    /// clock is the one that actually binds, and half a second is the most a foreground
    /// change may cost.
    pub const STUDY: ReadBudget = ReadBudget {
        depth: 8,
        elements: 900,
        children: 40,
        time: Duration::from_millis(500),
        text: TextBudget::STUDY,
        values: true,
        child_windows: 8,
    };
}

/// Marks a thread as COM-initialized for as long as it is held.
pub struct ComApartment {
    initialized: bool,
}

impl ComApartment {
    /// Join the apartment-threaded model on this thread.
    ///
    /// Succeeding with `RPC_E_CHANGED_MODE` is not possible here, but another
    /// component may already have initialized the thread; that is reported as a
    /// non-error and simply means we must not uninitialize it ourselves.
    pub fn enter() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComApartment {
            initialized: hr.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

/// What one bounded read of a window found.
#[derive(Debug, Default)]
pub struct WindowReading {
    /// The document the window is on, as its location and its name. Both parts are
    /// optional: an editor may publish one without the other.
    pub document: Option<(Option<String>, Option<String>)>,
    /// Redacted lines of the text the window is displaying.
    pub lines: Vec<String>,
    /// How many elements the walk looked at. Only the probe reads this; it is the
    /// difference between "the budget ran out" and "the window had nothing more".
    pub visited: usize,
}

/// Handle to the UIAutomation service.
pub struct Automation {
    inner: IUIAutomation,
}

impl Automation {
    /// Create the automation client. Requires COM to be initialized on this thread.
    pub fn new() -> windows::core::Result<Self> {
        let inner: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
        Ok(Automation { inner })
    }

    /// Read the URL out of a browser window's address bar.
    ///
    /// Returns `None` whenever the address bar cannot be found or read, which is
    /// common and not an error worth reporting.
    pub fn address_bar_url(&self, hwnd: HWND, browser: Browser) -> Option<String> {
        let root = unsafe { self.inner.ElementFromHandle(hwnd) }.ok()?;

        let condition = unsafe {
            self.inner.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &VARIANT::from(UIA_EditControlTypeId.0),
            )
        }
        .ok()?;

        let candidates = unsafe { root.FindAll(TreeScope_Descendants, &condition) }.ok()?;
        let count = unsafe { candidates.Length() }
            .ok()?
            .min(MAX_EDIT_CANDIDATES);

        let names = browser.address_bar_names();
        for i in 0..count {
            let Ok(element) = (unsafe { candidates.GetElement(i) }) else {
                continue;
            };

            // Never read a password field, whatever it is called.
            if element_is_password(&element) {
                continue;
            }

            let name = unsafe { element.CurrentName() }
                .map(|n| n.to_string())
                .unwrap_or_default();
            let lowered = name.to_ascii_lowercase();
            if !names.iter().any(|candidate| lowered.contains(candidate)) {
                continue;
            }

            if let Some(value) = element_value(&element) {
                return normalize_url(&value);
            }
        }
        None
    }

    /// One bounded read of a window's accessibility tree.
    ///
    /// The document a window is on and the text it is displaying are found in the same
    /// walk because the walk is the expensive part. Both are budgeted four ways — by
    /// elements visited, by depth, by children queued per element, and by a wall clock —
    /// and the walk stops as soon as it has what was asked of it.
    ///
    /// An earlier version asked UIAutomation for every `Document` descendant of the
    /// window in one call. That is a whole-tree search inside the other process, it
    /// answers in seconds on a large Chromium window, and none of the caps applied until
    /// after it returned. Nothing about the caller changed; the cost did.
    ///
    /// Password fields and offscreen elements are never read — the first because it is a
    /// credential, the second because a hidden menu is not something the user was
    /// looking at.
    ///
    /// Each line is labelled as writing, content or furniture, because the budget is
    /// contended and losing that contention to a row of window controls is what made the
    /// read useless. The label comes from the control type and from whether the element
    /// sits inside a toolbar or a menu; nothing below a button is walked into, since a
    /// button holds a label and not a document.
    pub fn read_window(
        &self,
        hwnd: HWND,
        want_document: bool,
        want_text: bool,
        budget: ReadBudget,
        window_title: Option<&str>,
    ) -> WindowReading {
        let mut reading = WindowReading::default();
        if !want_document && !want_text {
            return reading;
        }

        let Ok(root) = (unsafe { self.inner.ElementFromHandle(hwnd) }) else {
            return reading;
        };
        let Ok(condition) = (unsafe { self.inner.CreateTrueCondition() }) else {
            return reading;
        };

        let deadline = Instant::now() + budget.time;
        let mut raw: Vec<(Surface, String)> = Vec::new();
        let mut writing = 0usize;
        let mut queue: VecDeque<(IUIAutomationElement, u32, Surface)> =
            VecDeque::from([(root, 0u32, Surface::Content)]);
        let mut visited = 0usize;
        // The window's own child windows are walked only once its own tree is
        // exhausted. Seeding them at the start cost an Electron window its whole clock
        // on trees it had already published through the top-level element.
        let mut asked_child_windows = false;

        loop {
            self.walk(
                &mut queue,
                &condition,
                &budget,
                deadline,
                (want_document, want_text),
                (&mut reading, &mut raw, &mut writing, &mut visited),
            );

            let satisfied = !want_text || writing >= budget.text.lines;
            let exhausted = visited >= budget.elements || Instant::now() >= deadline;
            if asked_child_windows || satisfied || exhausted || budget.child_windows == 0 {
                break;
            }
            asked_child_windows = true;
            for child in crate::win::child_windows(hwnd, budget.child_windows) {
                if let Ok(element) = unsafe { self.inner.ElementFromHandle(child) } {
                    queue.push_back((element, 0u32, Surface::Content));
                }
            }
        }

        reading.visited = visited;
        reading.lines = text::redact_lines(raw, budget.text, window_title);
        reading
    }

    /// Drain a queue of elements into the reading, within the budget.
    ///
    /// Split out only so the walk can be run twice over different starting points; the
    /// budgets are shared across both runs, which is what keeps the second one from
    /// doubling the cost of the first.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        queue: &mut VecDeque<(IUIAutomationElement, u32, Surface)>,
        condition: &windows::Win32::UI::Accessibility::IUIAutomationCondition,
        budget: &ReadBudget,
        deadline: Instant,
        (want_document, want_text): (bool, bool),
        (reading, raw, writing, visited): (
            &mut WindowReading,
            &mut Vec<(Surface, String)>,
            &mut usize,
            &mut usize,
        ),
    ) {
        while let Some((element, depth, inherited)) = queue.pop_front() {
            // Enough prose to fill the budget is enough to stop looking. Counting any
            // named element instead stopped the walk on a sidebar of past
            // conversations, several hundred entries wide, and never reached the one
            // that was open.
            let text_done = !want_text || *writing >= budget.text.lines;
            let document_done = !want_document || reading.document.is_some();
            if (text_done && document_done) || *visited >= budget.elements {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            *visited += 1;

            if element_is_password(&element) || element_is_offscreen(&element) {
                continue;
            }

            let kind = unsafe { element.CurrentControlType() }.ok();
            let name = unsafe { element.CurrentName() }
                .ok()
                .map(|name| name.to_string());

            // Furniture is inherited from a toolbar or a menu, and only from those.
            // Everything else is judged on its own control type, so that a pane counts
            // as scaffolding without disqualifying the document inside it.
            let below_furniture = inherited == Surface::Furniture
                || kind.is_some_and(|kind| FURNITURE_SUBTREES.contains(&kind));
            let surface = match kind {
                _ if below_furniture => Surface::Furniture,
                Some(kind) if FURNITURE_CONTROLS.contains(&kind) => Surface::Furniture,
                Some(kind) if WRITING_CONTROLS.contains(&kind) => Surface::Writing,
                _ => Surface::Content,
            };

            if !document_done && kind == Some(UIA_DocumentControlTypeId) {
                let title = name.as_deref().and_then(text::redact_line);
                // The value of a document element is where it came from. It is only
                // worth recording when it names a location rather than repeating the
                // contents.
                let path = element_value(&element).and_then(|value| document_path(&value));
                if title.is_some() || path.is_some() {
                    reading.document = Some((path, title));
                }
            }

            // An element that said nothing is scaffolding rather than a level of
            // content, and must not be charged to the depth budget. Chromium hangs a
            // page under seven or more unnamed panes: counting those spent the whole
            // budget on empty containers and returned nothing but the window's own
            // name from windows full of text. Only a level that named something
            // counts as a level, so the budget still limits how far into real content
            // the read reaches — which is what it was there to do.
            let named = name.as_deref().is_some_and(|name| !name.trim().is_empty());

            if want_text
                && let Some(name) = name
                && !name.trim().is_empty()
            {
                if surface == Surface::Writing {
                    *writing += 1;
                }
                raw.push((surface, name));
            }

            // Counted per element rather than per line: one document answers with a
            // page of them, and stopping the walk on that is stopping it on whichever
            // element happened to be reached first.
            if want_text
                && budget.values
                && surface == Surface::Writing
                && kind.is_some_and(|kind| WRITING_VALUE_CONTROLS.contains(&kind))
            {
                let lines = element_writing(&element, budget.text.lines);
                if !lines.is_empty() {
                    *writing += 1;
                }
                raw.extend(lines.into_iter().map(|line| (Surface::Writing, line)));
            }

            if depth >= budget.depth || kind.is_some_and(|kind| LEAF_CONTROLS.contains(&kind)) {
                continue;
            }
            let child_depth = if named { depth + 1 } else { depth };
            let Ok(children) = (unsafe { element.FindAll(TreeScope_Children, condition) }) else {
                continue;
            };
            let count = unsafe { children.Length() }
                .unwrap_or(0)
                .min(budget.children);
            let child_surface = if below_furniture {
                Surface::Furniture
            } else {
                Surface::Content
            };
            for i in 0..count {
                if let Ok(child) = unsafe { children.GetElement(i) } {
                    queue.push_back((child, child_depth, child_surface));
                }
            }
        }
    }

    /// True when keyboard focus is inside a password field anywhere on the desktop.
    pub fn focus_is_password_field(&self) -> bool {
        unsafe { self.inner.GetFocusedElement() }
            .ok()
            .is_some_and(|element| element_is_password(&element))
    }

    /// Decide whether a browser window is a private or incognito one.
    ///
    /// The window title is not enough. Current Chrome titles an incognito window
    /// exactly as it titles a normal one — `about:blank - Google Chrome` — and only
    /// the accessibility tree carries the marker, on the browser's root view:
    /// `about:blank - Google Chrome (Incognito)`. Reading the tree is therefore the
    /// primary signal, with the title kept as a cheap confirmation for the browsers
    /// that still publish it there.
    pub fn window_is_private(&self, hwnd: HWND, browser: Browser) -> bool {
        self.window_names(hwnd)
            .iter()
            .any(|name| browser.title_indicates_private(name))
    }

    /// Accessible names of a window and its immediate children.
    fn window_names(&self, hwnd: HWND) -> Vec<String> {
        let mut names = Vec::new();

        let Ok(root) = (unsafe { self.inner.ElementFromHandle(hwnd) }) else {
            return names;
        };
        if let Ok(name) = unsafe { root.CurrentName() } {
            names.push(name.to_string());
        }

        // The marker lives one level down, on the browser's root view. Going deeper
        // would walk the page content, which is both slow and none of our business.
        let Ok(condition) = (unsafe { self.inner.CreateTrueCondition() }) else {
            return names;
        };
        let Ok(children) = (unsafe { root.FindAll(TreeScope_Children, &condition) }) else {
            return names;
        };
        let count = unsafe { children.Length() }
            .unwrap_or(0)
            .min(MAX_WINDOW_CHILDREN);

        for i in 0..count {
            if let Ok(child) = unsafe { children.GetElement(i) }
                && let Ok(name) = unsafe { child.CurrentName() }
            {
                names.push(name.to_string());
            }
        }
        names
    }
}

/// Controls that make everything below them furniture as well.
///
/// A ribbon publishes several hundred controls, all of them named after what they do
/// and none of them after anything that was read or written. Naming the container is
/// what lets one test disqualify the lot.
///
/// Deliberately short. `Pane` was on this list for one round and disqualified almost
/// everything: Word hangs its document under a pane, and so does Chromium.
const FURNITURE_SUBTREES: &[UIA_CONTROLTYPE_ID] = &[
    UIA_ToolBarControlTypeId,
    UIA_MenuBarControlTypeId,
    UIA_MenuControlTypeId,
    UIA_TitleBarControlTypeId,
    UIA_StatusBarControlTypeId,
];

/// Controls whose own name describes the frame rather than what is in it.
///
/// Their children are judged on their own account: a pane is scaffolding, but what it
/// holds may be the whole of the window's content.
const FURNITURE_CONTROLS: &[UIA_CONTROLTYPE_ID] = &[
    UIA_ButtonControlTypeId,
    UIA_SplitButtonControlTypeId,
    UIA_MenuItemControlTypeId,
    UIA_CheckBoxControlTypeId,
    UIA_RadioButtonControlTypeId,
    UIA_ComboBoxControlTypeId,
    UIA_ScrollBarControlTypeId,
    UIA_SliderControlTypeId,
    UIA_SpinnerControlTypeId,
    UIA_ThumbControlTypeId,
    UIA_ProgressBarControlTypeId,
    UIA_ToolTipControlTypeId,
    UIA_SeparatorControlTypeId,
    UIA_HeaderControlTypeId,
    UIA_HeaderItemControlTypeId,
    UIA_ImageControlTypeId,
    UIA_WindowControlTypeId,
    UIA_PaneControlTypeId,
];

/// Controls with nothing below them worth the cross-process calls to reach.
///
/// A button holds a label. Walking into one on a sidebar of several hundred of them is
/// how the element budget was spent before the read ever reached the conversation.
const LEAF_CONTROLS: &[UIA_CONTROLTYPE_ID] = &[
    UIA_ButtonControlTypeId,
    UIA_SplitButtonControlTypeId,
    UIA_MenuItemControlTypeId,
    UIA_CheckBoxControlTypeId,
    UIA_RadioButtonControlTypeId,
    UIA_ComboBoxControlTypeId,
    UIA_ScrollBarControlTypeId,
    UIA_SliderControlTypeId,
    UIA_SpinnerControlTypeId,
    UIA_ThumbControlTypeId,
    UIA_ProgressBarControlTypeId,
    UIA_ToolTipControlTypeId,
    UIA_SeparatorControlTypeId,
    UIA_ImageControlTypeId,
];

/// Controls that carry prose rather than a label for something.
///
/// These win the budget outright. A conversation's messages and a manuscript's
/// paragraphs are `Text` and `Document` elements; the sidebar of past conversations
/// standing in front of them is not, and that is the whole of the difference between
/// reading the chat and reading the list of chats.
const WRITING_CONTROLS: &[UIA_CONTROLTYPE_ID] = &[
    UIA_TextControlTypeId,
    UIA_DocumentControlTypeId,
    UIA_EditControlTypeId,
];

/// Controls worth asking for their contents as well as their name.
///
/// A `Text` element's name already is its text, and asking one for a text range is not
/// free: Chromium answers with a pattern covering the whole document, once per element,
/// which spent a Claude window's entire read on a hundred copies of the same page.
const WRITING_VALUE_CONTROLS: &[UIA_CONTROLTYPE_ID] =
    &[UIA_DocumentControlTypeId, UIA_EditControlTypeId];

/// The writing inside an editing surface, as lines.
///
/// Word names its editing surface "Page 1 content", which says a document was open and
/// nothing about what was in it. The writing itself comes through one of two patterns:
/// a plain text box publishes it as a `Value`, and a word processor publishes it as a
/// `Text` range. Both are tried, and both are cut before they are split, because a
/// range can be a whole page and the work of splitting one is not worth doing on the
/// thread that pumps the hooks.
///
/// An element whose value is a location is a page rather than a document, and is
/// refused outright. A Chromium document publishes its address as its value and its
/// whole window as its text range, in document order — which is the navigation, then
/// the sidebar, then eventually the conversation. Taking the first lines of that is
/// taking the furniture again, by a longer road.
fn element_writing(element: &IUIAutomationElement, max_lines: usize) -> Vec<String> {
    let writing = match element_value(element) {
        Some(value) if document_path(&value).is_some() => return Vec::new(),
        Some(value) => Some(value),
        None => element_range_text(element),
    };
    let Some(writing) = writing else {
        return Vec::new();
    };

    writing
        .chars()
        .take(MAX_VALUE_CHARS)
        .collect::<String>()
        // Word ends a paragraph with a carriage return and no line feed, so splitting
        // on line feeds alone returns the document as a single line.
        .split(['\n', '\r'])
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .map(str::to_owned)
        .collect()
}

/// The text of an element that publishes a range rather than a value.
fn element_range_text(element: &IUIAutomationElement) -> Option<String> {
    let pattern = unsafe { element.GetCurrentPattern(UIA_TextPatternId) }.ok()?;
    let text: IUIAutomationTextPattern = pattern.cast().ok()?;
    let range = unsafe { text.DocumentRange() }.ok()?;
    let found = unsafe { range.GetText(MAX_VALUE_CHARS as i32) }
        .ok()?
        .to_string();
    (!found.trim().is_empty()).then_some(found)
}

fn element_is_password(element: &IUIAutomationElement) -> bool {
    unsafe { element.CurrentIsPassword() }.is_ok_and(|flag| flag.as_bool())
}

fn element_is_offscreen(element: &IUIAutomationElement) -> bool {
    unsafe { element.CurrentIsOffscreen() }.is_ok_and(|flag| flag.as_bool())
}

/// Whether a document element's value names a location worth recording.
///
/// An editor puts the file's path here; a text control puts its contents here. The
/// two are told apart by shape: a location is one line, has no spaces around a
/// separator, and either looks like a Windows path or like an address. Anything else
/// is the document itself and must not be copied into the log.
pub fn document_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r', '\t']) {
        return None;
    }
    if trimmed.chars().count() > text::MAX_LINE_CHARS {
        return None;
    }

    let windows_path = trimmed.len() > 3
        && trimmed.as_bytes()[1] == b':'
        && matches!(trimmed.as_bytes()[2], b'\\' | b'/')
        && trimmed.as_bytes()[0].is_ascii_alphabetic();
    let unc_path = trimmed.starts_with(r"\\");

    if windows_path || unc_path {
        return Some(trimmed.to_owned());
    }
    normalize_url(trimmed)
}

fn element_value(element: &IUIAutomationElement) -> Option<String> {
    let pattern = unsafe { element.GetCurrentPattern(UIA_ValuePatternId) }.ok()?;
    let value: IUIAutomationValuePattern = pattern.cast().ok()?;
    let text = unsafe { value.CurrentValue() }.ok()?.to_string();
    (!text.trim().is_empty()).then_some(text)
}

/// Tidy an omnibox value into something that reads as a URL.
///
/// Browsers hide the scheme, so `github.com/rust-lang` comes back rather than
/// `https://github.com/rust-lang`. Anything that is plainly a search phrase rather
/// than an address is discarded, since recording what someone typed into a search
/// box is a different and more sensitive thing than recording where they went.
pub fn normalize_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
        || trimmed.starts_with("about:")
        || trimmed.starts_with("chrome://")
        || trimmed.starts_with("edge://")
    {
        return Some(trimmed.to_owned());
    }

    // Looks like a bare host if it has no spaces and a dot before any slash.
    if trimmed.contains(' ') {
        return None;
    }
    let host = trimmed.split('/').next().unwrap_or(trimmed);
    if !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return None;
    }

    Some(format!("https://{trimmed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_fully_qualified_urls_unchanged() {
        for url in [
            "https://github.com/rust-lang/rust",
            "http://localhost:1420/",
            "about:blank",
            "chrome://settings",
            "file:///C:/notes.txt",
        ] {
            assert_eq!(normalize_url(url).as_deref(), Some(url));
        }
    }

    #[test]
    fn restores_the_scheme_browsers_hide() {
        assert_eq!(
            normalize_url("github.com/rust-lang/rust").as_deref(),
            Some("https://github.com/rust-lang/rust")
        );
        assert_eq!(
            normalize_url("example.com").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn discards_search_phrases() {
        assert_eq!(normalize_url("how to write a win32 hook"), None);
        assert_eq!(normalize_url("rust"), None);
        assert_eq!(normalize_url(""), None);
        assert_eq!(normalize_url("   "), None);
        assert_eq!(normalize_url(".com"), None);
        assert_eq!(normalize_url("trailing."), None);
    }

    #[test]
    fn a_document_value_that_names_a_location_is_kept() {
        for path in [
            r"C:\Users\someone\Documents\budget-2026.xlsx",
            r"D:/work/notes.md",
            r"\\server\share\report.docx",
            "https://docs.example.com/spec/overview",
        ] {
            assert_eq!(document_path(path).as_deref(), Some(path), "{path}");
        }
    }

    #[test]
    fn a_document_value_that_is_the_document_itself_is_refused() {
        // A text control reports its contents through the same property an editor
        // reports its path through. Copying that into the log is copying the file.
        assert_eq!(document_path("The quick brown fox jumped over"), None);
        assert_eq!(document_path("first line\nsecond line"), None);
        assert_eq!(document_path(&"a".repeat(400)), None);
        assert_eq!(document_path("   "), None);
    }

    #[test]
    fn automation_starts_in_an_apartment() {
        let _apartment = ComApartment::enter();
        let automation = Automation::new().expect("UIAutomation must be available on Windows");

        // No password field can be focused in a headless test process, but the call
        // must complete rather than fault.
        let _ = automation.focus_is_password_field();
    }
}
