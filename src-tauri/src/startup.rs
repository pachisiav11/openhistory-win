//! Whether Windows launches OpenHistory when the user signs in.
//!
//! One value under the current user's `Run` key. Per-user rather than per-machine:
//! the history belongs to one account, the installer writes to the user's profile,
//! and nothing here should ever need an administrator.
//!
//! The setting in `config.json` is what the user changed; this module is the copy of
//! it Windows reads. They are brought into line at every launch, so an entry deleted
//! by hand comes back and one left behind by an older install is corrected.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::HSTRING;

/// Where Windows looks for programs to start at sign-in.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// The name of our entry. Stable, so an old entry is replaced rather than doubled.
const ENTRY: &str = "OpenHistory";

/// Passed to the copy Windows starts, and to nothing else.
///
/// A history recorder that opens a window over whatever the user signed in to do is
/// a nuisance. Started this way the application goes straight to the tray.
pub const AUTOSTART_ARG: &str = "--autostart";

/// Make the sign-in entry agree with the setting.
///
/// Writes only when the entry differs from what it should be, so an ordinary launch
/// touches the registry once and then never again.
pub fn apply(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().context("could not find this executable")?;
    apply_in(RUN_KEY, ENTRY, enabled, &exe)
}

/// True when this process was started by the sign-in entry rather than by the user.
pub fn launched_by_windows() -> bool {
    started_automatically(std::env::args())
}

fn started_automatically(args: impl Iterator<Item = String>) -> bool {
    args.skip(1).any(|argument| argument == AUTOSTART_ARG)
}

/// What the entry must contain for a given executable.
///
/// Quoted, because the installed path runs through `AppData\Local` and a program
/// files path has a space in it either way.
fn command_for(exe: &Path) -> String {
    format!("\"{}\" {AUTOSTART_ARG}", exe.display())
}

fn apply_in(key: &str, name: &str, enabled: bool, exe: &Path) -> Result<()> {
    let wanted = enabled.then(|| command_for(exe));
    if read_value(key, name) == wanted {
        return Ok(());
    }

    match wanted {
        Some(command) => write_value(key, name, &command),
        None => delete_value(key, name),
    }
}

fn read_value(key: &str, name: &str) -> Option<String> {
    let key = HSTRING::from(key);
    let name = HSTRING::from(name);

    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;

    // `size` is in bytes and counts the terminator.
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;

    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

fn write_value(key: &str, name: &str, value: &str) -> Result<()> {
    let key = HSTRING::from(key);
    let name = HSTRING::from(name);

    let mut handle = HKEY::default();
    // SAFETY: every pointer below outlives the call, and the handle is closed on
    // both paths out.
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &key,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
    }
    .ok()
    .context("could not open the sign-in key for writing")?;

    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is alive for the whole call, and REG_SZ data is the UTF-16
    // string including its terminator, measured in bytes.
    let bytes = unsafe {
        std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), std::mem::size_of_val(&wide[..]))
    };
    let written = unsafe { RegSetValueExW(handle, &name, None, REG_SZ, Some(bytes)) };
    unsafe { RegCloseKey(handle) }.ok().ok();

    written.ok().context("could not write the sign-in entry")
}

fn delete_value(key: &str, name: &str) -> Result<()> {
    let key = HSTRING::from(key);
    let name = HSTRING::from(name);

    let mut handle = HKEY::default();
    let opened =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &key, None, KEY_SET_VALUE, &mut handle) };
    if opened == ERROR_FILE_NOT_FOUND {
        // No key means no entry, which is what was asked for.
        return Ok(());
    }
    opened.ok().context("could not open the sign-in key")?;

    let removed = unsafe { RegDeleteValueW(handle, &name) };
    unsafe { RegCloseKey(handle) }.ok().ok();

    if removed == ERROR_SUCCESS || removed == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(anyhow!(
            "could not remove the sign-in entry: {}",
            removed.to_hresult().message()
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use windows::Win32::System::Registry::RegDeleteKeyW;

    use super::*;

    /// Where the sign-in entry points, so a test can check the path without
    /// re-deriving the quoting rules it is meant to be checking.
    fn entry_in(key: &str, name: &str) -> Option<PathBuf> {
        let value = read_value(key, name)?;
        let path = value.strip_suffix(&format!(" {AUTOSTART_ARG}"))?;
        Some(PathBuf::from(path.trim_matches('"')))
    }

    /// A key of our own to write in. The real Run key is the user's, and a test has
    /// no business changing what starts on their machine.
    struct ScratchKey(String);

    impl ScratchKey {
        fn new(name: &str) -> Self {
            ScratchKey(format!(r"Software\openhistory-win\test-{name}"))
        }

        fn path(&self) -> &str {
            &self.0
        }
    }

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            let key = HSTRING::from(self.0.as_str());
            let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &key) };
        }
    }

    #[test]
    fn enabling_writes_the_quoted_command_and_disabling_takes_it_away() {
        let key = ScratchKey::new("enable-disable");
        let exe = PathBuf::from(r"C:\Users\someone\AppData\Local\OpenHistory\openhistory-win.exe");

        apply_in(key.path(), ENTRY, true, &exe).unwrap();
        assert_eq!(
            read_value(key.path(), ENTRY).as_deref(),
            Some(r#""C:\Users\someone\AppData\Local\OpenHistory\openhistory-win.exe" --autostart"#),
            "the path is quoted and carries the flag"
        );
        assert_eq!(entry_in(key.path(), ENTRY).as_deref(), Some(exe.as_path()));

        apply_in(key.path(), ENTRY, false, &exe).unwrap();
        assert_eq!(read_value(key.path(), ENTRY), None);
    }

    #[test]
    fn applying_the_same_setting_twice_is_not_an_error() {
        let key = ScratchKey::new("idempotent");
        let exe = PathBuf::from(r"C:\OpenHistory\openhistory-win.exe");

        apply_in(key.path(), ENTRY, true, &exe).unwrap();
        apply_in(key.path(), ENTRY, true, &exe).unwrap();
        assert_eq!(entry_in(key.path(), ENTRY).as_deref(), Some(exe.as_path()));

        apply_in(key.path(), ENTRY, false, &exe).unwrap();
        apply_in(key.path(), ENTRY, false, &exe).unwrap();
        assert_eq!(read_value(key.path(), ENTRY), None);
    }

    #[test]
    fn a_moved_executable_corrects_the_entry_rather_than_adding_another() {
        let key = ScratchKey::new("moved");
        let old = PathBuf::from(r"D:\build\openhistory-win.exe");
        let new = PathBuf::from(r"C:\Users\someone\AppData\Local\OpenHistory\openhistory-win.exe");

        apply_in(key.path(), ENTRY, true, &old).unwrap();
        apply_in(key.path(), ENTRY, true, &new).unwrap();

        assert_eq!(entry_in(key.path(), ENTRY).as_deref(), Some(new.as_path()));
    }

    #[test]
    fn disabling_a_setting_that_was_never_on_leaves_no_key_behind() {
        let key = ScratchKey::new("never-on");
        let exe = PathBuf::from(r"C:\OpenHistory\openhistory-win.exe");

        apply_in(key.path(), ENTRY, false, &exe).unwrap();
        assert_eq!(read_value(key.path(), ENTRY), None);
    }

    #[test]
    fn only_windows_starting_us_counts_as_automatic() {
        let launched = ["openhistory-win.exe", AUTOSTART_ARG].map(String::from);
        assert!(started_automatically(launched.into_iter()));

        let opened = ["openhistory-win.exe"].map(String::from);
        assert!(!started_automatically(opened.into_iter()));

        // The program's own name is not an argument, however it was invoked.
        let named = [AUTOSTART_ARG].map(String::from);
        assert!(!started_automatically(named.into_iter()));
    }
}
