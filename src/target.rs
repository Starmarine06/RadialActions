//! Finds the file or folder under a screen point in Explorer or on the desktop.
//!
//! UI Automation names the item under the cursor, then the owning shell view
//! is asked for the real path. Everything is created on demand and released
//! afterwards so nothing stays resident while idle.

use std::path::PathBuf;
use windows::core::{w, Interface, BSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Com::{
    CoCreateInstance, IDispatch, IServiceProvider, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Children,
    UIA_BoundingRectanglePropertyId, UIA_DataItemControlTypeId, UIA_ListItemControlTypeId,
    UIA_NamePropertyId,
};
use windows::Win32::UI::Shell::{
    IFolderView, IShellBrowser, IShellItemArray, IShellWindows, IWebBrowserApp, ShellWindows,
    SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY, SVGIO_ALLVIEW, SVGIO_FLAG_VIEWORDER, SVGIO_SELECTION,
    SWC_DESKTOP, SWFO_NEEDDISPATCH, _SVGIO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetAncestor, WindowFromPoint, GA_ROOT,
};

const CSIDL_DESKTOP: i32 = 0;
// {4C96BE40-915C-11CF-99D3-00AA004AE837}
const SID_STOP_LEVEL_BROWSER: windows::core::GUID =
    windows::core::GUID::from_u128(0x4c96be40_915c_11cf_99d3_00aa004ae837);

/// The hovered item, or every selected item when the hovered one is part of a
/// multi-selection. Always at least one path.
pub fn targets_at(pt: POINT) -> Option<Vec<PathBuf>> {
    unsafe {
        let (view, hovered) = resolve(pt)?;
        let selected = selected_paths(&view);
        if selected.len() > 1 && selected.contains(&hovered) {
            Some(selected)
        } else {
            Some(vec![hovered])
        }
    }
}

/// Selected items with a real file-system path. Huge selections are ignored.
unsafe fn selected_paths(view: &IFolderView) -> Vec<PathBuf> {
    let Ok(items) = view.Items::<IShellItemArray>(SVGIO_SELECTION) else { return Vec::new() };
    let count = items.GetCount().unwrap_or(0);
    if count > 2000 {
        return Vec::new();
    }
    (0..count)
        .filter_map(|i| {
            let item = items.GetItemAt(i).ok()?;
            item.GetDisplayName(SIGDN_FILESYSPATH).ok()?.to_string().ok().map(PathBuf::from)
        })
        .collect()
}

unsafe fn resolve(pt: POINT) -> Option<(IFolderView, PathBuf)> {
    {
        let hit = WindowFromPoint(pt);
        let root = GetAncestor(hit, GA_ROOT);
        let desktop = matches!(window_class(root).as_str(), "Progman" | "WorkerW");
        // Overlays (GPU HUDs etc.) can sit above the desktop and swallow UIA
        // point queries, so the desktop is matched against its own icon list.
        let (name, index) = if desktop { desktop_item_at(hit, pt)? } else { item_at(pt)? };
        let view = folder_view_for(root)?;
        let path = path_for(&view, &name, index)?;
        Some((view, path))
    }
}

/// Name and, when Explorer exposes it, view position of the item under `pt`.
unsafe fn item_at(pt: POINT) -> Option<(String, Option<usize>)> {
    let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let walker = uia.ControlViewWalker().ok()?;
    let mut el: IUIAutomationElement = uia.ElementFromPoint(pt).ok()?;
    // In details view the point lands on a cell; climb to the row.
    for _ in 0..4 {
        let ct = el.CurrentControlType().ok()?;
        if ct == UIA_ListItemControlTypeId || ct == UIA_DataItemControlTypeId {
            let name: BSTR = el.CurrentName().ok()?;
            let s = name.to_string();
            let index = el.CurrentAutomationId().ok().and_then(|id| id.to_string().parse().ok());
            return (!s.is_empty()).then_some((s, index));
        }
        el = walker.GetParentElement(&el).ok()?;
    }
    None
}

unsafe fn desktop_item_at(hit: HWND, pt: POINT) -> Option<(String, Option<usize>)> {
    let list = if window_class(hit) == "SysListView32" {
        hit
    } else {
        FindWindowExW(Some(hit), None, w!("SysListView32"), None).ok()?
    };
    let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let cache = uia.CreateCacheRequest().ok()?;
    cache.AddProperty(UIA_NamePropertyId).ok()?;
    cache.AddProperty(UIA_BoundingRectanglePropertyId).ok()?;
    let items = uia
        .ElementFromHandle(list)
        .ok()?
        .FindAllBuildCache(TreeScope_Children, &uia.CreateTrueCondition().ok()?, &cache)
        .ok()?;
    for i in 0..items.Length().ok()? {
        let el = items.GetElement(i).ok()?;
        let r = el.CachedBoundingRectangle().ok()?;
        if pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom {
            // List-view children come back in item order, which is view order.
            return Some((el.CachedName().ok()?.to_string(), Some(i as usize)));
        }
    }
    None
}

/// Shell view of the Explorer window `root`, or of the desktop.
unsafe fn folder_view_for(root: HWND) -> Option<IFolderView> {
    let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;

    let class = window_class(root);
    if class == "Progman" || class == "WorkerW" {
        let mut found = 0i32;
        let disp = windows
            .FindWindowSW(
                &VARIANT::from(CSIDL_DESKTOP),
                &VARIANT::default(),
                SWC_DESKTOP,
                &mut found,
                SWFO_NEEDDISPATCH,
            )
            .ok()?;
        return view_of(&browser_of(&disp)?);
    }

    // Windows 11 lists every tab as its own window with the same HWND. The
    // active tab is the topmost tab child of the frame.
    let active = FindWindowExW(Some(root), None, w!("ShellTabWindowClass"), None).ok();
    let mut fallback = None;
    for i in 0..windows.Count().ok()? {
        let Ok(d) = windows.Item(&VARIANT::from(i)) else { continue };
        let Ok(app) = d.cast::<IWebBrowserApp>() else { continue };
        if app.HWND().map(|h| h.0 as isize) != Ok(root.0 as isize) {
            continue;
        }
        let Some(browser) = browser_of(&d) else { continue };
        match active {
            Some(tab) if browser.GetWindow().ok() != Some(tab) => {
                fallback.get_or_insert(browser);
            }
            _ => return view_of(&browser),
        }
    }
    // Builds without tab windows (Windows 10) have only the one entry.
    match active {
        Some(_) => None,
        None => view_of(&fallback?),
    }
}

unsafe fn browser_of(disp: &IDispatch) -> Option<IShellBrowser> {
    let sp: IServiceProvider = disp.cast().ok()?;
    sp.QueryService(&SID_STOP_LEVEL_BROWSER).ok()
}

unsafe fn view_of(browser: &IShellBrowser) -> Option<IFolderView> {
    browser.QueryActiveShellView().ok()?.cast().ok()
}

/// The real path of the hovered item. Display names repeat when extensions are
/// hidden ("report.pdf" and "report.docx" both read "report"), so the view
/// position decides, and an ambiguous name without one is refused rather than
/// guessed: acting on the wrong file is worse than doing nothing.
unsafe fn path_for(view: &IFolderView, name: &str, index: Option<usize>) -> Option<PathBuf> {
    let items: IShellItemArray = view
        .Items(_SVGIO(SVGIO_ALLVIEW.0 | SVGIO_FLAG_VIEWORDER.0 as i32))
        .ok()?;
    let count = items.GetCount().ok()?;
    let mut names = Vec::new();
    for i in 0..count {
        let shown = items.GetItemAt(i).ok()?.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        names.push(shown.to_string().ok()?);
    }
    let at = pick_index(&names, name, index)?;
    let p = items.GetItemAt(at as u32).ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    p.to_string().ok().map(PathBuf::from)
}

fn pick_index(names: &[String], name: &str, index: Option<usize>) -> Option<usize> {
    if let Some(i) = index {
        if names.get(i).is_some_and(|n| n == name) {
            return Some(i);
        }
    }
    let mut matches = names.iter().enumerate().filter(|(_, n)| *n == name).map(|(i, _)| i);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn window_class(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

#[cfg(test)]
mod tests {
    use super::pick_index;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn duplicate_names_follow_the_view_position() {
        let n = names(&["report", "report", "zeta"]);
        assert_eq!(pick_index(&n, "report", Some(1)), Some(1));
        assert_eq!(pick_index(&n, "report", Some(0)), Some(0));
    }

    #[test]
    fn an_ambiguous_name_is_never_guessed() {
        let n = names(&["report", "report", "zeta"]);
        assert_eq!(pick_index(&n, "report", None), None);
        assert_eq!(pick_index(&n, "report", Some(2)), None);
        assert_eq!(pick_index(&n, "zeta", Some(0)), Some(2));
        assert_eq!(pick_index(&n, "missing", Some(0)), None);
    }
}
