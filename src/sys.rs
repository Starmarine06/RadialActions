//! Small OS helpers: clipboard, Explorer reveal, Recycle Bin, registry and
//! start-with-Windows (registry normally, a startup task in the Store package).

use std::path::{Path, PathBuf};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE};
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_DWORD,
    RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    FO_DELETE, SHFILEOPSTRUCTW,
};

/// Must match the startup task id in the package manifest.
const STARTUP_TASK: &str = "RadialActionsStartup";

pub fn copy_text(s: &str) -> Result<(), String> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // Another program may hold the clipboard for a moment.
        let mut opened = OpenClipboard(None);
        for _ in 0..10 {
            if opened.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            opened = OpenClipboard(None);
        }
        opened.map_err(|_| "The clipboard is busy".to_string())?;
        let result = (|| {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|e| e.to_string())?;
            let dst = GlobalLock(h) as *mut u16;
            if dst.is_null() {
                return Err("clipboard memory unavailable".to_string());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
            let _ = GlobalUnlock(h);
            // 13 = CF_UNICODETEXT; the system owns the block after this call.
            SetClipboardData(13, Some(HANDLE(h.0))).map_err(|e| e.to_string())?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn reveal(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Sends files and folders to the Recycle Bin. Where there is none (network
/// drives) Windows asks before deleting for good.
pub fn recycle(paths: &[PathBuf]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    // A double-null-terminated list.
    let mut from: Vec<u16> = Vec::new();
    for p in paths {
        from.extend(p.as_os_str().encode_wide());
        from.push(0);
    }
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0 | FOF_WANTNUKEWARNING.0) as u16,
        ..Default::default()
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted.as_bool() {
        Err("Couldn't move the files to the Recycle Bin".into())
    } else {
        Ok(())
    }
}

/// A DWORD from HKEY_CURRENT_USER.
pub fn read_dword(key: &str, value: &str) -> Option<u32> {
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let mut out = 0u32;
    let mut size = 4u32;
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_DWORD, None, Some(&mut out as *mut u32 as *mut _), Some(&mut size))
    };
    status.is_ok().then_some(out)
}

/// True when running from an installed Store (MSIX) package.
pub fn is_packaged() -> bool {
    let mut len = 0u32;
    unsafe { GetCurrentPackageFullName(&mut len, None) == ERROR_INSUFFICIENT_BUFFER }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("RadialActions");

/// Runs a Windows Runtime call on its own thread: its async operations never
/// finish on a COM single-threaded one.
fn winrt<T: Send>(f: impl FnOnce() -> windows::core::Result<T> + Send) -> Result<T, String> {
    std::thread::scope(|s| s.spawn(f).join())
        .map_err(|_| "Startup setting unavailable".to_string())?
        .map_err(|e| e.message().trim().to_string())
}

fn startup_task() -> windows::core::Result<windows::ApplicationModel::StartupTask> {
    windows::ApplicationModel::StartupTask::GetAsync(&HSTRING::from(STARTUP_TASK))?.join()
}

pub fn autostart_enabled() -> bool {
    if is_packaged() {
        use windows::ApplicationModel::StartupTaskState as State;
        return winrt(|| startup_task()?.State()).is_ok_and(|s| s == State::Enabled || s == State::EnabledByPolicy);
    }
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok()
    }
}

/// Adds or removes the entry that launches this app at sign-in.
pub fn set_autostart(on: bool) -> Result<(), String> {
    if is_packaged() {
        use windows::ApplicationModel::StartupTaskState as State;
        return winrt(move || {
            let task = startup_task()?;
            if !on {
                task.Disable()?;
                return Ok(true);
            }
            let state = task.RequestEnableAsync()?.join()?;
            Ok(state == State::Enabled || state == State::EnabledByPolicy)
        })
        .and_then(|ok| if ok { Ok(()) } else { Err("Turned off in Task Manager > Startup apps".to_string()) });
    }
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let cmd: Vec<u16> = format!("\"{}\" --background", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
            let err = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(cmd.as_ptr() as *const _),
                (cmd.len() * 2) as u32,
            );
            err.ok().map_err(|e| e.to_string())
        } else {
            let err = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
            // Already absent counts as success.
            if err.is_ok() || err.0 == 2 { Ok(()) } else { err.ok().map_err(|e| e.to_string()) }
        }
    }
}
