//! The settings, welcome and rename windows.
//!
//! They run as separate short-lived processes (`radialactions --ui <page>`), so
//! the tray app never loads a browser engine. The page is bundled HTML; it
//! talks to Rust with small JSON messages.

use crate::config::{self, Settings};
use crate::{actions, hook, journal, overlay, rename, sys, theme, transfer};
use serde_json::{json, Value};
use std::path::PathBuf;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::IconExtWindows;
use tao::window::{Icon, Theme, WindowBuilder};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindow, MB_ICONERROR, SW_RESTORE, SW_SHOWNORMAL,
};
use wry::{WebContext, WebViewBuilder};

const PAGE: &str = include_str!("../ui/app.html");

fn title_of(page: &str) -> &'static str {
    match page {
        "welcome" => "Welcome to RadialActions",
        "rename" => "Rename with RadialActions",
        _ => "RadialActions Settings",
    }
}

/// Opens a window in its own process.
pub fn spawn(page: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe).args(["--ui", page]).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Opens the rename window. The list goes through a file: a big selection
/// would not fit on a command line.
pub fn open_rename(paths: &[PathBuf]) -> Result<(), String> {
    let list = std::env::temp_dir().join(format!("radialactions_rename_{}.json", std::process::id()));
    let bytes = serde_json::to_vec(paths).map_err(|e| e.to_string())?;
    std::fs::write(&list, bytes).map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .args(["--ui", "rename"])
        .arg(list)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The real renderer's picture of a wheel, as a data URL.
fn preview(settings: &Settings, category: &str) -> Result<String, String> {
    let (title, files): (&str, &[&str]) = match category {
        "pdf" => ("report.pdf", &["report.pdf"]),
        "word" => ("letter.docx", &["letter.docx"]),
        "excel" => ("budget.xlsx", &["budget.xlsx"]),
        "powerpoint" => ("pitch.pptx", &["pitch.pptx"]),
        "data" => ("data.json", &["data.json"]),
        "web" => ("notes.md", &["notes.md"]),
        "text" => ("todo.txt", &["todo.txt"]),
        "archive" => ("backup.zip", &["backup.zip"]),
        "folder" => ("My folder", &[]),
        "other" => ("setup.bin", &["setup.bin"]),
        "many" => ("3 files selected", &["a.pdf", "b.pdf", "c.pdf"]),
        _ => ("holiday.png", &["holiday.png"]),
    };
    let paths: Vec<PathBuf> = if files.is_empty() {
        vec![std::env::temp_dir()]
    } else {
        files.iter().map(PathBuf::from).collect()
    };
    let (_, _, root) = actions::wheel_with(&paths, settings);
    let (pixels, w, h) = overlay::render(title.into(), &root, &settings.theme, 1.25, &[], Some(0))
        .ok_or("Nothing to show")?;
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            let a = p >> 24;
            let un = |shift: u32| (((p >> shift) & 0xFF) * 255).checked_div(a).unwrap_or(0).min(255) as u8;
            [un(16), un(8), un(0), a as u8]
        })
        .collect();
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba).ok_or("Bad picture")?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(format!("data:image/png;base64,{}", base64(png.get_ref())))
}

/// A folder of harmless sample files to try the wheel on.
fn practice_folder() -> Result<(), String> {
    let home = std::env::var_os("USERPROFILE").ok_or("No user folder")?;
    let dir = PathBuf::from(home).join("Documents").join("RadialActions Practice");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let picture = dir.join("Sunset.png");
    if !picture.exists() {
        let img = image::RgbImage::from_fn(960, 640, |x, y| {
            let (fx, fy) = (x as f32 / 960.0, y as f32 / 640.0);
            if (x as f32 - 480.0).hypot(y as f32 - 400.0) < 110.0 && fy < 0.68 {
                image::Rgb([255, 214, 102])
            } else if fy < 0.68 {
                image::Rgb([(60.0 + 190.0 * fy) as u8, (40.0 + 110.0 * fy) as u8, (120.0 + 40.0 * (1.0 - fy)) as u8])
            } else {
                image::Rgb([(30.0 + 40.0 * fx) as u8, 45, (90.0 - 60.0 * fy) as u8])
            }
        });
        img.save(&picture).map_err(|e| e.to_string())?;
    }
    for (name, text) in [
        ("Shopping list.txt", "Coffee\nBread\nApples\n"),
        ("Scores.csv", "name,score\nAsha,91\nBen,84\nChen,88\n"),
        ("Notes.md", "# Trip notes\n\n- Pack the charger\n- Book the early train\n"),
    ] {
        let p = dir.join(name);
        if !p.exists() {
            std::fs::write(p, text).map_err(|e| e.to_string())?;
        }
    }
    std::process::Command::new("explorer.exe").arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn open_external(what: &str) -> Result<(), String> {
    // A fixed list: the page can't ask to open anything else.
    let url = match what {
        "source" => "https://github.com/Starmarine06/RadialActions",
        "issues" => "https://github.com/Starmarine06/RadialActions/issues",
        "privacy" => "https://github.com/Starmarine06/RadialActions/blob/master/PRIVACY.md",
        "startup" => "ms-settings:startupapps",
        _ => return Err("Unknown link".into()),
    };
    let url = HSTRING::from(url);
    unsafe { ShellExecuteW(None, w!("open"), &url, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    Ok(())
}

fn incoming(args: &Value) -> Result<Settings, String> {
    serde_json::from_value::<Settings>(args.clone()).map(Settings::sanitised).map_err(|e| e.to_string())
}

fn handle(command: &str, args: &Value, files: &[PathBuf]) -> Result<Value, String> {
    match command {
        "load" => {
            let catalogue: Vec<Value> = actions::catalogue()
                .into_iter()
                .map(|(key, title, slices)| json!({"key": key, "title": title, "slices": slices}))
                .collect();
            let accent = theme::accent();
            Ok(json!({
                "settings": config::load(),
                "catalogue": catalogue,
                "autostart": sys::autostart_enabled(),
                "version": env!("CARGO_PKG_VERSION"),
                "systemLight": theme::system_is_light(),
                "accent": format!("#{:02x}{:02x}{:02x}", accent.0 as u8, accent.1 as u8, accent.2 as u8),
                "undo": journal::pending(),
                "files": files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            }))
        }
        "save" => {
            let mut s = incoming(args)?;
            let current = config::load();
            // The tray app adds these while the window is open.
            s.recent_folders = current.recent_folders;
            config::save(&s)?;
            hook::notify_reload();
            Ok(Value::Null)
        }
        "preview" => preview(&incoming(&args["settings"])?, args["category"].as_str().unwrap_or("image")).map(Value::from),
        "autostart" => {
            let result = sys::set_autostart(args.as_bool().unwrap_or(false));
            let now = sys::autostart_enabled();
            result.map(|_| Value::from(now))
        }
        "pickFolder" => Ok(transfer::pick_folder()?.map(|p| Value::from(p.display().to_string())).unwrap_or(Value::Null)),
        "pickProgram" => Ok(pick_program()?.map(Value::from).unwrap_or(Value::Null)),
        "practice" => practice_folder().map(|_| Value::Null),
        "undo" => journal::undo().map(Value::from),
        "open" => open_external(args.as_str().unwrap_or("")).map(|_| Value::Null),
        "renamePreview" => {
            let rules: rename::Rules = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            serde_json::to_value(rename::preview(files, &rules)).map_err(|e| e.to_string())
        }
        "renameApply" => {
            let rules: rename::Rules = serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            journal::begin();
            let n = rename::apply(files, &rules)?;
            journal::commit("Rename");
            Ok(Value::from(n))
        }
        _ => Err("Unknown request".into()),
    }
}

fn pick_program() -> Result<Option<String>, String> {
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let _ = dialog.SetTitle(w!("Choose a program"));
        if dialog.Show(None).is_err() {
            return Ok(None);
        }
        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
        let path = raw.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(Some)
    }
}

fn fail(message: &str) {
    let text = HSTRING::from(message);
    unsafe {
        MessageBoxW(None, &text, w!("RadialActions"), MB_ICONERROR);
    }
}

/// Brings an already-open window of this page to the front.
fn focus_existing(title: &str) -> bool {
    let title = HSTRING::from(title);
    unsafe {
        match FindWindowW(PCWSTR::null(), &title) {
            Ok(hwnd) if !hwnd.is_invalid() => {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
                true
            }
            _ => false,
        }
    }
}

/// Entry point for `--ui <page> [list-file]`.
pub fn run(args: &[String]) {
    let page = match args.first().map(String::as_str) {
        Some("welcome") => "welcome",
        Some("rename") => "rename",
        _ => "settings",
    };
    let mut files: Vec<PathBuf> = Vec::new();
    if page == "rename" {
        let Some(list) = args.get(1).map(PathBuf::from) else { return };
        files = std::fs::read(&list).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let _ = std::fs::remove_file(list);
        if files.is_empty() {
            return;
        }
    } else if focus_existing(title_of(page)) {
        return;
    }
    if page == "welcome" {
        let _ = config::update(|s| s.welcomed = true);
    }
    if let Err(e) = window(page, files) {
        fail(&format!(
            "The window couldn't open: {e}\n\nIt needs the Microsoft Edge WebView2 Runtime, which you can get from microsoft.com/edge/webview2."
        ));
    }
}

fn window(page: &'static str, files: Vec<PathBuf>) -> Result<(), String> {
    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let settings = config::load();
    let dark = match settings.theme.as_str() {
        "light" => false,
        "dark" | "neon" => true,
        _ => !theme::system_is_light(),
    };
    let (width, height) = match page {
        "welcome" => (780.0, 640.0),
        "rename" => (820.0, 620.0),
        _ => (1040.0, 720.0),
    };
    let window = WindowBuilder::new()
        .with_title(title_of(page))
        .with_inner_size(LogicalSize::new(width, height))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        // Resource 1 is the icon build.rs embeds.
        .with_window_icon(Icon::from_resource(1, None).ok())
        .with_theme(Some(if dark { Theme::Dark } else { Theme::Light }))
        .with_visible(false)
        .build(&event_loop)
        .map_err(|e| e.to_string())?;

    // The browser profile must live somewhere writable, not beside the exe.
    let profile = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("RadialActions").join("webview"));
    let mut context = WebContext::new(profile);
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(PAGE.replace("__PAGE__", page))
        .with_background_color(if dark { (24, 24, 27, 255) } else { (243, 243, 245, 255) })
        .with_ipc_handler(move |request| {
            let _ = proxy.send_event(request.into_body());
        })
        // The bundled page is the only thing this window ever shows.
        .with_navigation_handler(|url| url.starts_with("data:") || url.starts_with("about:"))
        .build(&window)
        .map_err(|e| e.to_string())?;
    window.set_visible(true);
    window.set_focus();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(message) => {
                let message: Value = serde_json::from_str(&message).unwrap_or_default();
                let command = message["cmd"].as_str().unwrap_or("");
                if command == "close" {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                let (ok, data) = match handle(command, &message["args"], &files) {
                    Ok(v) => (true, v),
                    Err(e) => (false, Value::from(e)),
                };
                let _ = webview.evaluate_script(&format!("window.reply({}, {ok}, {data})", message["id"].as_u64().unwrap_or(0)));
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn every_category_can_be_previewed() {
        for (key, _, slices) in actions::catalogue() {
            assert!(preview(&Settings::default(), key).is_ok(), "{key}");
            assert!(slices.contains(&"Send".to_string()), "{key}");
        }
    }
}
