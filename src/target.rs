//! Finds the file or folder under a screen point in Explorer or on the desktop.
//!
//! UI Automation names the item under the cursor, then the owning shell view
//! is asked for the real path. Everything is created on demand and released
//! afterwards so nothing stays resident while idle.

use std::path::PathBuf;
use windows::core::{w, Interface, BSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Com::{
    CoCreateInstance, IServiceProvider, CLSCTX_ALL, CLSCTX_INPROC_SERVER,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Children,
    UIA_BoundingRectanglePropertyId, UIA_DataItemControlTypeId, UIA_ListItemControlTypeId,
    UIA_NamePropertyId,
};
use windows::Win32::UI::Shell::{
    IFolderView, IShellBrowser, IShellItemArray, IShellWindows, ShellWindows, SIGDN_FILESYSPATH,
    SIGDN_NORMALDISPLAY, SVGIO_ALLVIEW, SVGIO_SELECTION, SWC_DESKTOP, SWFO_NEEDDISPATCH,
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
        let name = if desktop { desktop_item_name_at(hit, pt)? } else { item_name_at(pt)? };
        let view = folder_view_for(root)?;
        let path = path_for_name(&view, &name)?;
        Some((view, path))
    }
}

unsafe fn item_name_at(pt: POINT) -> Option<String> {
    let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
    let walker = uia.ControlViewWalker().ok()?;
    let mut el: IUIAutomationElement = uia.ElementFromPoint(pt).ok()?;
    // In details view the point lands on a cell; climb to the row.
    for _ in 0..4 {
        let ct = el.CurrentControlType().ok()?;
        if ct == UIA_ListItemControlTypeId || ct == UIA_DataItemControlTypeId {
            let name: BSTR = el.CurrentName().ok()?;
            let s = name.to_string();
            return (!s.is_empty()).then_some(s);
        }
        el = walker.GetParentElement(&el).ok()?;
    }
    None
}

unsafe fn desktop_item_name_at(hit: HWND, pt: POINT) -> Option<String> {
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
            return Some(el.CachedName().ok()?.to_string());
        }
    }
    None
}

/// Shell view of the Explorer window `root`, or of the desktop.
unsafe fn folder_view_for(root: HWND) -> Option<IFolderView> {
    let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;

    let class = window_class(root);
    let disp = if class == "Progman" || class == "WorkerW" {
        let mut found = 0i32;
        windows
            .FindWindowSW(
                &VARIANT::from(CSIDL_DESKTOP),
                &VARIANT::default(),
                SWC_DESKTOP,
                &mut found,
                SWFO_NEEDDISPATCH,
            )
            .ok()?
    } else {
        let count = windows.Count().ok()?;
        let mut hit = None;
        for i in 0..count {
            let Ok(d) = windows.Item(&VARIANT::from(i)) else { continue };
            let Ok(app) = d.cast::<windows::Win32::UI::Shell::IWebBrowserApp>() else { continue };
            if app.HWND().map(|h| h.0 as isize) == Ok(root.0 as isize) {
                hit = Some(d);
                break;
            }
        }
        hit?
    };

    let sp: IServiceProvider = disp.cast().ok()?;
    let browser: IShellBrowser = sp.QueryService(&SID_STOP_LEVEL_BROWSER).ok()?;
    browser.QueryActiveShellView().ok()?.cast().ok()
}

unsafe fn path_for_name(view: &IFolderView, name: &str) -> Option<PathBuf> {
    let items: IShellItemArray = view.Items(SVGIO_ALLVIEW).ok()?;
    for i in 0..items.GetCount().ok()? {
        let item = items.GetItemAt(i).ok()?;
        let shown = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        if shown.to_string().ok()? == name {
            let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            return p.to_string().ok().map(PathBuf::from);
        }
    }
    None
}

fn window_class(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}
