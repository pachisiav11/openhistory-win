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

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern,
    TreeScope_Children, TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_EditControlTypeId,
    UIA_ValuePatternId,
};
use windows::core::Interface;

use crate::browser::Browser;

/// How many `Edit` elements to inspect before giving up on finding the address bar.
///
/// A browser window exposes a handful: the omnibox, the find bar, and occasionally a
/// form field. Walking further costs real time on a large accessibility tree and
/// never finds anything useful.
const MAX_EDIT_CANDIDATES: i32 = 12;

/// How many direct children of a window to read names from when checking for a
/// private-browsing marker. A browser window has a handful.
const MAX_WINDOW_CHILDREN: i32 = 8;

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

fn element_is_password(element: &IUIAutomationElement) -> bool {
    unsafe { element.CurrentIsPassword() }.is_ok_and(|flag| flag.as_bool())
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
    fn automation_starts_in_an_apartment() {
        let _apartment = ComApartment::enter();
        let automation = Automation::new().expect("UIAutomation must be available on Windows");

        // No password field can be focused in a headless test process, but the call
        // must complete rather than fault.
        let _ = automation.focus_is_password_field();
    }
}
