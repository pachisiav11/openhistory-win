//! Thin, safe wrappers over the Win32 calls the collector needs.
//!
//! Every function here returns `Option` or `Result` rather than panicking: windows
//! close and processes exit between the moment an event fires and the moment we ask
//! about them, so failure is ordinary rather than exceptional.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, MAX_PATH, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

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

/// Process that owns a window.
pub fn window_process_id(hwnd: HWND) -> Option<u32> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
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
    use super::*;

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
}
