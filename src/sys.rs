//! Small OS helpers: clipboard, Explorer reveal and the persisted pick mode.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// `true` when a slice is picked by releasing Shift over it instead of clicking.
pub static FLICK: AtomicBool = AtomicBool::new(false);

fn config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("RadialActions").join("config.txt"))
}

pub fn load_config() {
    if let Some(text) = config_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        FLICK.store(text.lines().any(|l| l.trim() == "mode=flick"), Ordering::Relaxed);
    }
}

pub fn save_config() {
    let Some(p) = config_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mode = if FLICK.load(Ordering::Relaxed) { "flick" } else { "click" };
    let _ = std::fs::write(p, format!("mode={mode}\n"));
}

pub fn copy_text(s: &str) -> Result<(), String> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        OpenClipboard(None).map_err(|e| e.to_string())?;
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

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("RadialActions");

pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok()
    }
}

/// Adds or removes the per-user Run entry that launches this exe at sign-in.
pub fn set_autostart(on: bool) -> Result<(), String> {
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let cmd: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
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
