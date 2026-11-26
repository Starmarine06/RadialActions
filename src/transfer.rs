//! Move to / copy to a folder. Nothing is ever overwritten.

use crate::{config, journal};
use std::path::{Path, PathBuf};
use windows::core::w;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

/// `dir\name`, or `dir\name (n)` when taken.
fn free_name(dir: &Path, src: &Path) -> PathBuf {
    let full = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (stem, ext) = match src.extension().and_then(|e| e.to_str()) {
        Some(e) if !src.is_dir() => (full[..full.len() - e.len() - 1].to_string(), format!(".{e}")),
        _ => (full.clone(), String::new()),
    };
    let mut out = dir.join(&full);
    let mut n = 1;
    while out.exists() {
        out = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    out
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        // Following a link could copy half the disk, or loop forever.
        return Err(std::io::Error::other("links can't be copied"));
    }
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn remove_tree(p: &Path) -> std::io::Result<()> {
    if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Moves `from` to the free path `to`, across drives if need be. The source is
/// only removed after a complete copy.
pub fn relocate(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.parent() == to.parent() {
        return Err("The file is in use".into());
    }
    if let Err(e) = copy_tree(from, to) {
        let _ = remove_tree(to);
        return Err(e.to_string());
    }
    remove_tree(from).map_err(|e| format!("Copied, but couldn't remove the original: {e}"))
}

fn check(src: &Path, dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("Folder not found: {}", dir.display()));
    }
    if src.is_dir() {
        let (s, d) = (src.canonicalize(), dir.canonicalize());
        if let (Ok(s), Ok(d)) = (s, d) {
            if d.starts_with(&s) {
                return Err("A folder can't go inside itself".into());
            }
        }
    }
    Ok(())
}

fn label_of(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string())
}

fn counted(n: usize) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{n} items")
    }
}

pub fn move_into(paths: &[PathBuf], dir: &Path) -> Result<String, String> {
    let mut moved = 0;
    for src in paths {
        check(src, dir)?;
        if src.parent() == Some(dir) {
            continue;
        }
        let dst = free_name(dir, src);
        relocate(src, &dst).map_err(|e| format!("Moved {moved}, then failed: {e}"))?;
        journal::moved(src, &dst);
        moved += 1;
    }
    if moved == 0 {
        return Err("Already in that folder".into());
    }
    Ok(format!("Moved {} to {}", counted(moved), label_of(dir)))
}

pub fn copy_into(paths: &[PathBuf], dir: &Path) -> Result<String, String> {
    for (i, src) in paths.iter().enumerate() {
        check(src, dir)?;
        let dst = free_name(dir, src);
        journal::created(&dst);
        if let Err(e) = copy_tree(src, &dst) {
            let _ = remove_tree(&dst);
            return Err(format!("Copied {i}, then failed: {e}"));
        }
    }
    Ok(format!("Copied {} to {}", counted(paths.len()), label_of(dir)))
}

/// The system folder picker. `Ok(None)` when the user cancels.
pub fn pick_folder() -> Result<Option<PathBuf>, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let options = dialog.GetOptions().map_err(|e| e.to_string())?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).map_err(|e| e.to_string())?;
        let _ = dialog.SetTitle(w!("Choose a folder"));
        if dialog.Show(None).is_err() {
            return Ok(None);
        }
        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|e| e.to_string())?;
        let path = raw.to_string().map(PathBuf::from).map_err(|e| e.to_string());
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(Some)
    }
}

/// Destinations to offer: pinned first, then recent, without repeats.
pub fn destinations(s: &config::Settings) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for f in s.folders.iter().chain(&s.recent_folders) {
        let p = PathBuf::from(f);
        if !f.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    out.truncate(5);
    out
}

pub fn remember(dir: &Path) {
    let dir = dir.display().to_string();
    let _ = config::update(|s| {
        s.recent_folders.retain(|f| f != &dir);
        s.recent_folders.insert(0, dir);
        s.recent_folders.truncate(5);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_transfer_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dest")).unwrap();
        d
    }

    #[test]
    fn move_and_copy_never_overwrite() {
        let d = dir("safe");
        let dest = d.join("dest");
        std::fs::write(dest.join("a.txt"), "old").unwrap();
        std::fs::write(d.join("a.txt"), "new").unwrap();
        copy_into(&[d.join("a.txt")], &dest).unwrap();
        move_into(&[d.join("a.txt")], &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dest.join("a (1).txt")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(dest.join("a (2).txt")).unwrap(), "new");
        assert!(!d.join("a.txt").exists());
    }

    #[test]
    fn folders_cannot_swallow_themselves() {
        let d = dir("loop");
        assert!(move_into(&[d.clone()], &d.join("dest")).unwrap_err().contains("inside itself"));
        assert!(copy_into(&[d.clone()], &d.join("dest")).is_err());
        assert!(d.join("dest").exists());
    }

    #[test]
    fn moving_where_it_already_is_says_so() {
        let d = dir("same");
        std::fs::write(d.join("a.txt"), "x").unwrap();
        assert!(move_into(&[d.join("a.txt")], &d).is_err());
        assert!(d.join("a.txt").exists());
    }
}
