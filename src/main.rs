#![windows_subsystem = "windows"]

mod actions;
mod archive;
mod config;
mod convert;
mod custom;
mod gfx;
mod hook;
mod journal;
mod ocr;
mod office;
#[cfg(test)]
mod office_tests;
mod ooxml;
mod overlay;
mod pdf;
mod pdfword;
mod pptx;
mod recent;
mod rename;
mod shellmenu;
mod sys;
mod target;
mod textconv;
mod theme;
mod transfer;
mod ui;
mod xlsx;

use std::path::PathBuf;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, POINT};
use windows::Win32::System::Com::{
    CoFreeUnusedLibrariesEx, CoInitializeEx, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize};
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
        hook::trim_later();
        return;
    };
    let settings = config::load();
    let (title, key, root) = actions::wheel_with(&paths, &settings);
    overlay::show(pt, title, key, paths[0].clone(), root, &settings);
}

fn main() {
    unsafe {
        // Physical pixels, so cursor and UI Automation coordinates agree.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_app(true),
        Some("--background") => run_app(false),
        Some("--ui") => ui::run(&args[1..]),
        Some("--menu-run") => {
            if let Some(job) = args.get(1) {
                shellmenu::run_job(std::path::Path::new(job));
            }
        }
        Some(command) => {
            // A windowless program has to borrow the caller's console to print.
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
            cli(command, &args[1..]);
        }
    }
}

/// The tray app. `by_hand` is false when Windows started it at sign-in.
fn run_app(by_hand: bool) {
    unsafe {
        // Held until the process exits.
        let _mutex = CreateMutexW(None, false, w!("Local\\RadialActions"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Launching it again is how people look for its window.
            hook::ask_running_for_settings();
            return;
        }
    }
    if !config::load().welcomed {
        let _ = ui::spawn("welcome");
    } else if by_hand && sys::is_packaged() && !sys::autostart_enabled() {
        // A Start-menu launch should show something, not just a tray icon.
        let _ = ui::spawn("settings");
    }
    if let Err(e) = hook::run(on_trigger) {
        eprintln!("failed to start: {e}");
        std::process::exit(1);
    }
}

fn cli(command: &str, rest: &[String]) {
    let files = |from: usize| -> Vec<PathBuf> { rest.iter().skip(from).map(PathBuf::from).collect() };
    match command {
        // `--probe X Y` prints the item at one screen point; used for testing.
        "--probe" => {
            let x = rest.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let y = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            match target::targets_at(POINT { x, y }) {
                Some(paths) => paths.iter().for_each(|p| println!("{}", p.display())),
                None => println!("(no item)"),
            }
        }
        // `--list <file>...` prints the wheel the app would show for those files.
        "--list" => {
            if rest.is_empty() {
                println!("usage: --list <file>...");
            } else {
                print!("{}", actions::outline(&files(0)));
            }
        }
        // `--run "<action id>" <file>...` runs one wheel action, e.g. --run Convert/JPG a.png.
        "--run" => {
            let id = rest.first().cloned().unwrap_or_default();
            let files = files(1);
            journal::begin();
            let result = match files.len() {
                0 => Err("no file given".to_string()),
                1 => actions::run_by_id(&files[0], &id),
                _ => actions::run_by_id_many(&files, &id),
            };
            journal::commit(&id);
            report(result);
        }
        "--undo" => report(journal::undo()),
        // `--menu <file>` lists other apps' right-click entries for it.
        "--menu" => {
            let Some(file) = files(0).into_iter().next() else { return println!("usage: --menu <file>") };
            report(shellmenu::available(&file).map(|found| {
                found.iter().map(|f| format!("{:8} {}  {}", f.source, f.title, f.app)).collect::<Vec<_>>().join("
")
            }));
        }
        // `--autostart on|off|status` manages the start-with-Windows entry.
        "--autostart" => {
            let result = match rest.first().map(String::as_str) {
                Some("on") => sys::set_autostart(true).map(|_| "enabled".to_string()),
                Some("off") => sys::set_autostart(false).map(|_| "disabled".to_string()),
                _ => Ok(if sys::autostart_enabled() { "enabled" } else { "disabled" }.to_string()),
            };
            report(result);
        }
        // `--render out.png <theme> <scale> <hover|-> <group|-> <file>...` draws a wheel to a PNG.
        "--render" => report(render(rest)),
        _ => println!("options: --list, --run, --undo, --autostart, --probe, --render"),
    }
}

fn report(result: Result<String, String>) {
    match result {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            println!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn render(args: &[String]) -> Result<String, String> {
    let [out, theme, scale, hover, open, files @ ..] = args else {
        return Err("usage: --render out.png <theme> <scale> <hover|-> <group|-> <file>...".into());
    };
    let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        return Err("no file given".into());
    }
    let (title, _, root) = actions::wheel_with(&paths, &config::load());
    let open: Vec<&str> = open.split(',').filter(|s| !s.is_empty() && *s != "-").collect();
    let (pixels, w, h) = overlay::render(title, &root, theme, scale.parse().unwrap_or(1.0), &open, hover.parse().ok())
        .ok_or("could not draw that wheel")?;
    // Premultiplied BGRA to straight RGBA.
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            let a = p >> 24;
            let un = |shift: u32| (((p >> shift) & 0xFF) * 255).checked_div(a).unwrap_or(0).min(255) as u8;
            [un(16), un(8), un(0), a as u8]
        })
        .collect();
    image::save_buffer(out, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
    Ok(out.clone())
}
