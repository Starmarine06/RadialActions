//! Other apps' right-click entries ("Send with Blip", "Resize with Image
//! Resizer") as wheel slices.
//!
//! Windows 11 apps register an `IExplorerCommand` class in their package
//! manifest; older programs add items to the classic shell menu. Both are
//! called here the way Explorer calls them. Invoking happens in a throwaway
//! process (`--menu-run`), so another app's code never loads into the tray app.

use crate::config::AppAction;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::core::{Interface, BOOL, GUID, HSTRING, PCSTR, PSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, IBindCtx, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IExplorerCommand, ILFree, IShellFolder, IShellItemArray,
    SHBindToParent, SHCreateShellItemArrayFromIDLists, SHParseDisplayName, CMF_NORMAL,
    CMINVOKECOMMANDINFO, ECF_HASSUBCOMMANDS, ECF_ISSEPARATOR, ECS_DISABLED, ECS_HIDDEN, GCS_VERBW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// One entry found for a file.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Found {
    /// Menu text; nested entries read "Parent > Child".
    pub title: String,
    /// "app" (Windows 11 handler) or "classic".
    pub source: String,
    /// Class id for "app" entries.
    pub clsid: String,
    /// The owning package, when known.
    pub app: String,
    /// Where it applies: "*", "folder" or an extension.
    pub types: String,
}

/// A handler declared in an installed package's manifest.
#[derive(Debug, PartialEq, Eq)]
struct Declared {
    clsid: String,
    /// "*", ".png", "Directory".
    item_type: String,
}

fn declared_in(manifest: &str) -> Vec<Declared> {
    let Ok(doc) = roxmltree::Document::parse(manifest) else { return Vec::new() };
    let mut out = Vec::new();
    for item in doc.descendants().filter(|n| n.tag_name().name() == "ItemType") {
        let Some(item_type) = item.attribute("Type") else { continue };
        for verb in item.children().filter(|n| n.tag_name().name() == "Verb") {
            let Some(clsid) = verb.attribute("Clsid") else { continue };
            let d = Declared { clsid: clsid.trim_matches(['{', '}']).to_ascii_lowercase(), item_type: item_type.to_string() };
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

fn applies(item_type: &str, path: &Path) -> bool {
    if path.is_dir() {
        return item_type.eq_ignore_ascii_case("Directory") || item_type.eq_ignore_ascii_case("Folder");
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    item_type == "*" || item_type.strip_prefix('.').is_some_and(|t| t.eq_ignore_ascii_case(ext))
}

fn types_for(item_type: &str) -> String {
    if item_type == "*" {
        "*".into()
    } else if item_type.starts_with('.') {
        item_type[1..].to_ascii_lowercase()
    } else {
        "folder".into()
    }
}

/// (package name, handler) for every installed package that adds menu entries.
fn packaged_handlers() -> Vec<(String, Declared)> {
    // Windows Runtime calls need their own thread when the caller is single-threaded.
    std::thread::spawn(|| -> Vec<(String, Declared)> {
        use windows::Management::Deployment::PackageManager;
        let Ok(manager) = PackageManager::new() else { return Vec::new() };
        let Ok(packages) = manager.FindPackagesByUserSecurityId(&HSTRING::new()) else { return Vec::new() };
        let mut out = Vec::new();
        for package in packages {
            let Ok(location) = package.InstalledPath() else { continue };
            let Ok(text) = std::fs::read_to_string(PathBuf::from(location.to_string()).join("AppxManifest.xml")) else {
                continue;
            };
            if !text.contains("FileExplorerContextMenus") {
                continue;
            }
            let name = package.Id().and_then(|i| i.Name()).map(|n| n.to_string()).unwrap_or_default();
            out.extend(declared_in(&text).into_iter().map(|d| (name.clone(), d)));
        }
        out
    })
    .join()
    .unwrap_or_default()
}

/// A shell item list for the files, as Explorer would hand to a menu handler.
struct Pidls(Vec<*mut ITEMIDLIST>);

impl Pidls {
    fn new(paths: &[PathBuf]) -> Result<Self, String> {
        let mut out = Pidls(Vec::new());
        for p in paths {
            let mut pidl = std::ptr::null_mut();
            unsafe { SHParseDisplayName(&HSTRING::from(p.as_os_str()), None::<&IBindCtx>, &mut pidl, 0, None) }
                .map_err(|_| format!("Can't open {}", p.display()))?;
            out.0.push(pidl);
        }
        Ok(out)
    }

    fn array(&self) -> Result<IShellItemArray, String> {
        let list: Vec<*const ITEMIDLIST> = self.0.iter().map(|p| *p as *const _).collect();
        unsafe { SHCreateShellItemArrayFromIDLists(&list) }.map_err(|e| e.message())
    }
}

impl Drop for Pidls {
    fn drop(&mut self) {
        for p in &self.0 {
            unsafe { ILFree(Some(*p as *const _)) };
        }
    }
}

fn command(clsid: &str) -> Result<IExplorerCommand, String> {
    let id = GUID::try_from(clsid.trim_matches(['{', '}'])).map_err(|_| "Bad class id".to_string())?;
    unsafe { CoCreateInstance(&id, None, CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER) }
        .map_err(|e| format!("That app's menu handler didn't start: {}", e.message()))
}

fn title_of(cmd: &IExplorerCommand, items: &IShellItemArray) -> Option<String> {
    unsafe {
        let raw = cmd.GetTitle(items).ok()?;
        let title = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        title.map(|t| t.replace('&', "")).filter(|t| !t.trim().is_empty())
    }
}

/// Visible leaf commands under `cmd`, with their menu path.
fn leaves(cmd: IExplorerCommand, items: &IShellItemArray, prefix: &str, depth: u8, out: &mut Vec<(String, IExplorerCommand)>) {
    unsafe {
        let state = cmd.GetState(items, true).unwrap_or(0);
        if state & (ECS_HIDDEN.0 | ECS_DISABLED.0) as u32 != 0 {
            return;
        }
        let flags = cmd.GetFlags().unwrap_or(0);
        if flags & ECF_ISSEPARATOR.0 as u32 != 0 {
            return;
        }
        let title = title_of(&cmd, items);
        if flags & ECF_HASSUBCOMMANDS.0 as u32 != 0 {
            if depth >= 2 {
                return;
            }
            let Ok(children) = cmd.EnumSubCommands() else { return };
            let prefix = match &title {
                Some(t) => format!("{prefix}{t} > "),
                None => prefix.to_string(),
            };
            loop {
                let mut next = [None];
                let mut got = 0;
                if children.Next(&mut next, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                let Some(child) = next[0].take() else { break };
                leaves(child, items, &prefix, depth + 1, out);
            }
        } else if let Some(title) = title {
            out.push((format!("{prefix}{title}"), cmd));
        }
    }
}

/// The classic menu for files in one folder.
struct Classic {
    menu: HMENU,
    handler: IContextMenu,
}

const FIRST_ID: u32 = 1;

impl Classic {
    fn new(paths: &[PathBuf], pidls: &Pidls) -> Result<Self, String> {
        let err = |e: windows::core::Error| e.message();
        let first = paths.first().ok_or("Nothing selected")?;
        if paths.iter().any(|p| p.parent() != first.parent()) {
            return Err("The files must be in one folder".into());
        }
        unsafe {
            let mut children = Vec::new();
            let mut folder: Option<IShellFolder> = None;
            for pidl in &pidls.0 {
                let mut child = std::ptr::null_mut();
                let parent: IShellFolder = SHBindToParent(*pidl, Some(&mut child)).map_err(err)?;
                folder.get_or_insert(parent);
                children.push(child as *const ITEMIDLIST);
            }
            let handler: IContextMenu = folder
                .ok_or("Nothing selected")?
                .GetUIObjectOf(HWND::default(), &children, None)
                .map_err(err)?;
            let menu = CreatePopupMenu().map_err(err)?;
            handler
                .QueryContextMenu(menu, 0, FIRST_ID, 0x7FFF, CMF_NORMAL)
                .ok()
                .map_err(err)?;
            Ok(Classic { menu, handler })
        }
    }

    fn verb(&self, id: u32) -> String {
        let mut buf = [0u16; 128];
        let ok = unsafe {
            self.handler.GetCommandString((id - FIRST_ID) as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr() as *mut u8), 127)
        };
        if ok.is_err() {
            return String::new();
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&buf[..len]).to_ascii_lowercase()
    }

    /// (menu path, command id) for every enabled item.
    fn items(&self) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        self.walk(self.menu, "", 0, &mut out);
        out
    }

    fn walk(&self, menu: HMENU, prefix: &str, depth: u8, out: &mut Vec<(String, u32)>) {
        let count = unsafe { GetMenuItemCount(Some(menu)) };
        for i in 0..count.max(0) {
            let mut text = [0u16; 256];
            let mut info = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_STRING | MIIM_ID | MIIM_SUBMENU | MIIM_STATE | MIIM_FTYPE,
                dwTypeData: windows::core::PWSTR(text.as_mut_ptr()),
                cch: 255,
                ..Default::default()
            };
            if unsafe { GetMenuItemInfoW(menu, i as u32, true, &mut info) }.is_err() {
                continue;
            }
            if info.fType.contains(MFT_SEPARATOR) || info.fState.contains(MFS_DISABLED) {
                continue;
            }
            let label = String::from_utf16_lossy(&text[..(info.cch as usize).min(255)]);
            // Drop accelerator marks and the shortcut column.
            let label = label.split('\t').next().unwrap_or("").replace('&', "");
            if label.trim().is_empty() {
                continue;
            }
            if !info.hSubMenu.is_invalid() {
                if depth < 2 {
                    // Many submenus are only filled in when about to open.
                    if let Ok(lazy) = self.handler.cast::<IContextMenu2>() {
                        unsafe {
                            let _ = lazy.HandleMenuMsg(WM_INITMENUPOPUP, WPARAM(info.hSubMenu.0 as usize), LPARAM(i as isize));
                        }
                    }
                    self.walk(info.hSubMenu, &format!("{prefix}{label} > "), depth + 1, out);
                }
                continue;
            }
            out.push((format!("{prefix}{label}"), info.wID));
        }
    }

    fn invoke(&self, id: u32, owner: HWND) -> Result<(), String> {
        let info = CMINVOKECOMMANDINFO {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
            hwnd: owner,
            // A number in place of a string means "the item with this offset".
            lpVerb: PCSTR((id - FIRST_ID) as usize as *const u8),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        unsafe { self.handler.InvokeCommand(&info) }.map_err(|e| e.message())
    }
}

impl Drop for Classic {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.menu);
        }
    }
}

/// Everyday verbs the wheel already covers or Explorer does better.
const ORDINARY: [&str; 10] = ["open", "cut", "copy", "delete", "rename", "properties", "link", "paste", "copyaspath", "opennewwindow"];

/// Every other-app entry offered for `path`.
pub fn available(path: &Path) -> Result<Vec<Found>, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let paths = [path.to_path_buf()];
    let pidls = Pidls::new(&paths)?;
    let items = pidls.array()?;
    let mut out: Vec<Found> = Vec::new();
    for (app, declared) in packaged_handlers() {
        if !applies(&declared.item_type, path) {
            continue;
        }
        let Ok(cmd) = command(&declared.clsid) else { continue };
        let mut found = Vec::new();
        leaves(cmd, &items, "", 0, &mut found);
        for (title, _) in found {
            if !out.iter().any(|f| f.title == title) {
                out.push(Found {
                    title,
                    source: "app".into(),
                    clsid: declared.clsid.clone(),
                    app: app.clone(),
                    types: types_for(&declared.item_type),
                });
            }
        }
    }
    if let Ok(classic) = Classic::new(&paths, &pidls) {
        let types = if path.is_dir() {
            "folder".to_string()
        } else {
            path.extension().and_then(|e| e.to_str()).unwrap_or("*").to_ascii_lowercase()
        };
        for (title, id) in classic.items() {
            if ORDINARY.contains(&classic.verb(id).as_str()) || out.iter().any(|f| f.title == title) {
                continue;
            }
            out.push(Found { title, source: "classic".into(), clsid: String::new(), app: String::new(), types: types.clone() });
        }
    }
    Ok(out)
}

/// Runs messages while the called app does its work. Stays while it has a
/// window of ours open (classic handlers show dialogs inside the caller).
fn pump() {
    unsafe extern "system" fn visible(hwnd: HWND, found: LPARAM) -> BOOL {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() && IsWindowVisible(hwnd).as_bool() {
            *(found.0 as *mut bool) = true;
        }
        true.into()
    }
    let started = Instant::now();
    let mut quiet_since = Instant::now();
    while started.elapsed() < Duration::from_secs(1800) {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let mut open = false;
        unsafe {
            let _ = EnumWindows(Some(visible), LPARAM(&mut open as *mut bool as isize));
        }
        if open {
            quiet_since = Instant::now();
        }
        if started.elapsed() > Duration::from_secs(5) && quiet_since.elapsed() > Duration::from_secs(3) {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Calls the entry on `paths`, in this process.
pub fn invoke(action: &AppAction, paths: &[PathBuf]) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let pidls = Pidls::new(paths)?;
    let missing = || format!("'{}' isn't offered for this file", action.title);
    if action.source == "app" {
        let items = pidls.array()?;
        let mut found = Vec::new();
        leaves(command(&action.clsid)?, &items, "", 0, &mut found);
        // A lone command may reword itself ("Send 3 files with X"), so fall back to it.
        let at = found.iter().position(|(t, _)| *t == action.title).or((found.len() == 1).then_some(0)).ok_or_else(missing)?;
        unsafe { found[at].1.Invoke(&items, None::<&IBindCtx>) }.map_err(|e| e.message())?;
        pump();
    } else {
        let classic = Classic::new(paths, &pidls)?;
        let (_, id) = classic.items().into_iter().find(|(t, _)| *t == action.title).ok_or_else(missing)?;
        classic.invoke(id, HWND::default())?;
        pump();
    }
    Ok(())
}

/// Starts the helper process that calls `action`. Returns once it has either
/// reported a failure or had time to hand the files over.
pub fn launch(action: &AppAction, paths: &[PathBuf]) -> Result<String, String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let job = std::env::temp_dir().join(format!("radialactions_menu_{}_{n}.json", std::process::id()));
    let bytes = serde_json::to_vec(&(action, paths)).map_err(|e| e.to_string())?;
    std::fs::write(&job, bytes).map_err(|e| e.to_string())?;
    let result = job.with_extension("result");
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(exe).arg("--menu-run").arg(&job).spawn().map_err(|e| e.to_string())?;
    unsafe {
        // Let the other app bring its window to the front.
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(4) {
        if result.exists() || child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let failure = std::fs::read_to_string(&result).ok().filter(|s| !s.is_empty());
    let crashed = child.try_wait().ok().flatten().is_some_and(|s| !s.success());
    let _ = std::fs::remove_file(&result);
    match failure {
        Some(e) => Err(e),
        None if crashed => Err(format!("{} failed", action.label)),
        // The other app shows its own window, so no pop-up is needed.
        None => Ok(String::new()),
    }
}

/// Body of `--menu-run <job>`: calls the entry and reports a failure to a file.
pub fn run_job(job: &Path) {
    let parsed: Option<(AppAction, Vec<PathBuf>)> = std::fs::read(job).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let _ = std::fs::remove_file(job);
    let Some((action, paths)) = parsed else { return };
    if let Err(e) = invoke(&action, &paths) {
        let _ = std::fs::write(job.with_extension("result"), e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handlers_are_read_from_a_package_manifest() {
        let xml = r#"<Package xmlns:d4="urn:4" xmlns:d5="urn:5"><d4:FileExplorerContextMenus>
          <d5:ItemType Type="*"><d5:Verb Id="A" Clsid="160A73D9-5263-41b8-b9c7-930db8da5d5c"/></d5:ItemType>
          <d5:ItemType Type="Directory"><d5:Verb Id="A" Clsid="{160a73d9-5263-41b8-b9c7-930db8da5d5c}"/></d5:ItemType>
        </d4:FileExplorerContextMenus></Package>"#;
        let found = declared_in(xml);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].clsid, "160a73d9-5263-41b8-b9c7-930db8da5d5c");
        assert!(applies("*", Path::new("a.png")) && !applies("Directory", Path::new("a.png")));
        assert!(applies(".PNG", Path::new("a.png")) && applies("Directory", &std::env::temp_dir()));
        assert_eq!((types_for(".Png"), types_for("Directory")), ("png".to_string(), "folder".to_string()));
    }
}
