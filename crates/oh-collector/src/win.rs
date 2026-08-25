//! Thin, safe wrappers over the Win32 calls the collector needs.
//!
//! Every function here returns `Option` or `Result` rather than panicking: windows
//! close and processes exit between the moment an event fires and the moment we ask
//! about them, so failure is ordinary rather than exceptional.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, MAX_PATH, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

/// Owns a process handle so it is closed on every path out of a function.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // Nothing useful to do if this fails, and it cannot fail for a handle we
            // opened ourselves and have not already closed.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..end])
        .to_string_lossy()
        .into_owned()
}

/// The window currently in the foreground, or `None` when the desktop has focus.
pub fn foreground_window() -> Option<HWND> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.is_invalid()).then_some(hwnd)
}

/// Title text of a window. Empty titles come back as `None`.
pub fn window_title(hwnd: HWND) -> Option<String> {
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return None;
    }

    let mut buffer = vec![0u16; length as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if written <= 0 {
        return None;
    }

    let title = wide_to_string(&buffer[..written as usize]);
    (!title.trim().is_empty()).then_some(title)
}

/// A window's registered class name.
///
/// Class names are the only reliable way to tell one of Explorer's transient shell
/// surfaces from a real application window, because both report the same process.
pub fn window_class(hwnd: HWND) -> Option<String> {
    // 256 is the maximum length `RegisterClass` accepts, so no class name is longer.
    let mut buffer = [0u16; 256];
    let written = unsafe { GetClassNameW(hwnd, &mut buffer) };
    (written > 0).then(|| wide_to_string(&buffer[..written as usize]))
}

/// Windows owned by a window, at any level below it.
///
/// An embedded browser keeps its page in a window of its own — `WRY_WEBVIEW`,
/// `Chrome_WidgetWin_1`, `Chrome_RenderWidgetHostHWND` — and builds the page's
/// accessibility tree only when something asks *that* window for it. Reading the
/// top-level window therefore reaches a node called "… - Web content" with nothing
/// under it, which is what a Tauri or WebView2 application looked like: a menu bar and
/// an empty promise.
pub fn child_windows(parent: HWND, max: usize) -> Vec<HWND> {
    let mut found: Vec<HWND> = Vec::new();
    if max == 0 {
        return found;
    }

    let mut state = (&mut found, max);
    // SAFETY: the callback runs to completion inside this call, so the pointer to
    // `state` is valid for as long as it is used.
    let _ =
        unsafe { EnumChildWindows(Some(parent), Some(gather), LPARAM(&raw mut state as isize)) };
    found
}

unsafe extern "system" fn gather(hwnd: HWND, param: LPARAM) -> BOOL {
    // SAFETY: `param` is the pointer `child_windows` passed to EnumChildWindows.
    let (found, max) = unsafe { &mut *(param.0 as *mut (&mut Vec<HWND>, usize)) };
    found.push(hwnd);
    // Zero ends the enumeration.
    BOOL(i32::from(found.len() < *max))
}

/// Process that owns a window.
pub fn window_process_id(hwnd: HWND) -> Option<u32> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// True when a window belongs to this process.
///
/// Reading a window is not thread-safe against ourselves: `GetWindowTextW` on a
/// window owned by another thread of the same process sends `WM_GETTEXT` and waits
/// for that thread to pump it. The collector runs on its own thread while the
/// application's window is owned by the main one, so asking about our own window
/// while the main thread waits on the collector would deadlock both.
pub fn is_own_window(hwnd: HWND) -> bool {
    window_process_id(hwnd) == Some(current_process_id())
}

fn open_for_query(pid: u32) -> Option<OwnedHandle> {
    // SYNCHRONIZE is requested so the same handle can answer whether the process has
    // exited; see `process_is_alive`.
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let handle = unsafe { OpenProcess(access, false, pid) }.ok()?;
    (!handle.is_invalid()).then_some(OwnedHandle(handle))
}

/// Full path to a process executable.
///
/// Returns `None` for protected and elevated processes, which is expected: the
/// collector records what it can see and stays quiet about the rest.
pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    let handle = open_for_query(pid)?;

    let mut buffer = vec![0u16; MAX_PATH as usize * 2];
    let mut size = buffer.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    }
    .ok()?;

    let path = wide_to_string(&buffer[..size as usize]);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// True when the process is still running.
///
/// `OpenProcess` alone is not the answer: a process object outlives the process
/// itself for as long as anyone holds a handle to it, so an exited program stays
/// openable and would look alive. A process handle becomes signalled at exit, so a
/// zero-timeout wait distinguishes the two. `GetExitCodeProcess` would also work but
/// misreports any program that genuinely exits with code 259.
pub fn process_is_alive(pid: u32) -> bool {
    let Some(handle) = open_for_query(pid) else {
        return false;
    };
    let state = unsafe { WaitForSingleObject(handle.0, 0) };
    state == WAIT_TIMEOUT
}

/// This process's own identifier.
pub fn current_process_id() -> u32 {
    unsafe { GetCurrentProcessId() }
}

/// Human-readable application name, taken from the executable's version resource.
///
/// Falls back to the file stem, so `Code.exe` reports "Visual Studio Code" when the
/// resource is present and "Code" when it is not.
pub fn display_name(path: &Path) -> String {
    version_product_name(path).unwrap_or_else(|| file_stem(path))
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn version_product_name(path: &Path) -> Option<String> {
    let wide = HSTRING::from(path.as_os_str());

    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None) };
    if size == 0 {
        return None;
    }

    let mut block = vec![0u8; size as usize];
    unsafe { GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, block.as_mut_ptr().cast()) }
        .ok()?;

    // The translation table names which language and codepage the strings are filed
    // under; reading FileDescription from the wrong one yields nothing.
    let (language, codepage) = version_translation(&block)?;
    for field in ["FileDescription", "ProductName"] {
        let query = HSTRING::from(format!(
            "\\StringFileInfo\\{language:04x}{codepage:04x}\\{field}"
        ));
        if let Some(value) = version_string(&block, &query)
            && !value.trim().is_empty()
        {
            return Some(value);
        }
    }
    None
}

fn version_translation(block: &[u8]) -> Option<(u16, u16)> {
    let query = HSTRING::from("\\VarFileInfo\\Translation");
    let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;

    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            PCWSTR(query.as_ptr()),
            &mut data,
            &mut len,
        )
    };
    if !ok.as_bool() || data.is_null() || len < 4 {
        return None;
    }

    // SAFETY: the call above reported at least four readable bytes at `data`, laid
    // out as two little-endian u16 values inside the version block we own.
    let pair = unsafe { std::slice::from_raw_parts(data.cast::<u16>(), 2) };
    Some((pair[0], pair[1]))
}

fn version_string(block: &[u8], query: &HSTRING) -> Option<String> {
    let mut data: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;

    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            PCWSTR(query.as_ptr()),
            &mut data,
            &mut len,
        )
    };
    if !ok.as_bool() || data.is_null() || len == 0 {
        return None;
    }

    // SAFETY: `len` is the character count reported for a string inside the version
    // block, which outlives this borrow.
    let text = unsafe { std::slice::from_raw_parts(data.cast::<u16>(), len as usize) };
    Some(wide_to_string(text))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG,
        PM_REMOVE, PeekMessageW, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
        WNDCLASSW,
    };
    use windows::core::{PCWSTR, w};

    use super::*;

    unsafe extern "system" fn test_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    /// A message-only window owned by the calling thread, with a title to read.
    fn create_test_window() -> HWND {
        let class_name: PCWSTR = w!("OpenHistoryWinTestWindow");
        let instance = unsafe { GetModuleHandleW(None) }.expect("module handle");
        let class = WNDCLASSW {
            lpfnWndProc: Some(test_window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        unsafe { RegisterClassW(&class) };

        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("a title only this thread can hand over"),
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

    /// Run the calling thread's message queue until it is empty.
    fn pump() {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            let _ = unsafe { TranslateMessage(&message) };
            unsafe { DispatchMessageW(&message) };
        }
    }

    #[test]
    fn reads_own_process_identity() {
        let pid = current_process_id();
        assert!(pid != 0);
        assert!(process_is_alive(pid));

        let path = process_image_path(pid).expect("own image path must be readable");
        assert!(path.is_file(), "{} should exist", path.display());
        assert!(!file_stem(&path).is_empty());
    }

    #[test]
    fn a_pid_that_cannot_exist_is_not_alive() {
        // The System Idle Process is pid 0 and cannot be opened for query.
        assert!(!process_is_alive(0));
    }

    #[test]
    fn an_exited_process_is_not_alive_even_while_its_handle_is_held() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "exit"])
            .spawn()
            .expect("cmd.exe must launch");
        let pid = child.id();

        assert!(process_is_alive(pid) || child.try_wait().is_ok_and(|s| s.is_some()));

        child.wait().expect("child must be reapable");

        // `child` still owns a handle to the process object, which keeps OpenProcess
        // succeeding. Liveness must not be inferred from that.
        assert!(
            !process_is_alive(pid),
            "an exited process must not report as alive"
        );
    }

    #[test]
    fn display_name_prefers_the_version_resource() {
        // explorer.exe ships a FileDescription of "Windows Explorer"; the stem alone
        // would be "explorer".
        let explorer =
            PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()))
                .join("explorer.exe");
        if explorer.is_file() {
            let name = display_name(&explorer);
            assert!(!name.is_empty());
            assert!(
                name.to_ascii_lowercase().contains("explorer"),
                "unexpected display name {name:?}"
            );
        }
    }

    #[test]
    fn display_name_falls_back_to_the_file_stem() {
        let missing = PathBuf::from(r"C:\definitely\not\here\SomeTool.exe");
        assert_eq!(display_name(&missing), "SomeTool");
    }

    #[test]
    fn a_window_of_this_process_is_recognised_as_ours() {
        let window = create_test_window();
        assert!(is_own_window(window), "a window we created is ours");
        assert_eq!(window_process_id(window), Some(current_process_id()));

        let _ = unsafe { DestroyWindow(window) };
    }

    #[test]
    fn an_invalid_window_belongs_to_nobody() {
        assert!(!is_own_window(HWND(std::ptr::null_mut())));
    }

    #[test]
    fn reading_our_own_title_waits_for_the_thread_that_owns_the_window() {
        // Why `is_own_window` exists. The title of a window in this process arrives
        // by a message its owning thread has to pump, so a second thread asking for
        // it gets nothing until this one does. When the owning thread is itself
        // waiting on that second thread, neither ever moves again.
        let window = create_test_window();
        let handle = window.0 as isize;

        let (answered, answer) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let hwnd = HWND(handle as *mut core::ffi::c_void);
            let _ = answered.send(window_title(hwnd));
        });

        assert!(
            answer.recv_timeout(Duration::from_millis(750)).is_err(),
            "the title came back without this thread pumping for it"
        );

        // Let the reader finish, so the test leaves no thread stuck behind it.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut title = None;
        while Instant::now() < deadline {
            pump();
            if let Ok(answered) = answer.recv_timeout(Duration::from_millis(50)) {
                title = answered;
                break;
            }
        }
        reader.join().expect("the reading thread must finish");
        let _ = unsafe { DestroyWindow(window) };

        assert_eq!(
            title.as_deref(),
            Some("a title only this thread can hand over"),
            "pumping the queue is what hands the title over"
        );
    }
}
