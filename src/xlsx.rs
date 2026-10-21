//! .xlsx reading without Excel: cell values formatted the way Excel shows
//! them, rendered to an HTML table (for PDF) or to CSV.

use crate::convert::Result;
use crate::ooxml::{attr, elements, esc, first, local, num, open_zip, read_text, rels_of, xml, Zip};
use std::collections::HashMap;
use std::path::Path;

#[derive(Default, Clone)]
struct Xf {
    num_fmt: u32,
    bold: bool,
    italic: bool,
    color: Option<String>,
    fill: Option<String>,
    border: [bool; 4], // left, right, top, bottom
    align: Option<String>,
    wrap: bool,
}

struct Styles {
    codes: HashMap<u32, String>,
    xfs: Vec<Xf>,
}

#[derive(Default, Clone)]
struct Cell {
    /// What Excel would display.
    text: String,
    /// The underlying value, for CSV.
    raw: String,
    numeric: bool,
    xf: usize,
}

struct Sheet {
    name: String,
    cells: HashMap<(usize, usize), Cell>,
    rows: usize,
    cols: usize,
    merges: Vec<(usize, usize, usize, usize)>, // r0 c0 r1 c1 (inclusive)
    widths: HashMap<usize, f32>,
    hidden_rows: Vec<usize>,
    hidden_cols: Vec<usize>,
}

fn rgb(n: &roxmltree::Node) -> Option<String> {
    attr(n, "rgb").and_then(|v| {
        let h = v.trim_start_matches('#');
        let h = if h.len() == 8 { &h[2..] } else { h };
        (h.len() == 6).then(|| format!("#{h}"))
    })
}

fn load_styles(z: &mut Zip) -> Styles {
    let mut s = Styles { codes: HashMap::new(), xfs: Vec::new() };
    let Some(text) = read_text(z, "xl/styles.xml") else { return s };
    let Ok(doc) = xml(&text) else { return s };
    let root = doc.root_element();

    if let Some(nf) = first(&root, "numFmts") {
        for f in elements(&nf) {
            if let (Some(id), Some(code)) = (attr(&f, "numFmtId").and_then(|v| v.parse().ok()), attr(&f, "formatCode")) {
                s.codes.insert(id, code.to_string());
            }
        }
    }
    let fonts: Vec<(bool, bool, Option<String>)> = first(&root, "fonts")
        .map(|f| {
            elements(&f)
                .map(|font| {
                    let on = |n: &str| first(&font, n).map(|e| !matches!(attr(&e, "val"), Some("0") | Some("false"))).unwrap_or(false);
                    (on("b"), on("i"), first(&font, "color").and_then(|c| rgb(&c)))
                })
                .collect()
        })
        .unwrap_or_default();
    let fills: Vec<Option<String>> = first(&root, "fills")
        .map(|f| {
            elements(&f)
                .map(|fill| {
                    let p = first(&fill, "patternFill")?;
                    if attr(&p, "patternType") != Some("solid") {
                        return None;
                    }
                    first(&p, "fgColor").and_then(|c| rgb(&c))
                })
                .collect()
        })
        .unwrap_or_default();
    let borders: Vec<[bool; 4]> = first(&root, "borders")
        .map(|b| {
            elements(&b)
                .map(|border| {
                    let has = |n: &str| first(&border, n).map(|e| attr(&e, "style").is_some()).unwrap_or(false);
                    [has("left"), has("right"), has("top"), has("bottom")]
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(xfs) = first(&root, "cellXfs") {
        for xf in elements(&xfs) {
            let idx = |n: &str| attr(&xf, n).and_then(|v| v.parse::<usize>().ok());
            let (bold, italic, color) = idx("fontId").and_then(|i| fonts.get(i).cloned()).unwrap_or((false, false, None));
            s.xfs.push(Xf {
                num_fmt: idx("numFmtId").unwrap_or(0) as u32,
                bold,
                italic,
                color,
                fill: idx("fillId").and_then(|i| fills.get(i).cloned()).flatten(),
                border: idx("borderId").and_then(|i| borders.get(i).copied()).unwrap_or([false; 4]),
                align: first(&xf, "alignment").and_then(|a| attr(&a, "horizontal")).map(str::to_string),
                wrap: first(&xf, "alignment").is_some_and(|a| attr(&a, "wrapText") == Some("1")),
            });
        }
    }
    s
}

fn builtin_code(id: u32) -> Option<&'static str> {
    Some(match id {
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        14 => "yyyy-mm-dd",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "yyyy-mm-dd h:mm",
        37 | 38 => "#,##0",
        39 | 40 => "#,##0.00",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        _ => return None,
    })
}

/// Days since 1970-01-01 -> (year, month, day).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"];

fn is_date_code(code: &str) -> bool {
    let mut quoted = false;
    let mut bracket = false;
    for c in code.chars() {
        match c {
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' if !quoted && !bracket => return true,
            'm' | 'M' if !quoted && !bracket => return true,
            _ => {}
        }
    }
    false
}

fn format_date(serial: f64, code: &str) -> String {
    // Excel counts 1900 as a leap year; shift so serial 61 is 1900-03-01.
    let day = serial.floor();
    let days = if day >= 61.0 { day as i64 - 25569 } else { day as i64 - 25568 };
    let (y, mo, d) = civil(days);
    let secs = ((serial - day) * 86400.0).round() as i64;
    let (h, mi, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let twelve = code.to_uppercase().contains("AM/PM") || code.contains("A/P");

    let chars: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut last_unit = ' ';
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '[' => {
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
            }
            '\\' | '_' | '*' => i += 1,
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => {
                let lc = c.to_ascii_lowercase();
                let mut n = 0;
                while i + n < chars.len() && chars[i + n].to_ascii_lowercase() == lc {
                    n += 1;
                }
                // "m" after hours or before seconds means minutes.
                let next_s = chars[i + n..].iter().find(|c| !matches!(c, ':' | ' ' | '.')).map(|c| c.to_ascii_lowercase()) == Some('s');
                let minutes = lc == 'm' && (last_unit == 'h' || next_s);
                match (lc, n) {
                    ('y', 1 | 2) => out.push_str(&format!("{:02}", y.rem_euclid(100))),
                    ('y', _) => out.push_str(&y.to_string()),
                    ('m', _) if minutes => out.push_str(&if n == 1 { mi.to_string() } else { format!("{mi:02}") }),
                    ('m', 1) => out.push_str(&mo.to_string()),
                    ('m', 2) => out.push_str(&format!("{mo:02}")),
                    ('m', 3) => out.push_str(&MONTHS[mo as usize - 1][..3]),
                    ('m', _) => out.push_str(MONTHS[mo as usize - 1]),
                    ('d', 1) => out.push_str(&d.to_string()),
                    ('d', 2) => out.push_str(&format!("{d:02}")),
                    ('d', 3) => out.push_str(&WEEKDAYS[days.rem_euclid(7) as usize][..3]),
                    ('d', _) => out.push_str(WEEKDAYS[days.rem_euclid(7) as usize]),
                    ('h', _) => {
                        let hh = if twelve { if h % 12 == 0 { 12 } else { h % 12 } } else { h };
                        out.push_str(&if n == 1 { hh.to_string() } else { format!("{hh:02}") })
                    }
                    ('s', _) => out.push_str(&if n == 1 { s.to_string() } else { format!("{s:02}") }),
                    _ => {}
                }
                last_unit = if minutes { 'm' } else { lc };
                i += n;
                continue;
            }
            'A' | 'a' if code[i..].to_uppercase().starts_with("AM/PM") => {
                out.push_str(if h >= 12 { "PM" } else { "AM" });
                i += 5;
                continue;
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn group_thousands(int_part: &str) -> String {
    let mut out = String::new();
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_number(v: f64, code: &str) -> String {
    let section = code.split(';').next().unwrap_or("");
    let mut literal_prefix = String::new();
    for c in ['$', '\u{20ac}', '\u{a3}', '\u{a5}', '\u{20b9}'] {
        if section.contains(c) {
            literal_prefix.push(c);
            break;
        }
    }
    let percent = section.contains('%');
    let decimals = section
        .split_once('.')
        .map(|(_, rest)| rest.chars().take_while(|c| matches!(c, '0' | '#')).count())
        .unwrap_or(0);
    let v = if percent { v * 100.0 } else { v };
    let body = format!("{:.*}", decimals, v.abs());
    let (int_part, frac) = body.split_once('.').map(|(a, b)| (a.to_string(), format!(".{b}"))).unwrap_or((body.clone(), String::new()));
    let int_part = if section.contains(',') { group_thousands(&int_part) } else { int_part };
    let sign = if v < 0.0 && body.chars().any(|c| c.is_ascii_digit() && c != '0') { "-" } else { "" };
    format!("{sign}{literal_prefix}{int_part}{frac}{}", if percent { "%" } else { "" })
}

fn general(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.12}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn display(v: f64, xf: Option<&Xf>, styles: &Styles) -> (String, bool) {
    let id = xf.map(|x| x.num_fmt).unwrap_or(0);
    if id == 0 {
        return (general(v), false);
    }
    let code = styles.codes.get(&id).cloned().or_else(|| builtin_code(id).map(str::to_string));
    match code {
        Some(c) if c.eq_ignore_ascii_case("general") || c == "@" => (general(v), false),
        Some(c) if is_date_code(&c.split(';').next().unwrap_or("").replace("\\", "")) && !c.contains('#') && !c.contains('0') => {
            (format_date(v, c.split(';').next().unwrap_or("")), true)
        }
        Some(c) => (format_number(v, &c), false),
        None => (general(v), false),
    }
}

fn col_row(r: &str) -> Option<(usize, usize)> {
    let letters: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let row: usize = r[letters.len()..].parse().ok()?;
    let col = letters.to_ascii_uppercase().bytes().fold(0usize, |a, b| a * 26 + (b - b'A') as usize + 1);
    (col > 0 && row > 0).then_some((col - 1, row - 1))
}

fn read_sheets(path: &Path) -> Result<(Vec<Sheet>, Styles)> {
    let mut z = open_zip(path)?;
    let wb_text = read_text(&mut z, "xl/workbook.xml").ok_or("Not an Excel workbook (no xl/workbook.xml)")?;
    let wb = xml(&wb_text)?;
    let rels = rels_of(&mut z, "xl/workbook.xml");
    let styles = load_styles(&mut z);

    let shared: Vec<String> = read_text(&mut z, "xl/sharedStrings.xml")
        .map(|t| {
            xml(&t)
                .map(|d| {
                    elements(&d.root_element())
                        .filter(|si| local(si) == "si")
                        .map(|si| si.descendants().filter(|n| n.is_element() && local(n) == "t").filter_map(|t| t.text()).collect::<String>())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let mut sheets = Vec::new();
    for s in wb.descendants().filter(|n| n.is_element() && local(n) == "sheet") {
        if matches!(attr(&s, "state"), Some("hidden") | Some("veryHidden")) {
            continue;
        }
        let name = attr(&s, "name").unwrap_or("Sheet").to_string();
        let Some(part) = attr(&s, "id").and_then(|id| rels.get(id)).cloned() else { continue };
        let Some(text) = read_text(&mut z, &part) else { continue };
        let doc = xml(&text)?;
        let root = doc.root_element();
        let mut sheet = Sheet {
            name,
            cells: HashMap::new(),
            rows: 0,
            cols: 0,
            merges: Vec::new(),
            widths: HashMap::new(),
            hidden_rows: Vec::new(),
            hidden_cols: Vec::new(),
        };
        if let Some(cols) = first(&root, "cols") {
            for c in elements(&cols) {
                let (lo, hi) = (num(attr(&c, "min")).unwrap_or(1.0) as usize, num(attr(&c, "max")).unwrap_or(1.0) as usize);
                for col in lo..=hi.min(lo + 200) {
                    if let Some(w) = num(attr(&c, "width")) {
                        sheet.widths.insert(col - 1, w);
                    }
                    if attr(&c, "hidden") == Some("1") {
                        sheet.hidden_cols.push(col - 1);
                    }
                }
            }
        }
        if let Some(data) = first(&root, "sheetData") {
            for (ri, row) in elements(&data).filter(|r| local(r) == "row").enumerate() {
                let row_idx = attr(&row, "r").and_then(|v| v.parse::<usize>().ok()).map(|r| r - 1).unwrap_or(ri);
                if attr(&row, "hidden") == Some("1") {
                    sheet.hidden_rows.push(row_idx);
                }
                for (ci, c) in elements(&row).filter(|c| local(c) == "c").enumerate() {
                    let (col, r) = attr(&c, "r").and_then(col_row).unwrap_or((ci, row_idx));
                    let xf_idx = attr(&c, "s").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                    let xf = styles.xfs.get(xf_idx);
                    let v = first(&c, "v").and_then(|v| v.text()).unwrap_or("");
                    let (text, raw, numeric) = match attr(&c, "t").unwrap_or("n") {
                        "s" => {
                            let t = v.parse::<usize>().ok().and_then(|i| shared.get(i)).cloned().unwrap_or_default();
                            (t.clone(), t, false)
                        }
                        "str" => (v.to_string(), v.to_string(), false),
                        "inlineStr" => {
                            let t: String = c.descendants().filter(|n| n.is_element() && local(n) == "t").filter_map(|t| t.text()).collect();
                            (t.clone(), t, false)
                        }
                        "b" => {
                            let t = if v == "1" { "TRUE" } else { "FALSE" };
                            (t.into(), t.into(), false)
                        }
                        "e" => (v.to_string(), v.to_string(), false),
                        _ => match v.parse::<f64>() {
                            Ok(n) => {
                                let (t, is_date) = display(n, xf, &styles);
                                (t.clone(), if is_date { t } else { general(n) }, !is_date)
                            }
                            Err(_) => (v.to_string(), v.to_string(), false),
                        },
                    };
                    if text.is_empty() && !xf.is_some_and(|x| x.fill.is_some() || x.border.iter().any(|b| *b)) {
                        continue;
                    }
                    sheet.rows = sheet.rows.max(r + 1);
                    sheet.cols = sheet.cols.max(col + 1);
                    sheet.cells.insert((r, col), Cell { text, raw, numeric, xf: xf_idx });
                }
            }
        }
        if let Some(m) = first(&root, "mergeCells") {
            for mc in elements(&m) {
                let Some((a, b)) = attr(&mc, "ref").and_then(|r| r.split_once(':')) else { continue };
                if let (Some((c0, r0)), Some((c1, r1))) = (col_row(a), col_row(b)) {
                    sheet.merges.push((r0, c0, r1, c1));
                }
            }
        }
        sheets.push(sheet);
    }
    if sheets.is_empty() {
        return Err("The workbook has no visible sheets".into());
    }
    Ok((sheets, styles))
}

const MAX_ROWS: usize = 5000;

pub fn xlsx_to_html(path: &Path) -> Result<String> {
    let (sheets, styles) = read_sheets(path)?;
    let mut body = String::new();
    for (si, sheet) in sheets.iter().enumerate() {
        let wide = sheet.cols > 8;
        let mut table = format!(
            "<div style=\"{}page:{}\"><table>",
            if si > 0 { "break-before:page;" } else { "" },
            if wide { "wide" } else { "tall" }
        );
        table.push_str("<colgroup>");
        for c in 0..sheet.cols {
            if sheet.hidden_cols.contains(&c) {
                continue;
            }
            let w = sheet.widths.get(&c).copied().unwrap_or(8.43) * 5.25 + 4.0;
            table.push_str(&format!("<col style=\"width:{w}pt\">"));
        }
        table.push_str("</colgroup>");

        let mut covered: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        let mut spans: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
        for &(r0, c0, r1, c1) in &sheet.merges {
            spans.insert((r0, c0), (r1 - r0 + 1, c1 - c0 + 1));
            for r in r0..=r1 {
                for c in c0..=c1 {
                    if (r, c) != (r0, c0) {
                        covered.insert((r, c));
                    }
                }
            }
        }
        let shown_rows = sheet.rows.min(MAX_ROWS);
        for r in 0..shown_rows {
            if sheet.hidden_rows.contains(&r) {
                continue;
            }
            table.push_str("<tr>");
            for c in 0..sheet.cols {
                if sheet.hidden_cols.contains(&c) || covered.contains(&(r, c)) {
                    continue;
                }
                let cell = sheet.cells.get(&(r, c));
                let xf = cell.and_then(|k| styles.xfs.get(k.xf));
                let mut css = String::new();
                if let Some(x) = xf {
                    css.push_str(if x.wrap { "white-space:pre-wrap;" } else { "white-space:nowrap;" });
                    if x.bold {
                        css.push_str("font-weight:bold;");
                    }
                    if x.italic {
                        css.push_str("font-style:italic;");
                    }
                    if let Some(col) = &x.color {
                        css.push_str(&format!("color:{col};"));
                    }
                    if let Some(f) = &x.fill {
                        css.push_str(&format!("background:{f};"));
                    }
                    for (side, on) in ["left", "right", "top", "bottom"].iter().zip(x.border) {
                        if on {
                            css.push_str(&format!("border-{side}:0.75pt solid #000;"));
                        }
                    }
                }
                let align = xf.and_then(|x| x.align.clone()).or_else(|| cell.filter(|k| k.numeric).map(|_| "right".to_string()));
                if let Some(a) = align {
                    css.push_str(&format!("text-align:{};", if a == "general" { "left".into() } else { a }));
                }
                let (rs, cs) = spans.get(&(r, c)).copied().unwrap_or((1, 1));
                let text = cell.map(|k| esc(&k.text)).unwrap_or_default();
                table.push_str(&format!("<td rowspan=\"{rs}\" colspan=\"{cs}\" style=\"{css}\">{text}</td>"));
            }
            table.push_str("</tr>");
        }
        table.push_str("</table>");
        if sheet.rows > MAX_ROWS {
            table.push_str(&format!("<p class=\"note\">Showing the first {MAX_ROWS} of {} rows.</p>", sheet.rows));
        }
        table.push_str("</div>");
        if sheets.len() > 1 {
            table = table.replacen("<table>", &format!("<div class=\"title\">{}</div><table>", esc(&sheet.name)), 1);
        }
        body.push_str(&table);
    }
    Ok(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
         @page tall{{size:A4 portrait;margin:15mm}}@page wide{{size:A4 landscape;margin:12mm}}\
         body{{font:11pt Calibri,'Segoe UI',sans-serif;margin:0}}\
         table{{border-collapse:collapse;table-layout:fixed}}td{{padding:1pt 3pt;overflow:hidden;white-space:nowrap;vertical-align:bottom;height:15pt}}\
         .title{{font:bold 12pt Calibri,sans-serif;margin:0 0 6pt}}.note{{color:#666;font-size:9pt}}</style></head><body>{body}</body></html>"
    ))
}

pub fn xlsx_to_csv(path: &Path) -> Result<String> {
    let (sheets, _) = read_sheets(path)?;
    let sheet = &sheets[0];
    let esc_csv = |s: &str| {
        if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
    };
    let mut out = String::new();
    for r in 0..sheet.rows {
        let line: Vec<String> = (0..sheet.cols).map(|c| sheet.cells.get(&(r, c)).map(|k| esc_csv(&k.raw)).unwrap_or_default()).collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    Ok(out)
}
