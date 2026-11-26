//! The user's own actions: start a chosen program on the hovered files.
//!
//! Arguments are split before placeholders are filled in, so a file name with
//! spaces or quotes stays one argument and can't add more of them.

use crate::config::Custom;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Does this action apply to `path`?
pub fn matches(c: &Custom, path: &Path) -> bool {
    if c.name.trim().is_empty() || c.program.trim().is_empty() {
        return false;
    }
    let dir = path.is_dir();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let types: Vec<String> = c
        .types
        .split([',', ' ', ';'])
        .map(|t| t.trim().trim_start_matches("*.").trim_start_matches('.').to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if types.is_empty() || types.iter().any(|t| t == "*") {
        return true;
    }
    types.iter().any(|t| if dir { t == "folder" } else { *t == ext })
}

/// Splits on spaces, keeping double-quoted runs together.
fn tokens(template: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut started) = (Vec::new(), String::new(), false, false);
    for ch in template.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Fills the placeholders for `path`. A lone `{files}` expands to every file.
pub fn arguments(template: &str, path: &Path, all: &[PathBuf]) -> Vec<String> {
    let text = |p: Option<&std::ffi::OsStr>| p.unwrap_or_default().to_string_lossy().into_owned();
    let values = [
        ("{file}", path.display().to_string()),
        ("{dir}", path.parent().map(|d| d.display().to_string()).unwrap_or_default()),
        ("{name}", text(path.file_name())),
        ("{stem}", text(path.file_stem())),
    ];
    let template = if template.trim().is_empty() { "{file}" } else { template };
    let mut out = Vec::new();
    for token in tokens(template) {
        if token == "{files}" {
            out.extend(all.iter().map(|p| p.display().to_string()));
            continue;
        }
        // One pass, so braces inside a file name are never expanded again.
        let mut filled = String::new();
        let mut rest = token.as_str();
        while let Some(at) = rest.find('{') {
            filled.push_str(&rest[..at]);
            rest = &rest[at..];
            match values.iter().find(|(k, _)| rest.starts_with(k)) {
                Some((k, v)) => {
                    filled.push_str(v);
                    rest = &rest[k.len()..];
                }
                None => {
                    filled.push('{');
                    rest = &rest[1..];
                }
            }
        }
        filled.push_str(rest);
        out.push(filled);
    }
    out
}

/// Starts the program: once with every file for `{files}`, otherwise per file.
pub fn run(c: &Custom, paths: &[PathBuf]) -> Result<String, String> {
    let program = c.program.trim().trim_matches('"');
    let together = tokens(&c.args).iter().any(|t| t == "{files}");
    let rounds = if together { 1 } else { paths.len().min(50) };
    for path in paths.iter().take(rounds) {
        let mut cmd = Command::new(program);
        cmd.args(arguments(&c.args, path, paths));
        if let Some(dir) = path.parent() {
            cmd.current_dir(dir);
        }
        cmd.spawn().map_err(|e| format!("{}: {e}", c.name))?;
    }
    Ok(format!("Started {}", c.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(types: &str) -> Custom {
        Custom { name: "n".into(), program: "p".into(), args: String::new(), types: types.into() }
    }

    #[test]
    fn types_filter_by_extension() {
        assert!(matches(&action("*"), Path::new("a.png")));
        assert!(matches(&action("png, .JPG"), Path::new("a.jpg")));
        assert!(!matches(&action("png"), Path::new("a.pdf")));
        assert!(matches(&action("folder"), &std::env::temp_dir()));
        assert!(!matches(&Custom::default(), Path::new("a.png")), "unfinished actions stay hidden");
    }

    #[test]
    fn odd_file_names_stay_one_argument() {
        let p = Path::new("C:\\my files\\a {dir} & b.txt");
        assert_eq!(
            arguments("-i {file} -o \"{dir}\\{stem} out.mp3\"", p, &[]),
            ["-i", "C:\\my files\\a {dir} & b.txt", "-o", "C:\\my files\\a {dir} & b out.mp3"]
        );
        assert_eq!(arguments("", p, &[]), ["C:\\my files\\a {dir} & b.txt"]);
        let all = [PathBuf::from("a"), PathBuf::from("b c")];
        assert_eq!(arguments("--add {files}", &all[0], &all), ["--add", "a", "b c"]);
    }
}
