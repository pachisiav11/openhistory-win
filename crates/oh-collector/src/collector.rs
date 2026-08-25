//! The collector thread.
//!
//! One thread owns everything: the COM apartment, the UIAutomation client, the
//! WinEvent hooks and a message-only window that receives power and session
//! notifications. Win32 requires it — `SetWinEventHook` with `WINEVENT_OUTOFCONTEXT`
//! delivers through the calling thread's message queue, and a window procedure only
//! runs on the thread that created its window.
//!
//! Because the WinEvent callback signature carries no user pointer, per-thread state
//! lives in a thread-local. That is sound here precisely because the hook, the window
//! procedure and the state all belong to the same thread.

use std::cell::RefCell;
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use oh_core::{
    ActivityEvent, ApplicationDescriptor, BrowserObservation, DocumentObservation, EventKind,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_NAMECHANGE,
    EVENT_SYSTEM_FOREGROUND, GetMessageW, HWND_MESSAGE, KillTimer, MSG, OBJID_WINDOW,
    PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE, PostMessageW,
    PostThreadMessageW, RegisterClassW, SetTimer, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_POWERBROADCAST, WM_QUIT, WM_TIMER,
    WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use crate::browser::Browser;
use crate::config::CollectorConfig;
use crate::uia::{Automation, ComApartment, ReadBudget};
use crate::win;

/// Anything that can receive events. The collector calls this on its own thread, so
/// implementations must not block for long.
pub type EventSink = Box<dyn Fn(ActivityEvent) + Send + 'static>;

/// Identifier for the liveness timer on the collector's message window.
const LIVENESS_TIMER: usize = 1;

/// How often to check whether the recorded application has exited.
///
/// Closing a program does not reliably produce a foreground-change notification —
/// terminating one produces none at all — so the exit has to be noticed by asking
/// rather than by waiting to be told. Two seconds is far below the five-minute gap
/// that ends an episode, so nothing downstream can tell the difference.
const LIVENESS_INTERVAL_MS: u32 = 2_000;

thread_local! {
    static STATE: RefCell<Option<CollectorState>> = const { RefCell::new(None) };
}

/// How much of a window may be written down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Privacy {
    /// Ordinary window: title and, for browsers, URL may be recorded.
    Recordable,
    /// A private or incognito browser window. Only the boundary itself is recorded.
    Private,
    /// A browser window whose privacy could not be established. The application is
    /// recorded; the title and URL are not.
    Undetermined,
}

/// Window classes that belong to the Windows shell rather than to an application the
/// user is working in.
///
/// These take the foreground constantly — Alt-Tab, Task View, the Start menu, the
/// taskbar — and every one of them is owned by `explorer.exe` or another shell host,
/// so a process-level exclusion would silently drop real File Explorer windows too.
/// The class name is the only thing that separates them.
const SHELL_SURFACE_CLASSES: &[&str] = &[
    "TaskSwitcherWnd",
    "TaskSwitcherOverlayWnd",
    "MultitaskingViewFrame",
    "XamlExplorerHostIslandWindow",
    "Windows.UI.Core.CoreWindow",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "ForegroundStaging",
    "WorkerW",
    "Progman",
];

/// True when a window is shell chrome rather than something the user was using.
fn is_shell_surface(hwnd: HWND) -> bool {
    win::window_class(hwnd).is_some_and(|class| class_is_shell_surface(&class))
}

fn class_is_shell_surface(class: &str) -> bool {
    SHELL_SURFACE_CLASSES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(class))
}

/// How long to wait before reading a window's visible text again.
///
/// The read walks the accessibility tree, which costs real time, and a title change
/// is not evidence that the interface changed — a video player retitles every second.
/// Half a minute is short enough that switching between two documents records both
/// and long enough that a chatty window costs nothing.
const VISIBLE_TEXT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

struct CollectorState {
    sink: EventSink,
    automation: Option<Automation>,
    config: CollectorConfig,
    last_pid: Option<u32>,
    last_app_name: Option<String>,
    last_title: Option<String>,
    last_url: Option<String>,
    /// When the visible text was last read, and for which window. A different window
    /// is read at once; the same one waits for the interval.
    last_text_read: Option<(isize, std::time::Instant)>,
    /// An application the foreground has left that was still running when it went.
    ///
    /// Closing a window hands the foreground on before the process has finished
    /// exiting, so the liveness check made at that moment can still say "running".
    /// Without somewhere to put it the pid is overwritten by the application now in
    /// front, and the exit is never noticed by anything. The liveness timer re-asks
    /// about whatever is parked here. One slot is enough: it is the application the
    /// user just left.
    departed: Option<(u32, String)>,
}

impl CollectorState {
    fn emit(&self, event: ActivityEvent) {
        (self.sink)(event);
    }

    /// Build the descriptor for a window's owning process, or `None` when the process
    /// is gone, unreadable, or excluded from recording.
    fn describe(&self, hwnd: HWND) -> Option<(ApplicationDescriptor, String)> {
        // Our own windows are never activity worth recording, and reading one would
        // hang: the title arrives by a message the main thread has to pump, and the
        // main thread may well be waiting on this one. The hooks skip this process
        // for the same reason; this covers the paths the hooks do not drive.
        if win::is_own_window(hwnd) {
            return None;
        }

        let pid = win::window_process_id(hwnd)?;
        let path = win::process_image_path(pid)?;
        let stem = win::file_stem(&path);

        if self.config.excludes(&stem) {
            return None;
        }

        let descriptor = ApplicationDescriptor {
            name: win::display_name(&path),
            path: path.to_string_lossy().into_owned(),
            pid,
            bundle_id: None,
        };
        Some((descriptor, stem))
    }

    fn base_event(&self, kind: EventKind, application: &ApplicationDescriptor) -> ActivityEvent {
        ActivityEvent::new(kind)
            .with_application(application.clone())
            .with_accessibility_trusted(self.automation.is_some())
    }

    /// Report that a recorded application has exited.
    ///
    /// Both the application in front and the one the foreground last left are asked
    /// about, because an application is usually still exiting at the moment it hands
    /// the foreground on.
    fn close_previous_application(&mut self) {
        if let Some((pid, name)) = self.departed.clone()
            && !win::process_is_alive(pid)
        {
            self.departed = None;
            self.emit_termination(pid, name);
        }

        let (Some(pid), Some(name)) = (self.last_pid, self.last_app_name.clone()) else {
            return;
        };
        if win::process_is_alive(pid) {
            return;
        }

        self.emit_termination(pid, name);
        self.last_pid = None;
        self.last_app_name = None;
        self.last_title = None;
        self.last_url = None;
    }

    fn emit_termination(&self, pid: u32, name: String) {
        self.emit(
            ActivityEvent::new(EventKind::ApplicationTerminated).with_application(
                ApplicationDescriptor {
                    name,
                    path: String::new(),
                    pid,
                    bundle_id: None,
                },
            ),
        );
    }

    /// Decide how much of a window may be recorded.
    fn assess(&self, hwnd: HWND, browser: Option<Browser>, title: Option<&str>) -> Privacy {
        let Some(browser) = browser else {
            return Privacy::Recordable;
        };

        // Firefox and older Chromium builds still publish the marker in the window
        // title, which is far cheaper to read than the accessibility tree.
        if title.is_some_and(|t| browser.title_indicates_private(t)) {
            return Privacy::Private;
        }

        match self.automation.as_ref() {
            Some(automation) if automation.window_is_private(hwnd, browser) => Privacy::Private,
            Some(_) => Privacy::Recordable,
            // Without the accessibility tree there is no way to tell an incognito
            // window from an ordinary one, and guessing wrong records a private
            // session. Record that the browser was in front and nothing more.
            None => Privacy::Undetermined,
        }
    }

    fn handle_foreground(&mut self, hwnd: HWND) {
        if is_shell_surface(hwnd) {
            return;
        }
        self.close_previous_application();

        let Some((application, stem)) = self.describe(hwnd) else {
            return;
        };
        let title = win::window_title(hwnd);

        // Explorer owns the desktop and the taskbar, so the foreground bounces back
        // to it repeatedly with nothing actually changing. Re-reporting an identical
        // window would fill the timeline with duplicates.
        if self.last_pid == Some(application.pid) && self.last_title == title {
            return;
        }

        if let (Some(previous), Some(name)) = (self.last_pid, self.last_app_name.clone())
            && previous != application.pid
        {
            self.departed = Some((previous, name));
        }
        self.last_pid = Some(application.pid);
        self.last_app_name = Some(application.name.clone());
        self.report_window(
            EventKind::ApplicationActivated,
            hwnd,
            &application,
            &stem,
            title,
        );
    }

    fn handle_title_change(&mut self, hwnd: HWND) {
        // Only the foreground window matters; background windows retitle constantly.
        if win::foreground_window() != Some(hwnd) || is_shell_surface(hwnd) {
            return;
        }

        let Some((application, stem)) = self.describe(hwnd) else {
            return;
        };
        let title = win::window_title(hwnd);
        if title == self.last_title {
            return;
        }

        self.report_window(EventKind::WindowChanged, hwnd, &application, &stem, title);
    }

    /// Report one window observation, honouring whatever the privacy assessment
    /// allows. Shared by the foreground and title-change paths so the two can never
    /// disagree about what is safe to record.
    fn report_window(
        &mut self,
        kind: EventKind,
        hwnd: HWND,
        application: &ApplicationDescriptor,
        stem: &str,
        title: Option<String>,
    ) {
        let browser = Browser::from_exe_stem(stem);

        match self.assess(hwnd, browser, title.as_deref()) {
            Privacy::Private => {
                // Acknowledged and then left alone: no title, no URL, nothing that
                // could identify the session reaches disk.
                self.emit(
                    self.base_event(EventKind::PrivacyBoundary, application)
                        .with_browser(BrowserObservation {
                            url: None,
                            is_private: true,
                        }),
                );
                self.last_title = None;
                self.last_url = None;
                return;
            }
            Privacy::Undetermined => {
                self.emit(self.base_event(kind, application));
                self.last_title = None;
                self.last_url = None;
                return;
            }
            Privacy::Recordable => {}
        }

        let sensitive = self
            .automation
            .as_ref()
            .is_some_and(|a| a.focus_is_password_field());

        let mut event = self.base_event(kind, application);
        if sensitive {
            event = event.mark_sensitive();
        } else {
            if let Some(title) = title.clone() {
                event = event.with_window_title(title);
            }
            let (document, lines) =
                self.read_window(hwnd, title.as_deref(), stem, &application.name);
            if let Some(document) = document {
                event = event.with_document(document);
            }
            event = event.with_visible_text(lines);
        }
        self.emit(event);

        self.last_title = title;
        if !sensitive && let Some(browser) = browser {
            self.report_url(hwnd, application, browser);
        }
    }

    /// One bounded read of the window: the document it is on, and the text it shows.
    ///
    /// Both come from one walk of the accessibility tree, because the walk is what
    /// costs time and this runs on the thread that pumps the hooks.
    ///
    /// A window that has just been read contributes no text rather than the previous
    /// answer: repeating an observation would say it was seen twice, which is not what
    /// happened. The document is read on every window change, since that is what
    /// changed.
    ///
    /// The window title is what the document is checked against: an application that
    /// answers with the name of its editing control rather than the open file would
    /// otherwise be recorded as a document called "Text editor". See
    /// `DocumentObservation::names_the_window`.
    fn read_window(
        &mut self,
        hwnd: HWND,
        window_title: Option<&str>,
        stem: &str,
        display_name: &str,
    ) -> (Option<DocumentObservation>, Vec<String>) {
        let Some(automation) = self.automation.as_ref() else {
            return (None, Vec::new());
        };

        let window = hwnd.0 as isize;
        let now = std::time::Instant::now();
        let due = !self.last_text_read.is_some_and(|(last_window, last_read)| {
            last_window == window && now.duration_since(last_read) < VISIBLE_TEXT_INTERVAL
        });

        let want_text = self.config.capture_visible_text && due;
        let budget = if self.config.reads_deeply(stem, display_name) {
            ReadBudget::STUDY
        } else {
            ReadBudget::GLANCE
        };
        let reading = automation.read_window(
            hwnd,
            self.config.capture_documents,
            want_text,
            budget,
            window_title,
        );

        // A read that came back with nothing but the window's own name did not observe
        // the window, so it must not start the clock that suppresses the next one.
        // Chromium and Electron build their accessibility tree the first time a client
        // reaches into it, and paint the page after the window already exists; the read
        // taken the instant a tab opens is always that read. Holding it for half a
        // minute is what turned a window full of text into `["Claude"]`. Leaving the
        // clock unset lets the title change that follows a page load try again.
        if want_text && reading.lines.len() > 1 {
            self.last_text_read = Some((window, now));
        }

        let document = reading.document.and_then(|(path, title)| {
            let document = DocumentObservation { path, title };
            (!document.is_empty() && document.names_the_window(window_title)).then_some(document)
        });
        (document, reading.lines)
    }

    fn report_url(&mut self, hwnd: HWND, application: &ApplicationDescriptor, browser: Browser) {
        if !self.config.capture_urls {
            return;
        }
        let Some(automation) = self.automation.as_ref() else {
            return;
        };
        let Some(url) = automation.address_bar_url(hwnd, browser) else {
            return;
        };
        if self.last_url.as_deref() == Some(url.as_str()) {
            return;
        }

        self.emit(
            self.base_event(EventKind::UrlChanged, application)
                .with_browser(BrowserObservation {
                    url: Some(url.clone()),
                    is_private: false,
                }),
        );
        self.last_url = Some(url);
    }

    fn handle_simple(&self, kind: EventKind) {
        self.emit(ActivityEvent::new(kind));
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if hwnd.is_invalid() || id_object != OBJID_WINDOW.0 {
        return;
    }

    STATE.with(|cell| {
        let Ok(mut borrowed) = cell.try_borrow_mut() else {
            return;
        };
        let Some(state) = borrowed.as_mut() else {
            return;
        };

        match event {
            EVENT_SYSTEM_FOREGROUND => state.handle_foreground(hwnd),
            EVENT_OBJECT_NAMECHANGE => state.handle_title_change(hwnd),
            _ => {}
        }
    });
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_POWERBROADCAST => {
            handle_power(wparam, lparam);
            LRESULT(1)
        }
        WM_TIMER => {
            with_state(|state| state.close_previous_application());
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            let kind = match wparam.0 as u32 {
                WTS_SESSION_LOCK => Some(EventKind::SessionLocked),
                WTS_SESSION_UNLOCK => Some(EventKind::SessionUnlocked),
                _ => None,
            };
            if let Some(kind) = kind {
                with_state(|state| state.handle_simple(kind));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn handle_power(wparam: WPARAM, lparam: LPARAM) {
    match wparam.0 as u32 {
        PBT_APMSUSPEND => with_state(|state| state.handle_simple(EventKind::ScreenSlept)),
        PBT_APMRESUMEAUTOMATIC => with_state(|state| state.handle_simple(EventKind::ScreenWoke)),
        PBT_POWERSETTINGCHANGE => {
            if lparam.0 == 0 {
                return;
            }
            // SAFETY: for PBT_POWERSETTINGCHANGE the system passes a pointer to a
            // POWERBROADCAST_SETTING that stays valid for the duration of this call.
            let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
            if setting.PowerSetting != GUID_CONSOLE_DISPLAY_STATE || setting.DataLength == 0 {
                return;
            }

            // Data[0]: 0 off, 1 on, 2 dimmed. Dimming is not a state change worth
            // recording — the user is still there.
            match setting.Data[0] {
                0 => with_state(|state| state.handle_simple(EventKind::ScreenSlept)),
                1 => with_state(|state| state.handle_simple(EventKind::ScreenWoke)),
                _ => {}
            }
        }
        _ => {}
    }
}

fn with_state(f: impl FnOnce(&mut CollectorState)) {
    STATE.with(|cell| {
        if let Ok(mut borrowed) = cell.try_borrow_mut()
            && let Some(state) = borrowed.as_mut()
        {
            f(state);
        }
    });
}

/// A running collector. Dropping this stops the thread.
pub struct Collector {
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
    /// Raw handle to the message-only window. Stored as an integer because `HWND` is
    /// not `Send`; it is only ever used to post messages, which is thread-safe.
    window: isize,
}

impl Collector {
    /// Start collecting. Returns once the hooks are installed and the first
    /// `collectorStarted` event has been delivered.
    pub fn start(sink: EventSink, config: CollectorConfig) -> Result<Collector> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, isize), String>>();

        let thread = std::thread::Builder::new()
            .name("openhistory-collector".into())
            .spawn(move || collector_thread(sink, config, ready_tx))
            .context("could not spawn the collector thread")?;

        match ready_rx.recv() {
            Ok(Ok((thread_id, window))) => Ok(Collector {
                thread: Some(thread),
                thread_id,
                window,
            }),
            Ok(Err(message)) => Err(anyhow!(message)),
            Err(_) => Err(anyhow!("the collector thread stopped before it started")),
        }
    }

    /// Stop collecting and wait for the thread to unwind its hooks.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(thread) = self.thread.take() {
            // SAFETY: posting WM_QUIT to a thread we started is valid whether or not
            // its message loop is currently running.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            let _ = thread.join();
        }
    }

    /// Deliver a session-change notification as though the system had sent it.
    ///
    /// Locking a real workstation is not something a test may do to someone's
    /// machine, so the lock and unlock paths are exercised by posting the same
    /// message Windows would post, to the same window procedure.
    pub fn inject_session_change(&self, locked: bool) {
        let code = if locked {
            WTS_SESSION_LOCK
        } else {
            WTS_SESSION_UNLOCK
        };
        // SAFETY: `self.window` is a window this collector created and has not
        // destroyed; PostMessageW is safe to call across threads.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.window as *mut core::ffi::c_void)),
                WM_WTSSESSION_CHANGE,
                WPARAM(code as usize),
                LPARAM(0),
            )
        };
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn collector_thread(
    sink: EventSink,
    config: CollectorConfig,
    ready: mpsc::Sender<Result<(u32, isize), String>>,
) {
    let _apartment = ComApartment::enter();
    let automation = Automation::new().ok();
    if automation.is_none() {
        tracing::warn!("UIAutomation is unavailable; URLs and password fields cannot be read");
    }

    let window = match create_message_window() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(format!(
                "could not create the collector window: {error}"
            )));
            return;
        }
    };

    let power = unsafe {
        RegisterPowerSettingNotification(
            windows::Win32::Foundation::HANDLE(window.0),
            &GUID_CONSOLE_DISPLAY_STATE,
            windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    }
    .ok();
    if power.is_none() {
        tracing::warn!("display power notifications are unavailable");
    }

    let session_registered =
        unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) }.is_ok();
    if !session_registered {
        tracing::warn!("session lock notifications are unavailable");
    }

    STATE.with(|cell| {
        *cell.borrow_mut() = Some(CollectorState {
            sink,
            automation,
            config,
            last_pid: None,
            last_app_name: None,
            last_title: None,
            last_url: None,
            last_text_read: None,
            departed: None,
        });
    });

    let hooks = install_hooks();
    if hooks.is_empty() {
        let _ = ready.send(Err("SetWinEventHook was refused".into()));
        cleanup(window, session_registered, power, &hooks);
        return;
    }

    let timer = unsafe { SetTimer(Some(window), LIVENESS_TIMER, LIVENESS_INTERVAL_MS, None) };
    if timer == 0 {
        tracing::warn!("could not start the liveness timer; application exits may go unnoticed");
    }

    with_state(|state| state.handle_simple(EventKind::CollectorStarted));

    let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    let _ = ready.send(Ok((thread_id, window.0 as isize)));

    // Report the window that is already in front, so a session that begins with the
    // user mid-task is not blank until they switch away.
    //
    // This happens after the caller has been released. Describing a window can call
    // into the window's own process, which may be slow or stopped, and the caller is
    // usually a user interface starting up: it must not wait on a stranger's window.
    if let Some(hwnd) = win::foreground_window() {
        with_state(|state| state.handle_foreground(hwnd));
    }

    run_message_loop();

    cleanup(window, session_registered, power, &hooks);
    STATE.with(|cell| *cell.borrow_mut() = None);
}

fn create_message_window() -> Result<HWND> {
    let class_name: PCWSTR = w!("OpenHistoryCollectorWindow");
    let instance = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW failed")?;

    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        ..Default::default()
    };
    // A non-zero atom means registered; zero usually means "already registered by an
    // earlier run on another thread", which is fine.
    unsafe { RegisterClassW(&class) };

    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("OpenHistory"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
    }
    .context("CreateWindowExW failed")?;

    Ok(window)
}

fn install_hooks() -> Vec<HWINEVENTHOOK> {
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
    ]
    .into_iter()
    .filter_map(|(min, max)| {
        let hook = unsafe { SetWinEventHook(min, max, None, Some(win_event_proc), 0, 0, flags) };
        (!hook.is_invalid()).then_some(hook)
    })
    .collect()
}

fn run_message_loop() {
    let mut message = MSG::default();
    // GetMessageW returns 0 on WM_QUIT and -1 on error; either ends the loop.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn cleanup(
    window: HWND,
    session_registered: bool,
    power: Option<HPOWERNOTIFY>,
    hooks: &[HWINEVENTHOOK],
) {
    for hook in hooks {
        let _ = unsafe { UnhookWinEvent(*hook) };
    }
    let _ = unsafe { KillTimer(Some(window), LIVENESS_TIMER) };
    if let Some(power) = power {
        let _ = unsafe { UnregisterPowerSettingNotification(power) };
    }
    if session_registered {
        let _ = unsafe { WTSUnRegisterSessionNotification(window) };
    }
    let _ = unsafe { DestroyWindow(window) };
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    use super::*;

    /// A message-only window owned by the calling thread, titled so that anything
    /// which does read it produces something recognisable.
    fn create_owned_window() -> HWND {
        let class_name: PCWSTR = w!("OpenHistoryCollectorTestWindow");
        let instance = unsafe { GetModuleHandleW(None) }.expect("module handle");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        unsafe { RegisterClassW(&class) };

        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("OpenHistory itself"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
        }
        .expect("the test window must be created")
    }

    fn test_state(sink: EventSink) -> CollectorState {
        CollectorState {
            sink,
            automation: None,
            config: CollectorConfig::default(),
            last_pid: None,
            last_app_name: None,
            last_title: None,
            last_url: None,
            last_text_read: None,
            departed: None,
        }
    }

    #[test]
    fn our_own_window_is_neither_recorded_nor_waited_on() {
        // The launch deadlock, in miniature. One thread owns a window and does not
        // pump for it — as the application's main thread does not while it waits for
        // the collector to start — and the collector, on another thread, is handed
        // that window as the one in front. Without the guard in `describe` this asks
        // the owning thread for a title it will never answer, and both threads stop
        // for good.
        let (created, window) = mpsc::channel::<isize>();
        let (release, released) = mpsc::channel::<()>();
        let owner = std::thread::spawn(move || {
            let window = create_owned_window();
            let _ = created.send(window.0 as isize);
            // Deliberately not pumping: this thread is standing in for a main thread
            // blocked inside `Collector::start`.
            let _ = released.recv();
            let _ = unsafe { DestroyWindow(window) };
        });

        let handle = window.recv_timeout(Duration::from_secs(5)).expect("window");

        let (finished, outcome) = mpsc::channel::<Option<ActivityEvent>>();
        let collector = std::thread::spawn(move || {
            let (recorded, events) = mpsc::channel::<ActivityEvent>();
            let mut state = test_state(Box::new(move |event| {
                let _ = recorded.send(event);
            }));
            state.handle_foreground(HWND(handle as *mut core::ffi::c_void));
            let _ = finished.send(events.try_recv().ok());
        });

        let seen = outcome
            .recv_timeout(Duration::from_secs(5))
            .expect("describing our own window must not wait on the thread that owns it");
        assert!(
            seen.is_none(),
            "a window of this process reached the timeline: {seen:?}"
        );

        collector.join().expect("the collector thread must finish");
        let _ = release.send(());
        owner.join().expect("the owning thread must finish");
    }

    #[test]
    fn shell_chrome_is_not_an_application() {
        // Alt-Tab, Task View, the Start menu and the taskbar all report themselves as
        // explorer.exe, so the class name is what keeps them out of the timeline.
        for class in [
            "TaskSwitcherWnd",
            "MultitaskingViewFrame",
            "Windows.UI.Core.CoreWindow",
            "Shell_TrayWnd",
            "Progman",
        ] {
            assert!(class_is_shell_surface(class), "{class} must be filtered");
        }
    }

    #[test]
    fn real_windows_are_left_alone() {
        // CabinetWClass is File Explorer proper, which is a real application window
        // owned by the same process as the shell chrome above.
        for class in [
            "CabinetWClass",
            "Chrome_WidgetWin_1",
            "MozillaWindowClass",
            "#32770",
            "ApplicationFrameWindow",
        ] {
            assert!(!class_is_shell_surface(class), "{class} must be recorded");
        }
    }

    #[test]
    fn class_matching_ignores_case() {
        assert!(class_is_shell_surface("taskswitcherwnd"));
        assert!(class_is_shell_surface("SHELL_TRAYWND"));
    }
}
