#![windows_subsystem = "windows"]

mod actions;
mod archive;
mod convert;
mod gfx;
mod hook;
mod office;
#[cfg(test)]
mod office_tests;
mod ooxml;
mod overlay;
mod ocr;
mod pdf;
mod pdfword;
mod pptx;
mod recent;
mod sys;
mod target;
mod textconv;
mod xlsx;

use windows::Win32::Foundation::POINT;
use windows::Win32::System::Com::{
    CoFreeUnusedLibrariesEx, CoInitializeEx, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// Unloads idle COM libraries (UI Automation) and releases cached pages so
/// the resident size drops back to its idle baseline.
pub fn trim_memory() {
    unsafe {
        CoFreeUnusedLibrariesEx(0, None);
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

fn on_trigger(pt: POINT) {
    let Some(paths) = target::targets_at(pt) else {
        trim_memory();
        return;
    };
    let (title, key, root) = actions::wheel_for(&paths);
    overlay::show(pt, title, key, paths[0].clone(), root);
    trim_memory();
}

fn main() {
    unsafe {
        // Physical pixels, so cursor and UI Automation coordinates agree.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let args: Vec<String> = std::env::args().collect();
    // `--probe X Y` prints the item at one screen point and exits; used for testing.
    if args.get(1).map(String::as_str) == Some("--probe") {
        let x = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let y = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        match target::targets_at(POINT { x, y }) {
            Some(paths) => {
                for p in paths {
                    println!("{}", p.display());
                }
            }
            None => println!("(no item)"),
        }
        return;
    }
    // `--list <file>...` prints the wheel the app would show for those files.
    if args.get(1).map(String::as_str) == Some("--list") {
        let files: Vec<std::path::PathBuf> = args.iter().skip(2).map(std::path::PathBuf::from).collect();
        if files.is_empty() {
            println!("usage: --list <file>...");
        } else {
            print!("{}", actions::outline(&files));
        }
        return;
    }
    // `--run "<action id>" <file>...` runs one wheel action headlessly, e.g. --run Convert/JPG a.png.
    if args.get(1).map(String::as_str) == Some("--run") {
        let id = args.get(2).cloned().unwrap_or_default();
        let files: Vec<std::path::PathBuf> = args.iter().skip(3).map(std::path::PathBuf::from).collect();
        let result = match files.len() {
            0 => Err("no file given".to_string()),
            1 => actions::run_by_id(&files[0], &id),
            _ => actions::run_by_id_many(&files, &id),
        };
        match result {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                println!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // `--autostart on|off|status` manages the start-with-Windows entry from a shell.
    if args.get(1).map(String::as_str) == Some("--autostart") {
        let msg = match args.get(2).map(String::as_str) {
            Some("on") => sys::set_autostart(true).map(|_| "enabled".to_string()),
            Some("off") => sys::set_autostart(false).map(|_| "disabled".to_string()),
            _ => Ok(if sys::autostart_enabled() { "enabled" } else { "disabled" }.to_string()),
        };
        println!("{}", msg.unwrap_or_else(|e| format!("error: {e}")));
        return;
    }
    sys::load_config();
    trim_memory();
    if let Err(e) = hook::run(on_trigger) {
        eprintln!("failed to start: {e}");
    }
}
