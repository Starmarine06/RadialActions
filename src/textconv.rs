//! Text and data conversions. JSON is the pivot for YAML, TOML and CSV.

use crate::convert::{unique_sibling, Result};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

fn read_text(src: &Path) -> Result<String> {
    let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
    Ok(decode(&bytes))
}

/// UTF-8 (with or without BOM), UTF-16 with BOM, else Windows-1252.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    for (bom, big_endian) in [([0xFF, 0xFE], false), ([0xFE, 0xFF], true)] {
        if let Some(rest) = bytes.strip_prefix(&bom) {
            let units = rest.chunks_exact(2).map(|c| {
                if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }
            });
            return char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect();
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
        '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}',
        '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
        '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}', '\u{178}',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// Saves `text` as a new file beside `src` and returns its path.
pub fn save_sibling(src: &Path, suffix: &str, ext: &str, text: &str) -> Result<PathBuf> {
    write_text(src, suffix, ext, text)
}

fn write_text(src: &Path, suffix: &str, ext: &str, text: &str) -> Result<PathBuf> {
    let dst = unique_sibling(src, suffix, ext);
    std::fs::write(&dst, text).map_err(|e| e.to_string())?;
    Ok(dst)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    Json,
    Yaml,
    Toml,
    Csv,
}

impl Data {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Csv => "csv",
        }
    }

    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "json" => Some(Self::Json),
            "yaml" | "yml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            "csv" => Some(Self::Csv),
            _ => None,
        }
    }
}

fn parse(src: &Path, from: Data) -> Result<Value> {
    let text = read_text(src)?;
    match from {
        Data::Json => serde_json::from_str(&text).map_err(|e| format!("Invalid JSON: {e}")),
        Data::Yaml => serde_norway::from_str(&text).map_err(|e| format!("Invalid YAML: {e}")),
        Data::Toml => toml::from_str(&text).map_err(|e| format!("Invalid TOML: {e}")),
        Data::Csv => Ok(csv_to_value(&text)),
    }
}

fn render(value: &Value, to: Data) -> Result<String> {
    match to {
        Data::Json => serde_json::to_string_pretty(value).map(|s| s + "\n").map_err(|e| e.to_string()),
        Data::Yaml => serde_norway::to_string(value).map_err(|e| e.to_string()),
        Data::Toml => {
            let v = toml::Value::try_from(value)
                .map_err(|_| "TOML can't hold this data (needs an object, no nulls)".to_string())?;
            toml::to_string_pretty(&v).map_err(|e| e.to_string())
        }
        Data::Csv => value_to_csv(value),
    }
}

pub fn convert_data(src: &Path, from: Data, to: Data) -> Result<PathBuf> {
    let text = render(&parse(src, from)?, to)?;
    write_text(src, "", to.ext(), &text)
}

pub fn json_pretty(src: &Path) -> Result<PathBuf> {
    let v = parse(src, Data::Json)?;
    write_text(src, "_pretty", "json", &render(&v, Data::Json)?)
}

pub fn json_minify(src: &Path) -> Result<PathBuf> {
    let v = parse(src, Data::Json)?;
    write_text(src, "_min", "json", &serde_json::to_string(&v).map_err(|e| e.to_string())?)
}

// ---- CSV ----------------------------------------------------------------

/// RFC 4180 records: quoted fields, doubled quotes, embedded newlines.
fn csv_records(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let (mut row, mut field) = (Vec::new(), String::new());
    let (mut quoted, mut chars) = (false, text.chars().peekable());
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if field.is_empty() => quoted = true,
            (',', false) => row.push(std::mem::take(&mut field)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    rows
}

fn infer(s: &str) -> Value {
    let plain_int = s == "0" || (!s.starts_with('0') && !s.starts_with("-0") && !s.is_empty());
    if plain_int {
        if let Ok(i) = s.parse::<i64>() {
            return Value::from(i);
        }
    }
    if s.contains('.') && !s.starts_with('0') || s.starts_with("0.") {
        if let Some(n) = s.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return Value::Number(n);
        }
    }
    Value::String(s.to_string())
}

fn csv_to_value(text: &str) -> Value {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = csv_records(text).into_iter();
    let Some(header) = rows.next() else { return Value::Array(Vec::new()) };
    let items = rows
        .map(|r| {
            let obj: Map<String, Value> = header
                .iter()
                .enumerate()
                .map(|(i, h)| (h.clone(), r.get(i).map_or(Value::Null, |c| infer(c))))
                .collect();
            Value::Object(obj)
        })
        .collect();
    Value::Array(items)
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn value_to_csv(value: &Value) -> Result<String> {
    let Value::Array(items) = value else {
        return Err("CSV needs a JSON array of objects".into());
    };
    let mut columns: Vec<String> = Vec::new();
    for item in items {
        let Value::Object(o) = item else {
            return Err("CSV needs a JSON array of objects".into());
        };
        for k in o.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    let mut out = columns.iter().map(|c| csv_escape(c)).collect::<Vec<_>>().join(",") + "\r\n";
    for item in items {
        let line: Vec<String> = columns
            .iter()
            .map(|c| csv_escape(&item.get(c).map(cell).unwrap_or_default()))
            .collect();
        out += &(line.join(",") + "\r\n");
    }
    Ok(out)
}

// ---- Markdown, line endings, encoding ------------------------------------

pub fn markdown_to_html(src: &Path) -> Result<PathBuf> {
    let md = read_text(src)?;
    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, pulldown_cmark::Parser::new_ext(&md, pulldown_cmark::Options::all()));
    let title = src.file_stem().and_then(|s| s.to_str()).unwrap_or("Document");
    let page = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>{}</title>\n<style>\
         body{{font:16px/1.6 system-ui,sans-serif;max-width:46rem;margin:2rem auto;padding:0 1rem}}\
         pre,code{{background:#f4f4f5;border-radius:4px}}pre{{padding:.8rem;overflow:auto}}code{{padding:.1rem .3rem}}\
         pre code{{padding:0}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:.3rem .6rem}}\
         blockquote{{border-left:3px solid #ccc;margin-left:0;padding-left:1rem;color:#555}}</style></head>\n<body>\n{body}</body></html>\n",
        title.replace('&', "&amp;").replace('<', "&lt;")
    );
    write_text(src, "", "html", &page)
}

pub fn line_endings(src: &Path, crlf: bool) -> Result<PathBuf> {
    let text = read_text(src)?.replace("\r\n", "\n");
    let text = if crlf { text.replace('\n', "\r\n") } else { text };
    write_text(src, if crlf { "_crlf" } else { "_lf" }, &ext_of(src), &text)
}

/// Rewrites a text file as plain UTF-8 in place (used on Office text exports).
pub fn normalize_to_utf8(path: &Path) -> Result<()> {
    let text = read_text(path)?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

pub fn to_utf8(src: &Path) -> Result<PathBuf> {
    let text = read_text(src)?;
    write_text(src, "_utf8", &ext_of(src), &text)
}

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("txt").to_string()
}

// ---- Text to PDF ----------------------------------------------------------

/// Monospaced PDF: Courier 10 pt on A4, wrapped at 82 columns, 60 lines a page.
pub fn text_to_pdf(src: &Path) -> Result<PathBuf> {
    const COLS: usize = 82;
    const LINES: usize = 60;
    let text = read_text(src)?;

    let mut lines: Vec<String> = Vec::new();
    for raw in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let raw = raw.replace('\t', "    ");
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
        }
        for chunk in chars.chunks(COLS) {
            lines.push(chunk.iter().collect());
        }
    }
    let pages: Vec<&[String]> = lines.chunks(LINES).collect();
    let pages = if pages.is_empty() { vec![&[][..]] } else { pages };

    // Objects: 1 catalog, 2 pages, 3 font, then a (page, content) pair per page.
    let mut buf: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let mut add = |buf: &mut Vec<u8>, body: &[u8]| {
        offsets.push(buf.len());
        buf.extend_from_slice(format!("{} 0 obj\n", offsets.len()).as_bytes());
        buf.extend_from_slice(body);
        buf.extend_from_slice(b"\nendobj\n");
    };
    let kids: String = (0..pages.len()).map(|i| format!("{} 0 R ", 4 + i * 2)).collect();
    add(&mut buf, b"<< /Type /Catalog /Pages 2 0 R >>");
    add(&mut buf, format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).as_bytes());
    add(&mut buf, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>");
    for (i, page) in pages.iter().enumerate() {
        let content_id = 5 + i * 2;
        add(
            &mut buf,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>"
            )
            .as_bytes(),
        );
        let mut stream: Vec<u8> = b"BT /F1 10 Tf 12.5 TL 50 792 Td\n".to_vec();
        for line in page.iter() {
            stream.push(b'(');
            for ch in line.chars() {
                match ch {
                    '(' | ')' | '\\' => stream.extend_from_slice(&[b'\\', ch as u8]),
                    c if (c as u32) < 0x20 => stream.push(b' '),
                    c if (c as u32) < 0x100 => stream.push(c as u32 as u8),
                    _ => stream.push(b'?'),
                }
            }
            stream.extend_from_slice(b") Tj T*\n");
        }
        stream.extend_from_slice(b"ET");
        let mut body = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
        body.extend_from_slice(&stream);
        body.extend_from_slice(b"\nendstream");
        add(&mut buf, &body);
    }

    let xref = buf.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
    for o in &offsets {
        table.push_str(&format!("{o:010} 00000 n \n"));
    }
    table.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", offsets.len() + 1));
    buf.extend_from_slice(table.as_bytes());

    let dst = unique_sibling(src, "", "pdf");
    std::fs::write(&dst, buf).map_err(|e| e.to_string())?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_txt_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(d: &Path, name: &str, body: &[u8]) -> PathBuf {
        let p = d.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn json_yaml_toml_round_trip() {
        let d = dir("rt");
        let j = file(&d, "a.json", br#"{"name":"x","n":3,"list":[1,2],"nested":{"ok":true}}"#);
        let y = convert_data(&j, Data::Json, Data::Yaml).unwrap();
        let t = convert_data(&y, Data::Yaml, Data::Toml).unwrap();
        let back = convert_data(&t, Data::Toml, Data::Json).unwrap();
        let orig: Value = serde_json::from_str(&read(&j)).unwrap();
        let round: Value = serde_json::from_str(&read(&back)).unwrap();
        assert_eq!(orig, round);
    }

    #[test]
    fn toml_rejects_null_with_clear_error() {
        let d = dir("null");
        let j = file(&d, "a.json", br#"{"a":null}"#);
        let err = convert_data(&j, Data::Json, Data::Toml).unwrap_err();
        assert!(err.contains("TOML"));
    }

    #[test]
    fn bad_json_reports_error_and_writes_nothing() {
        let d = dir("bad");
        let j = file(&d, "a.json", b"{oops");
        assert!(convert_data(&j, Data::Json, Data::Yaml).unwrap_err().starts_with("Invalid JSON"));
        assert!(!d.join("a.yaml").exists());
    }

    #[test]
    fn csv_handles_quotes_newlines_and_types() {
        let d = dir("csv");
        let c = file(&d, "a.csv", "name,note,qty,zip\r\n\"Smith, J\",\"say \"\"hi\"\"\nline2\",3,007\r\nAnn,,4.5,10\r\n".as_bytes());
        let v: Value = serde_json::from_str(&read(&convert_data(&c, Data::Csv, Data::Json).unwrap())).unwrap();
        assert_eq!(v[0]["name"], "Smith, J");
        assert_eq!(v[0]["note"], "say \"hi\"\nline2");
        assert_eq!(v[0]["qty"], 3);
        assert_eq!(v[0]["zip"], "007"); // leading zeros stay text
        assert_eq!(v[1]["qty"], 4.5);
        // and back
        let j = convert_data(&c, Data::Csv, Data::Json).unwrap();
        let c2 = convert_data(&j, Data::Json, Data::Csv).unwrap();
        let v2: Value = serde_json::from_str(&read(&convert_data(&c2, Data::Csv, Data::Json).unwrap())).unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn json_to_csv_needs_array_of_objects() {
        let d = dir("csverr");
        let j = file(&d, "a.json", br#"{"a":1}"#);
        assert!(convert_data(&j, Data::Json, Data::Csv).is_err());
    }

    #[test]
    fn pretty_and_minify() {
        let d = dir("pm");
        let j = file(&d, "a.json", br#"{ "a" : [1, 2] }"#);
        assert!(read(&json_pretty(&j).unwrap()).contains("\n  \"a\""));
        assert_eq!(read(&json_minify(&j).unwrap()), r#"{"a":[1,2]}"#);
    }

    #[test]
    fn markdown_becomes_html() {
        let d = dir("md");
        let m = file(&d, "a.md", b"# Title\n\n- one\n- two\n");
        let h = read(&markdown_to_html(&m).unwrap());
        assert!(h.contains("<h1>Title</h1>") && h.contains("<li>one</li>"));
    }

    #[test]
    fn line_endings_and_encoding() {
        let d = dir("le");
        let t = file(&d, "a.txt", b"a\r\nb\nc");
        assert_eq!(read(&line_endings(&t, false).unwrap()), "a\nb\nc");
        assert_eq!(read(&line_endings(&t, true).unwrap()), "a\r\nb\r\nc");
        // Windows-1252 "caf\xe9" and UTF-16LE with BOM both become UTF-8.
        let latin = file(&d, "l.txt", b"caf\xe9 \x93q\x94");
        assert_eq!(read(&to_utf8(&latin).unwrap()), "caf\u{e9} \u{201c}q\u{201d}");
        let mut u16b = vec![0xFF, 0xFE];
        for u in "h\u{e9}llo".encode_utf16() {
            u16b.extend_from_slice(&u.to_le_bytes());
        }
        let u = file(&d, "u.txt", &u16b);
        assert_eq!(read(&to_utf8(&u).unwrap()), "h\u{e9}llo");
    }

    #[test]
    fn text_pdf_paginates_and_is_valid() {
        let d = dir("pdf");
        let long: String = (0..130).map(|i| format!("line {i} (with parens) \\ and a tab\there\n")).collect();
        let t = file(&d, "a.txt", long.as_bytes());
        let pdf = std::fs::read(text_to_pdf(&t).unwrap()).unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.starts_with("%PDF-1.4") && s.ends_with("%%EOF\n"));
        assert!(s.contains("/Count 3")); // 130 lines at 60 per page
        let off: usize = s.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert_eq!(&pdf[off..off + 4], b"xref");
        assert!(s.contains("\\(with parens\\) \\\\"));
    }

    #[test]
    fn empty_text_still_makes_a_page() {
        let d = dir("empty");
        let t = file(&d, "a.txt", b"");
        let s = String::from_utf8_lossy(&std::fs::read(text_to_pdf(&t).unwrap()).unwrap()).into_owned();
        assert!(s.contains("/Count 1"));
    }
}
