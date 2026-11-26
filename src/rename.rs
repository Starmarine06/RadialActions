//! Bulk rename: find/replace, prefix, suffix, case and numbering.
//!
//! Applying goes through temporary names so swaps and case-only changes work,
//! and everything is put back if any step fails.

use crate::journal;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub find: String,
    pub replace: String,
    pub prefix: String,
    pub suffix: String,
    /// "keep", "lower", "upper" or "title".
    pub case: String,
    pub number: bool,
    pub start: u32,
    pub digits: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Preview {
    pub old: String,
    pub new: String,
    pub error: Option<String>,
}

fn title_case(s: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = !c.is_alphanumeric() && c != '\'';
    }
    out
}

fn new_name(path: &Path, rules: &Rules, index: usize) -> String {
    let full = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let ext = path.extension().and_then(|e| e.to_str()).filter(|_| !path.is_dir());
    let mut stem = match ext {
        Some(e) => full[..full.len() - e.len() - 1].to_string(),
        None => full.clone(),
    };
    if !rules.find.is_empty() {
        stem = stem.replace(&rules.find, &rules.replace);
    }
    stem = match rules.case.as_str() {
        "lower" => stem.to_lowercase(),
        "upper" => stem.to_uppercase(),
        "title" => title_case(&stem),
        _ => stem,
    };
    stem = format!("{}{stem}{}", rules.prefix, rules.suffix);
    if rules.number {
        let n = rules.start as u64 + index as u64;
        stem = format!("{stem}{n:0width$}", width = rules.digits.min(10));
    }
    match ext {
        Some(e) => format!("{stem}.{e}"),
        None => stem,
    }
}

fn invalid(name: &str) -> Option<&'static str> {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.ends_with(|c: char| c.is_ascii_digit()));
    if name.trim().is_empty() || name.starts_with('.') && name.chars().all(|c| c == '.') {
        Some("Empty name")
    } else if name.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)) {
        Some("Character not allowed")
    } else if name.ends_with('.') || name.ends_with(' ') {
        Some("Can't end with a dot or space")
    } else if reserved {
        Some("Reserved by Windows")
    } else if name.encode_utf16().count() > 255 {
        Some("Too long")
    } else {
        None
    }
}

fn key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

fn target(path: &Path, name: &str) -> PathBuf {
    path.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

pub fn preview(paths: &[PathBuf], rules: &Rules) -> Vec<Preview> {
    let sources: HashSet<String> = paths.iter().map(|p| key(p)).collect();
    let mut taken = HashSet::new();
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let old = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let new = new_name(p, rules, i);
            let dst = target(p, &new);
            let mut error = invalid(&new).map(str::to_string);
            if error.is_none() && !taken.insert(key(&dst)) {
                error = Some("Same name twice".into());
            }
            // A name freed by this same batch is fine to take.
            if error.is_none() && dst.exists() && !sources.contains(&key(&dst)) {
                error = Some("Already exists".into());
            }
            Preview { old, new, error }
        })
        .collect()
}

pub fn apply(paths: &[PathBuf], rules: &Rules) -> Result<usize, String> {
    let plan = preview(paths, rules);
    if let Some(bad) = plan.iter().find(|p| p.error.is_some()) {
        return Err(format!("{}: {}", bad.new, bad.error.clone().unwrap_or_default()));
    }
    let changes: Vec<(PathBuf, PathBuf)> = paths
        .iter()
        .zip(&plan)
        .filter(|(_, p)| p.old != p.new)
        .map(|(src, p)| (src.clone(), target(src, &p.new)))
        .collect();

    // (current location, original, destination)
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    let rollback = |staged: &[(PathBuf, PathBuf, PathBuf)]| {
        for (now, original, _) in staged.iter().rev() {
            let _ = std::fs::rename(now, original);
        }
    };
    for (i, (src, dst)) in changes.iter().enumerate() {
        let tmp = target(src, &format!(".ra-rename-{}-{i}", std::process::id()));
        if let Err(e) = std::fs::rename(src, &tmp) {
            rollback(&staged);
            return Err(format!("{}: {e}", src.display()));
        }
        staged.push((tmp, src.clone(), dst.clone()));
    }
    for i in 0..staged.len() {
        let (tmp, _, dst) = staged[i].clone();
        let result = if dst.exists() { Err("Already exists".to_string()) } else { std::fs::rename(&tmp, &dst).map_err(|e| e.to_string()) };
        if let Err(e) = result {
            // Send the finished ones back to their temporary names first.
            for (tmp, _, dst) in staged[..i].iter().rev() {
                let _ = std::fs::rename(dst, tmp);
            }
            rollback(&staged);
            return Err(format!("{}: {e}", dst.display()));
        }
        staged[i].0 = tmp;
    }
    for (_, src, dst) in &staged {
        journal::moved(src, dst);
    }
    Ok(staged.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(name: &str, names: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let d = std::env::temp_dir().join(format!("radialactions_rename_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let paths = names
            .iter()
            .map(|n| {
                let p = d.join(n);
                std::fs::write(&p, n).unwrap();
                p
            })
            .collect();
        (d, paths)
    }

    #[test]
    fn rules_combine_and_keep_the_extension() {
        let (_, f) = files("rules", &["IMG_001.JPG", "IMG_002.JPG"]);
        let rules = Rules { find: "IMG_".into(), prefix: "trip ".into(), number: true, start: 5, digits: 3, ..Default::default() };
        let new: Vec<String> = preview(&f, &rules).into_iter().map(|p| p.new).collect();
        assert_eq!(new, ["trip 001005.JPG", "trip 002006.JPG"]);
        let title = Rules { find: "_".into(), replace: " ".into(), case: "title".into(), ..Default::default() };
        assert_eq!(preview(&f, &title)[0].new, "Img 001.JPG");
    }

    #[test]
    fn collisions_and_bad_names_block_everything() {
        let (d, f) = files("bad", &["a1.txt", "a2.txt", "keep.txt"]);
        let same = Rules { find: "1".into(), ..Default::default() };
        assert!(apply(&f[..2], &Rules { find: "a1".into(), replace: "keep".into(), ..same.clone() }).is_err());
        assert!(preview(&f[..2], &Rules { prefix: "<".into(), ..Default::default() })[0].error.is_some());
        assert!(preview(&f[..2], &Rules { find: "a1".into(), replace: "CON".into(), ..Default::default() })[0].error.is_some());
        assert!(d.join("a1.txt").exists() && d.join("a2.txt").exists());
    }

    #[test]
    fn chains_and_case_only_changes_work() {
        let (d, f) = files("chain", &["x.txt", "xx.txt"]);
        // x -> xx while xx -> xxxx
        let n = apply(&f, &Rules { find: "x".into(), replace: "xx".into(), ..Default::default() }).unwrap();
        assert_eq!(n, 2);
        assert_eq!(std::fs::read_to_string(d.join("xx.txt")).unwrap(), "x.txt");
        assert_eq!(std::fs::read_to_string(d.join("xxxx.txt")).unwrap(), "xx.txt");
        let up = apply(&[d.join("xx.txt")], &Rules { case: "upper".into(), ..Default::default() }).unwrap();
        assert_eq!(up, 1);
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.contains(&"XX.txt".to_string()), "{names:?}");
    }
}
