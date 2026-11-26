//! Remembers what the last action changed so it can be undone.
//!
//! Output helpers report the files they create; moves and renames report both
//! ends. Undo sends created files to the Recycle Bin and moves things back, so
//! nothing is ever destroyed outright.

use crate::config;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Step {
    Created(PathBuf),
    Moved { from: PathBuf, to: PathBuf },
}

#[derive(Serialize, Deserialize)]
struct Entry {
    label: String,
    steps: Vec<Step>,
}

thread_local! {
    static STEPS: RefCell<Vec<Step>> = const { RefCell::new(Vec::new()) };
}

pub fn begin() {
    STEPS.with_borrow_mut(Vec::clear);
}

/// Call with a path that does not exist yet and is about to be written.
pub fn created(path: &Path) {
    STEPS.with_borrow_mut(|s| s.push(Step::Created(path.to_path_buf())));
}

pub fn moved(from: &Path, to: &Path) {
    STEPS.with_borrow_mut(|s| s.push(Step::Moved { from: from.to_path_buf(), to: to.to_path_buf() }));
}

/// Takes the steps recorded on this thread that actually happened.
fn take() -> Vec<Step> {
    STEPS
        .with_borrow_mut(std::mem::take)
        .into_iter()
        .filter(|s| match s {
            Step::Created(p) => p.exists(),
            Step::Moved { to, .. } => to.exists(),
        })
        .collect()
}

fn file() -> Option<PathBuf> {
    config::data_dir().map(|d| d.join("undo.json"))
}

/// Saves what happened as the undoable action. Returns false if nothing did.
pub fn commit(label: &str) -> bool {
    let steps = take();
    if steps.is_empty() {
        return false;
    }
    let Some(file) = file() else { return false };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let entry = Entry { label: label.to_string(), steps };
    serde_json::to_vec(&entry).ok().is_some_and(|b| std::fs::write(file, b).is_ok())
}

fn load() -> Option<Entry> {
    serde_json::from_slice(&std::fs::read(file()?).ok()?).ok()
}

/// Name of the action that `undo` would reverse.
pub fn pending() -> Option<String> {
    load().map(|e| e.label)
}

/// Reverses `steps`, newest first. `discard` removes a created file.
fn reverse(steps: &[Step], discard: impl Fn(&[PathBuf]) -> Result<(), String>) -> Result<(usize, usize), String> {
    let (mut done, mut failed) = (0, 0);
    let mut created = Vec::new();
    for step in steps.iter().rev() {
        match step {
            Step::Created(p) if p.exists() => created.push(p.clone()),
            Step::Created(_) => {}
            Step::Moved { from, to } => {
                // Never overwrite whatever has taken the old name since.
                let blocked = from.exists() && !same_file_name_ignoring_case(from, to);
                if !blocked && to.exists() && crate::transfer::relocate(to, from).is_ok() {
                    done += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }
    if !created.is_empty() {
        discard(&created)?;
        done += created.len();
    }
    Ok((done, failed))
}

fn same_file_name_ignoring_case(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

pub fn undo() -> Result<String, String> {
    let entry = load().ok_or("Nothing to undo")?;
    let (done, failed) = reverse(&entry.steps, crate::sys::recycle)?;
    if let Some(f) = file() {
        let _ = std::fs::remove_file(f);
    }
    match (done, failed) {
        (0, 0) => Err("Those files are already gone".into()),
        (_, 0) => Ok(format!("Undid {}", entry.label)),
        _ => Err(format!("Undid {done}, couldn't restore {failed}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_journal_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_real_changes_are_kept() {
        let d = dir("keep");
        begin();
        created(&d.join("never.txt"));
        let made = d.join("made.txt");
        created(&made);
        std::fs::write(&made, "x").unwrap();
        assert_eq!(take(), vec![Step::Created(made)]);
    }

    #[test]
    fn reversing_moves_back_without_overwriting() {
        let d = dir("back");
        let (a, b, c) = (d.join("a.txt"), d.join("b.txt"), d.join("c.txt"));
        std::fs::write(&b, "moved").unwrap();
        std::fs::write(&c, "moved too").unwrap();
        let taken = d.join("taken.txt");
        std::fs::write(&taken, "new owner").unwrap();
        let steps = [Step::Moved { from: a.clone(), to: b.clone() }, Step::Moved { from: taken.clone(), to: c.clone() }];
        assert_eq!(reverse(&steps, |_| Ok(())).unwrap(), (1, 1));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "moved");
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "new owner");
        assert!(c.exists() && !b.exists());
    }
}
