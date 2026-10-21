//! Remembers which action the user runs most on each file type.
//!
//! One small tab-separated file: `type<TAB>action-id<TAB>count<TAB>last-used`.
//! The "usual" action is the most-used one; ties go to the most recent.

use std::path::{Path, PathBuf};

const MAX_ROWS: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    key: String,
    id: String,
    count: u64,
    seq: u64,
}

fn default_file() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("RadialActions").join("recent.txt"))
}

fn load(file: &Path) -> Vec<Row> {
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(Row {
                key: f.next()?.to_string(),
                id: f.next()?.to_string(),
                count: f.next()?.parse().ok()?,
                seq: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

fn save(file: &Path, rows: &[Row]) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: String = rows.iter().map(|r| format!("{}\t{}\t{}\t{}\n", r.key, r.id, r.count, r.seq)).collect();
    let _ = std::fs::write(file, text);
}

pub fn record_in(file: &Path, key: &str, id: &str) {
    let mut rows = load(file);
    let seq = rows.iter().map(|r| r.seq).max().unwrap_or(0) + 1;
    match rows.iter_mut().find(|r| r.key == key && r.id == id) {
        Some(r) => {
            r.count += 1;
            r.seq = seq;
        }
        None => rows.push(Row { key: key.into(), id: id.into(), count: 1, seq }),
    }
    if rows.len() > MAX_ROWS {
        // Drop the least recently used rows first.
        rows.sort_by_key(|r| std::cmp::Reverse(r.seq));
        rows.truncate(MAX_ROWS);
    }
    save(file, &rows);
}

pub fn top_in(file: &Path, key: &str) -> Option<String> {
    load(file)
        .into_iter()
        .filter(|r| r.key == key)
        .max_by_key(|r| (r.count, r.seq))
        .map(|r| r.id)
}

pub fn record(key: &str, id: &str) {
    if let Some(f) = default_file() {
        record_in(&f, key, id);
    }
}

pub fn top(key: &str) -> Option<String> {
    top_in(&default_file()?, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_recent_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("recent.txt")
    }

    #[test]
    fn empty_history_has_no_usual() {
        assert_eq!(top_in(&file("none"), "png"), None);
    }

    #[test]
    fn most_used_wins_per_type() {
        let f = file("freq");
        for _ in 0..3 {
            record_in(&f, "png", "Convert/JPG");
        }
        record_in(&f, "png", "PDF");
        record_in(&f, "txt", "PDF");
        assert_eq!(top_in(&f, "png").as_deref(), Some("Convert/JPG"));
        assert_eq!(top_in(&f, "txt").as_deref(), Some("PDF"));
        assert_eq!(top_in(&f, "zip"), None);
    }

    #[test]
    fn ties_go_to_the_most_recent() {
        let f = file("tie");
        record_in(&f, "png", "Gray");
        record_in(&f, "png", "PDF");
        assert_eq!(top_in(&f, "png").as_deref(), Some("PDF"));
        record_in(&f, "png", "Gray");
        // Gray now has 2 uses against PDF's 1.
        assert_eq!(top_in(&f, "png").as_deref(), Some("Gray"));
    }

    #[test]
    fn survives_junk_lines_and_caps_size() {
        let f = file("junk");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, "garbage\n\tonly\tthree\nx\ty\tnotanumber\t1\n").unwrap();
        assert_eq!(top_in(&f, "x"), None);
        for i in 0..(MAX_ROWS + 25) {
            record_in(&f, "k", &format!("action{i}"));
        }
        assert_eq!(load(&f).len(), MAX_ROWS);
        // The newest survive.
        assert_eq!(top_in(&f, "k").as_deref(), Some(format!("action{}", MAX_ROWS + 24).as_str()));
    }
}
