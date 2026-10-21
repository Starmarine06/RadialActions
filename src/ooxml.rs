//! Reads .docx, .xlsx and .pptx directly, with no Office installed or opened,
//! and renders them as HTML that Edge's headless printer turns into a PDF.
//! Also writes a plain .docx.
//!
//! This is a faithful-enough renderer, not a clone of Office: text, styles,
//! lists, tables, images and page setup carry over; exotic layout does not.

use crate::convert::Result;
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

pub type Zip = ZipArchive<File>;

// ---- shared helpers ---------------------------------------------------------------

pub fn open_zip(path: &Path) -> Result<Zip> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let file = File::open(path).map_err(|e| format!("{name}: {e}"))?;
    ZipArchive::new(file).map_err(|_| format!("{name} isn't a valid Office file (old format or damaged)"))
}

pub(crate) fn read_text(z: &mut Zip, name: &str) -> Option<String> {
    let mut f = z.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn read_bytes(z: &mut Zip, name: &str) -> Option<Vec<u8>> {
    let mut f = z.by_name(name).ok()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

pub(crate) fn xml(text: &str) -> Result<Document<'_>> {
    Document::parse_with_options(text, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() })
        .map_err(|e| format!("Damaged document: {e}"))
}

pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

pub(crate) fn local<'a>(n: &Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

/// Attribute by local name, ignoring its namespace prefix.
pub(crate) fn attr<'a>(n: &Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// The relationship id (`r:id`), which may sit beside a plain `id` attribute.
pub(crate) fn rid<'a>(n: &Node<'a, '_>) -> Option<&'a str> {
    n.attributes()
        .find(|a| a.name() == "id" && a.namespace().is_some_and(|ns| ns.contains("relationships")))
        .map(|a| a.value())
}

pub(crate) fn first<'a, 'i>(n: &Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && local(c) == name)
}

pub(crate) fn elements<'a, 'i>(n: &Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}

pub(crate) fn descendant<'a, 'i>(n: &Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.descendants().find(|c| c.is_element() && local(c) == name)
}

/// A boolean property element: present means on unless it says val=0/false.
fn toggle(n: &Node) -> bool {
    !matches!(attr(n, "val"), Some("0") | Some("false") | Some("off"))
}

pub(crate) fn num(s: Option<&str>) -> Option<f32> {
    s.and_then(|v| v.parse::<f32>().ok())
}

/// Resolves a relationship target against the folder of the part that owns it.
pub(crate) fn resolve(dir: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Relationship id -> resolved part path, or "ext:<url>" for external links.
pub(crate) fn rels_of(z: &mut Zip, part: &str) -> HashMap<String, String> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let path = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
    let mut map = HashMap::new();
    let Some(text) = read_text(z, &path) else { return map };
    let Ok(doc) = xml(&text) else { return map };
    for r in doc.descendants().filter(|n| n.is_element() && local(n) == "Relationship") {
        let (Some(id), Some(target)) = (attr(&r, "Id"), attr(&r, "Target")) else { continue };
        let value = if attr(&r, "TargetMode") == Some("External") { format!("ext:{target}") } else { resolve(dir, target) };
        map.insert(id.to_string(), value);
    }
    map
}

pub(crate) fn image_uri(z: &mut Zip, path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => return None, // EMF/WMF and friends can't be shown
    };
    let bytes = read_bytes(z, path)?;
    Some(format!("data:{mime};base64,{}", b64(&bytes)))
}

fn color_css(hex: &str) -> Option<String> {
    let h = hex.trim_start_matches('#');
    let h = if h.len() == 8 { &h[2..] } else { h }; // ARGB -> RGB
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{h}"))
}

// ---- DOCX ----------------------------------------------------------------------------

#[derive(Default, Clone)]
struct RunProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    caps: Option<bool>,
    size: Option<f32>,
    color: Option<String>,
    font: Option<String>,
    highlight: Option<String>,
    vert: Option<String>,
}

impl RunProps {
    fn over(&mut self, o: &RunProps) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f.clone(); } )* };
        }
        take!(bold, italic, underline, strike, caps, size, color, font, highlight, vert);
    }

    fn css(&self) -> String {
        let mut c = String::new();
        if self.bold == Some(true) {
            c.push_str("font-weight:bold;");
        }
        if self.italic == Some(true) {
            c.push_str("font-style:italic;");
        }
        let mut deco = Vec::new();
        if self.underline == Some(true) {
            deco.push("underline");
        }
        if self.strike == Some(true) {
            deco.push("line-through");
        }
        if !deco.is_empty() {
            c.push_str(&format!("text-decoration:{};", deco.join(" ")));
        }
        if self.caps == Some(true) {
            c.push_str("text-transform:uppercase;");
        }
        if let Some(s) = self.size {
            let s = if self.vert.is_some() { s * 0.7 } else { s };
            c.push_str(&format!("font-size:{s}pt;"));
        }
        if let Some(col) = &self.color {
            c.push_str(&format!("color:{col};"));
        }
        if let Some(f) = &self.font {
            c.push_str(&format!("font-family:'{}',sans-serif;", f.replace('\'', "")));
        }
        if let Some(h) = &self.highlight {
            c.push_str(&format!("background:{h};"));
        }
        match self.vert.as_deref() {
            Some("superscript") => c.push_str("vertical-align:super;"),
            Some("subscript") => c.push_str("vertical-align:sub;"),
            _ => {}
        }
        c
    }
}

fn highlight_css(name: &str) -> Option<String> {
    Some(
        match name {
            "yellow" => "#ffff00",
            "green" => "#00ff00",
            "cyan" => "#00ffff",
            "magenta" => "#ff00ff",
            "blue" => "#0000ff",
            "red" => "#ff0000",
            "darkBlue" => "#000080",
            "darkCyan" => "#008080",
            "darkGreen" => "#008000",
            "darkMagenta" => "#800080",
            "darkRed" => "#800000",
            "darkYellow" => "#808000",
            "darkGray" => "#808080",
            "lightGray" => "#c0c0c0",
            "black" => "#000000",
            _ => return None,
        }
        .to_string(),
    )
}

fn parse_run_props(rpr: &Node, theme: &Theme) -> RunProps {
    let mut p = RunProps::default();
    for c in elements(rpr) {
        match local(&c) {
            "b" => p.bold = Some(toggle(&c)),
            "i" => p.italic = Some(toggle(&c)),
            "strike" | "dstrike" => p.strike = Some(toggle(&c)),
            "caps" => p.caps = Some(toggle(&c)),
            "u" => p.underline = Some(!matches!(attr(&c, "val"), Some("none"))),
            "sz" => p.size = num(attr(&c, "val")).map(|h| h / 2.0),
            "color" => {
                p.color = attr(&c, "val").filter(|v| *v != "auto").and_then(color_css);
            }
            "highlight" => p.highlight = attr(&c, "val").and_then(highlight_css),
            "vertAlign" => p.vert = attr(&c, "val").filter(|v| *v != "baseline").map(str::to_string),
            "rFonts" => {
                p.font = attr(&c, "ascii")
                    .map(str::to_string)
                    .or_else(|| match attr(&c, "asciiTheme") {
                        Some(t) if t.starts_with("major") => theme.major.clone(),
                        Some(_) => theme.minor.clone(),
                        None => None,
                    })
            }
            _ => {}
        }
    }
    p
}

#[derive(Default, Clone)]
struct ParaProps {
    jc: Option<String>,
    before: Option<f32>,
    after: Option<f32>,
    line: Option<String>,
    left: Option<f32>,
    hanging: Option<f32>,
    first: Option<f32>,
    keep_break: bool,
}

impl ParaProps {
    fn over(&mut self, o: &ParaProps) {
        macro_rules! take {
            ($($f:ident),*) => { $( if o.$f.is_some() { self.$f = o.$f.clone(); } )* };
        }
        take!(jc, before, after, line, left, hanging, first);
        self.keep_break |= o.keep_break;
    }
}

fn parse_para_props(ppr: &Node) -> ParaProps {
    let mut p = ParaProps::default();
    for c in elements(ppr) {
        match local(&c) {
            "jc" => {
                p.jc = Some(
                    match attr(&c, "val").unwrap_or("left") {
                        "center" => "center",
                        "right" | "end" => "right",
                        "both" | "distribute" => "justify",
                        _ => "left",
                    }
                    .to_string(),
                )
            }
            "spacing" => {
                p.before = num(attr(&c, "before")).map(|t| t / 20.0);
                p.after = num(attr(&c, "after")).map(|t| t / 20.0);
                if let Some(l) = num(attr(&c, "line")) {
                    p.line = Some(match attr(&c, "lineRule") {
                        Some("exact") | Some("atLeast") => format!("{}pt", l / 20.0),
                        _ => format!("{:.3}", l / 240.0),
                    });
                }
            }
            "ind" => {
                p.left = num(attr(&c, "left").or(attr(&c, "start"))).map(|t| t / 20.0);
                p.hanging = num(attr(&c, "hanging")).map(|t| t / 20.0);
                p.first = num(attr(&c, "firstLine")).map(|t| t / 20.0);
            }
            "pageBreakBefore" => p.keep_break = toggle(&c),
            _ => {}
        }
    }
    p
}

#[derive(Default)]
pub struct Theme {
    pub major: Option<String>,
    pub minor: Option<String>,
    pub colors: HashMap<String, String>,
}

pub(crate) fn parse_theme(z: &mut Zip, path: &str) -> Theme {
    let mut t = Theme::default();
    let Some(text) = read_text(z, path) else { return t };
    let Ok(doc) = xml(&text) else { return t };
    for n in doc.descendants().filter(|n| n.is_element()) {
        match local(&n) {
            "majorFont" => t.major = first(&n, "latin").and_then(|l| attr(&l, "typeface")).map(str::to_string),
            "minorFont" => t.minor = first(&n, "latin").and_then(|l| attr(&l, "typeface")).map(str::to_string),
            "clrScheme" => {
                for c in elements(&n) {
                    let val = elements(&c).next().and_then(|v| attr(&v, "val").or(attr(&v, "lastClr"))).and_then(color_css);
                    if let Some(v) = val {
                        t.colors.insert(local(&c).to_string(), v);
                    }
                }
            }
            _ => {}
        }
    }
    t
}

#[derive(Default, Clone)]
struct Style {
    name: String,
    based: Option<String>,
    run: RunProps,
    para: ParaProps,
}

#[derive(Clone)]
struct LevelDef {
    fmt: String,
    text: String,
    start: u32,
    left: Option<f32>,
    hanging: Option<f32>,
}

struct Docx {
    z: Zip,
    rels: HashMap<String, String>,
    theme: Theme,
    styles: HashMap<String, Style>,
    default_style: String,
    def_run: RunProps,
    def_para: ParaProps,
    num_to_abs: HashMap<String, String>,
    levels: HashMap<(String, u32), LevelDef>,
    counters: HashMap<String, [u32; 9]>,
    /// Margins of the section being rendered, in points.
    margin_left: f32,
    margin_top: f32,
}

fn alpha(mut n: u32, upper: bool) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    let s: String = s.iter().rev().collect();
    if upper { s.to_uppercase() } else { s }
}

fn roman(mut n: u32, upper: bool) -> String {
    let table = [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper { s.to_uppercase() } else { s }
}

fn format_counter(fmt: &str, n: u32) -> String {
    match fmt {
        "lowerLetter" => alpha(n, false),
        "upperLetter" => alpha(n, true),
        "lowerRoman" => roman(n, false),
        "upperRoman" => roman(n, true),
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

impl Docx {
    fn open(path: &Path) -> Result<(Docx, String)> {
        let mut z = open_zip(path)?;
        let body = read_text(&mut z, "word/document.xml").ok_or("Not a Word document (no word/document.xml)")?;
        let theme = parse_theme(&mut z, "word/theme/theme1.xml");
        let rels = rels_of(&mut z, "word/document.xml");
        let mut d = Docx {
            z,
            rels,
            theme,
            styles: HashMap::new(),
            default_style: "Normal".into(),
            def_run: RunProps::default(),
            def_para: ParaProps::default(),
            num_to_abs: HashMap::new(),
            levels: HashMap::new(),
            counters: HashMap::new(),
            margin_left: 0.0,
            margin_top: 0.0,
        };
        d.load_styles();
        d.load_numbering();
        Ok((d, body))
    }

    fn load_styles(&mut self) {
        let Some(text) = read_text(&mut self.z, "word/styles.xml") else { return };
        let Ok(doc) = xml(&text) else { return };
        let Some(root) = doc.descendants().find(|n| n.is_element() && local(n) == "styles") else { return };
        if let Some(dd) = first(&root, "docDefaults") {
            if let Some(r) = first(&dd, "rPrDefault").and_then(|n| first(&n, "rPr")) {
                self.def_run = parse_run_props(&r, &self.theme);
            }
            if let Some(p) = first(&dd, "pPrDefault").and_then(|n| first(&n, "pPr")) {
                self.def_para = parse_para_props(&p);
            }
        }
        for s in elements(&root).filter(|n| local(n) == "style") {
            let Some(id) = attr(&s, "styleId") else { continue };
            if attr(&s, "type") == Some("paragraph") && attr(&s, "default") == Some("1") {
                self.default_style = id.to_string();
            }
            let style = Style {
                name: first(&s, "name").and_then(|n| attr(&n, "val")).unwrap_or(id).to_string(),
                based: first(&s, "basedOn").and_then(|n| attr(&n, "val")).map(str::to_string),
                run: first(&s, "rPr").map(|r| parse_run_props(&r, &self.theme)).unwrap_or_default(),
                para: first(&s, "pPr").map(|p| parse_para_props(&p)).unwrap_or_default(),
            };
            self.styles.insert(id.to_string(), style);
        }
    }

    fn load_numbering(&mut self) {
        let Some(text) = read_text(&mut self.z, "word/numbering.xml") else { return };
        let Ok(doc) = xml(&text) else { return };
        for n in doc.descendants().filter(|n| n.is_element()) {
            match local(&n) {
                "abstractNum" => {
                    let Some(abs) = attr(&n, "abstractNumId") else { continue };
                    for lvl in elements(&n).filter(|c| local(c) == "lvl") {
                        let ilvl: u32 = attr(&lvl, "ilvl").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let ind = first(&lvl, "pPr").and_then(|p| first(&p, "ind"));
                        self.levels.insert(
                            (abs.to_string(), ilvl),
                            LevelDef {
                                fmt: first(&lvl, "numFmt").and_then(|f| attr(&f, "val")).unwrap_or("decimal").to_string(),
                                text: first(&lvl, "lvlText").and_then(|f| attr(&f, "val")).unwrap_or("").to_string(),
                                start: first(&lvl, "start").and_then(|f| attr(&f, "val")).and_then(|v| v.parse().ok()).unwrap_or(1),
                                left: ind.and_then(|i| num(attr(&i, "left").or(attr(&i, "start")))).map(|t| t / 20.0),
                                hanging: ind.and_then(|i| num(attr(&i, "hanging"))).map(|t| t / 20.0),
                            },
                        );
                    }
                }
                "num" => {
                    if let (Some(id), Some(abs)) = (attr(&n, "numId"), first(&n, "abstractNumId").and_then(|a| attr(&a, "val"))) {
                        self.num_to_abs.insert(id.to_string(), abs.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    /// A style with everything it is based on folded in, base first.
    fn resolved(&self, id: &str) -> Style {
        let mut chain = Vec::new();
        let mut cur = Some(id.to_string());
        while let Some(c) = cur {
            if chain.len() > 12 {
                break;
            }
            match self.styles.get(&c) {
                Some(s) => {
                    cur = s.based.clone();
                    chain.push(s.clone());
                }
                None => break,
            }
        }
        let mut out = Style::default();
        for s in chain.iter().rev() {
            out.run.over(&s.run);
            out.para.over(&s.para);
            out.name = s.name.clone();
        }
        out
    }

    fn marker(&mut self, num_id: &str, ilvl: u32) -> Option<(String, LevelDef)> {
        let abs = self.num_to_abs.get(num_id)?.clone();
        let def = self.levels.get(&(abs.clone(), ilvl))?.clone();
        if def.fmt == "none" {
            return None;
        }
        let counters = self.counters.entry(num_id.to_string()).or_insert([0; 9]);
        let i = (ilvl as usize).min(8);
        counters[i] = if counters[i] == 0 { def.start } else { counters[i] + 1 };
        for deeper in counters.iter_mut().skip(i + 1) {
            *deeper = 0;
        }
        let snapshot = *counters;
        if def.fmt == "bullet" {
            return Some((["\u{2022}", "\u{25e6}", "\u{25aa}"][i.min(2)].to_string(), def));
        }
        let mut text = def.text.clone();
        for lvl in 0..9u32 {
            let token = format!("%{}", lvl + 1);
            if text.contains(&token) {
                let fmt = self.levels.get(&(abs.clone(), lvl)).map(|l| l.fmt.clone()).unwrap_or_else(|| "decimal".into());
                text = text.replace(&token, &format_counter(&fmt, snapshot[lvl as usize].max(1)));
            }
        }
        Some((text, def))
    }

    fn paragraph(&mut self, p: &Node) -> String {
        let ppr = first(p, "pPr");
        let style_id = ppr
            .and_then(|n| first(&n, "pStyle"))
            .and_then(|n| attr(&n, "val"))
            .unwrap_or(&self.default_style)
            .to_string();
        let style = self.resolved(&style_id);
        let mut run = self.def_run.clone();
        run.over(&style.run);
        let mut para = self.def_para.clone();
        para.over(&style.para);
        let direct = ppr.map(|n| parse_para_props(&n)).unwrap_or_default();
        para.over(&direct);
        if let Some(rpr) = ppr.and_then(|n| first(&n, "rPr")) {
            // The paragraph mark's own formatting only matters for empty paragraphs.
            if first(p, "r").is_none() {
                run.over(&parse_run_props(&rpr, &self.theme));
            }
        }

        let name = style.name.to_lowercase();
        let tag = if name == "title" {
            "h1".to_string()
        } else if let Some(level) = name.strip_prefix("heading ").and_then(|n| n.trim().parse::<u32>().ok()) {
            format!("h{}", level.clamp(1, 6))
        } else {
            "p".to_string()
        };

        // Bullets and numbering.
        let num_pr = ppr.and_then(|n| first(&n, "numPr"));
        let mut marker_html = String::new();
        if let Some(np) = num_pr {
            let id = first(&np, "numId").and_then(|n| attr(&n, "val")).unwrap_or("0").to_string();
            let ilvl = first(&np, "ilvl").and_then(|n| attr(&n, "val")).and_then(|v| v.parse().ok()).unwrap_or(0);
            if id != "0" {
                if let Some((text, def)) = self.marker(&id, ilvl) {
                    let hang = para.hanging.or(def.hanging).unwrap_or(18.0);
                    if direct.left.is_none() {
                        para.left = def.left.or(Some(36.0 * (ilvl + 1) as f32));
                    }
                    para.hanging = Some(hang);
                    marker_html = format!("<span style=\"display:inline-block;min-width:{hang}pt;text-indent:0\">{}</span>", esc(&text));
                }
            }
        }

        let mut inner = String::new();
        self.inline(p, &run, &mut inner);
        if inner.is_empty() && marker_html.is_empty() {
            inner.push_str("&nbsp;");
        }

        let mut css = String::new();
        if let Some(j) = &para.jc {
            css.push_str(&format!("text-align:{j};"));
        }
        css.push_str(&format!("margin-top:{}pt;margin-bottom:{}pt;", para.before.unwrap_or(0.0), para.after.unwrap_or(0.0)));
        if let Some(l) = &para.line {
            css.push_str(&format!("line-height:{l};"));
        }
        if let Some(left) = para.left {
            css.push_str(&format!("padding-left:{left}pt;"));
        }
        if let Some(h) = para.hanging {
            css.push_str(&format!("text-indent:-{h}pt;"));
        } else if let Some(f) = para.first {
            css.push_str(&format!("text-indent:{f}pt;"));
        }
        if para.keep_break {
            css.push_str("break-before:page;");
        }
        if let Some(sz) = run.size {
            css.push_str(&format!("font-size:{sz}pt;"));
        }
        if let Some(fill) = ppr.and_then(|n| first(&n, "shd")).and_then(|s| attr(&s, "fill")).filter(|f| *f != "auto").and_then(color_css) {
            css.push_str(&format!("background:{fill};"));
        }
        // A positioned frame (what PDF conversions and text boxes use) sits at absolute page coordinates.
        if let Some(fr) = ppr.and_then(|n| first(&n, "framePr")) {
            if let (Some(x), Some(y)) = (num(attr(&fr, "x")), num(attr(&fr, "y"))) {
                let left = x / 20.0 - if attr(&fr, "hAnchor") == Some("page") { self.margin_left } else { 0.0 };
                let top = y / 20.0 - if attr(&fr, "vAnchor") == Some("page") { self.margin_top } else { 0.0 };
                css.push_str(&format!("position:absolute;left:{left}pt;top:{top}pt;box-sizing:border-box;"));
                if let Some(w) = num(attr(&fr, "w")) {
                    css.push_str(&format!("width:{}pt;", w / 20.0));
                }
                if let Some(h) = num(attr(&fr, "h")) {
                    if attr(&fr, "hRule") == Some("exact") {
                        css.push_str(&format!("height:{}pt;overflow:hidden;", h / 20.0));
                    } else {
                        css.push_str(&format!("min-height:{}pt;", h / 20.0));
                    }
                }
            }
        }
        format!("<{tag} style=\"{css}\">{marker_html}{inner}</{tag}>")
    }

    fn inline(&mut self, parent: &Node, base: &RunProps, out: &mut String) {
        for c in elements(parent) {
            match local(&c) {
                "r" => out.push_str(&self.run(&c, base)),
                "hyperlink" => {
                    let mut inner = String::new();
                    self.inline(&c, base, &mut inner);
                    match attr(&c, "id").and_then(|id| self.rels.get(id)).and_then(|t| t.strip_prefix("ext:")) {
                        Some(url) => out.push_str(&format!("<a href=\"{}\">{inner}</a>", esc(url))),
                        None => out.push_str(&inner),
                    }
                }
                "ins" | "smartTag" | "sdt" | "sdtContent" | "fldSimple" | "customXml" => self.inline(&c, base, out),
                _ => {}
            }
        }
    }

    fn run(&mut self, r: &Node, base: &RunProps) -> String {
        let mut props = base.clone();
        if let Some(rpr) = first(r, "rPr") {
            if let Some(id) = first(&rpr, "rStyle").and_then(|n| attr(&n, "val")) {
                let s = self.resolved(id);
                props.over(&s.run);
            }
            props.over(&parse_run_props(&rpr, &self.theme));
        }
        let mut inner = String::new();
        for c in elements(r) {
            match local(&c) {
                "t" => inner.push_str(&esc(c.text().unwrap_or(""))),
                "tab" => inner.push_str("&emsp;"),
                "br" | "cr" => {
                    if attr(&c, "type") == Some("page") {
                        inner.push_str("<span style=\"display:block;break-after:page\"></span>");
                    } else {
                        inner.push_str("<br>");
                    }
                }
                "noBreakHyphen" => inner.push_str("&#8209;"),
                "drawing" | "pict" => inner.push_str(&self.drawing(&c)),
                _ => {}
            }
        }
        if inner.is_empty() {
            return String::new();
        }
        format!("<span style=\"{}\">{inner}</span>", props.css())
    }

    fn drawing(&mut self, d: &Node) -> String {
        let Some(id) = d.descendants().filter(|n| n.is_element() && local(n) == "blip").find_map(|b| attr(&b, "embed")) else {
            return String::new();
        };
        let Some(path) = self.rels.get(id).cloned() else { return String::new() };
        let Some(uri) = image_uri(&mut self.z, &path) else { return String::new() };
        let extent = descendant(d, "extent").and_then(|e| Some((num(attr(&e, "cx"))? / 12700.0, num(attr(&e, "cy"))? / 12700.0)));
        // A floating picture is placed by its offsets from the page or margin.
        if let Some(anchor) = d.children().find(|c| c.is_element() && local(c) == "anchor") {
            let offset = |name: &str| {
                let n = first(&anchor, name)?;
                let v = first(&n, "posOffset")?.text()?.trim().parse::<f32>().ok()? / 12700.0;
                Some((v, attr(&n, "relativeFrom") == Some("page")))
            };
            if let (Some((x, xp)), Some((y, yp)), Some((w, h))) = (offset("positionH"), offset("positionV"), extent) {
                let left = x - if xp { self.margin_left } else { 0.0 };
                let top = y - if yp { self.margin_top } else { 0.0 };
                // relativeHeight orders floating objects; behind-text ones stay below the text.
                let order = attr(&anchor, "relativeHeight").and_then(|v| v.parse::<i64>().ok()).map(|h| (h - 251_658_240).clamp(0, 400_000)).unwrap_or(0);
                let z = if attr(&anchor, "behindDoc") == Some("1") { order - 500_000 } else { order + 1 };
                return format!("<img src=\"{uri}\" style=\"position:absolute;left:{left}pt;top:{top}pt;width:{w}pt;height:{h}pt;max-width:none;z-index:{z}\">");
            }
        }
        let size = extent.map(|(w, h)| format!(" style=\"width:{w}pt;height:{h}pt\"")).unwrap_or_default();
        format!("<img src=\"{uri}\"{size}>")
    }

    fn table(&mut self, t: &Node) -> String {
        let tbl_pr = first(t, "tblPr");
        let style_id = tbl_pr.and_then(|p| first(&p, "tblStyle")).and_then(|s| attr(&s, "val")).unwrap_or("");
        let has_borders = tbl_pr
            .and_then(|p| first(&p, "tblBorders"))
            .map(|b| elements(&b).any(|e| !matches!(attr(&e, "val"), Some("nil") | Some("none"))))
            .unwrap_or(false)
            || style_id.to_lowercase().contains("grid");

        // Work out each cell's column span and vertical merge first, so row spans can be counted.
        struct Cell<'a, 'i> {
            node: Node<'a, 'i>,
            start: usize,
            span: usize,
            merge: u8, // 0 none, 1 restart, 2 continue
        }
        let mut grid: Vec<Vec<Cell>> = Vec::new();
        for tr in elements(t).filter(|n| local(n) == "tr") {
            let mut col = 0;
            let mut row = Vec::new();
            for tc in elements(&tr).filter(|n| local(n) == "tc") {
                let pr = first(&tc, "tcPr");
                let span = pr.and_then(|p| first(&p, "gridSpan")).and_then(|g| attr(&g, "val")).and_then(|v| v.parse().ok()).unwrap_or(1usize);
                let merge = match pr.and_then(|p| first(&p, "vMerge")) {
                    None => 0,
                    Some(v) if attr(&v, "val") == Some("restart") => 1,
                    Some(_) => 2,
                };
                row.push(Cell { node: tc, start: col, span, merge });
                col += span;
            }
            grid.push(row);
        }

        let mut html = String::from("<table style=\"border-collapse:collapse;margin:4pt 0\">");
        for (ri, row) in grid.iter().enumerate() {
            html.push_str("<tr>");
            for cell in row {
                if cell.merge == 2 {
                    continue;
                }
                let rowspan = if cell.merge == 1 {
                    1 + grid[ri + 1..]
                        .iter()
                        .take_while(|r| r.iter().any(|c| c.start == cell.start && c.merge == 2))
                        .count()
                } else {
                    1
                };
                let pr = first(&cell.node, "tcPr");
                let mut css = String::from("vertical-align:top;padding:2pt 5.4pt;");
                if has_borders {
                    css.push_str("border:0.5pt solid #000;");
                }
                if let Some(fill) = pr.and_then(|p| first(&p, "shd")).and_then(|s| attr(&s, "fill")).filter(|f| *f != "auto").and_then(color_css) {
                    css.push_str(&format!("background:{fill};"));
                }
                if let Some(w) = pr.and_then(|p| first(&p, "tcW")).and_then(|w| num(attr(&w, "w"))) {
                    css.push_str(&format!("width:{}pt;", w / 20.0));
                }
                let mut content = String::new();
                for child in elements(&cell.node) {
                    match local(&child) {
                        "p" => content.push_str(&self.paragraph(&child)),
                        "tbl" => content.push_str(&self.table(&child)),
                        _ => {}
                    }
                }
                html.push_str(&format!("<td colspan=\"{}\" rowspan=\"{rowspan}\" style=\"{css}\">{content}</td>", cell.span));
            }
            html.push_str("</tr>");
        }
        html.push_str("</table>");
        html
    }
}

struct Section<'a, 'i> {
    items: Vec<Node<'a, 'i>>,
    w: f32,
    h: f32,
    margins: [f32; 4], // top right bottom left, in points
}

fn section_of(sect: Option<Node>) -> (f32, f32, [f32; 4]) {
    let size = sect.and_then(|s| first(&s, "pgSz"));
    let (mut w, mut h) = (11906.0, 16838.0); // A4 in twips
    if let Some(s) = size {
        w = num(attr(&s, "w")).unwrap_or(w);
        h = num(attr(&s, "h")).unwrap_or(h);
    }
    let mar = sect.and_then(|s| first(&s, "pgMar"));
    let m = |name: &str| mar.and_then(|m| num(attr(&m, name))).unwrap_or(1440.0) / 20.0;
    (w / 20.0, h / 20.0, [m("top"), m("right"), m("bottom"), m("left")])
}

/// Splits the body at section breaks; each section has its own page size and margins.
fn sections<'a, 'i>(body: &Node<'a, 'i>) -> Vec<Section<'a, 'i>> {
    let mut out = Vec::new();
    let mut items = Vec::new();
    for c in elements(body) {
        match local(&c) {
            "p" | "tbl" | "sdt" => {
                items.push(c);
                if let Some(sp) = first(&c, "pPr").and_then(|p| first(&p, "sectPr")) {
                    let (w, h, margins) = section_of(Some(sp));
                    out.push(Section { items: std::mem::take(&mut items), w, h, margins });
                }
            }
            "sectPr" => {
                let (w, h, margins) = section_of(Some(c));
                out.push(Section { items: std::mem::take(&mut items), w, h, margins });
            }
            _ => {}
        }
    }
    if !items.is_empty() || out.is_empty() {
        let (w, h, margins) = section_of(None);
        out.push(Section { items, w, h, margins });
    }
    out
}

pub fn docx_to_html(path: &Path) -> Result<String> {
    let (mut d, body_xml) = Docx::open(path)?;
    let doc = xml(&body_xml)?;
    let body = descendant(&doc.root(), "body").ok_or("Empty document")?;
    let secs = sections(&body);
    let mut html = String::new();
    let mut page_css = String::new();
    for (i, sec) in secs.iter().enumerate() {
        let [mt, mr, mb, ml] = sec.margins;
        d.margin_left = ml;
        d.margin_top = mt;
        page_css.push_str(&format!("@page s{i}{{size:{}pt {}pt;margin:{mt}pt {mr}pt {mb}pt {ml}pt}}", sec.w, sec.h));
        let mut inner = String::new();
        for c in &sec.items {
            match local(c) {
                "p" => inner.push_str(&d.paragraph(c)),
                "tbl" => inner.push_str(&d.table(c)),
                "sdt" => {
                    if let Some(content) = first(c, "sdtContent") {
                        for item in elements(&content) {
                            match local(&item) {
                                "p" => inner.push_str(&d.paragraph(&item)),
                                "tbl" => inner.push_str(&d.table(&item)),
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Absolutely placed frames don't take up room, so give the section its page height.
        let has_frames = sec.items.iter().any(|n| n.descendants().any(|x| x.is_element() && local(&x) == "framePr"));
        let height = if has_frames { format!("height:{}pt;", (sec.h - mt - mb - 1.0).max(1.0)) } else { String::new() };
        let brk = if i + 1 < secs.len() { "break-after:page;" } else { "" };
        html.push_str(&format!("<div style=\"page:s{i};position:relative;isolation:isolate;{height}{brk}\">{inner}</div>"));
    }
    let mut base = d.def_run.clone();
    base.over(&d.resolved(&d.default_style.clone()).run);
    let font = base.font.clone().or(d.theme.minor.clone()).unwrap_or_else(|| "Calibri".into());
    let size = base.size.unwrap_or(11.0);
    Ok(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>{page_css}\
         html,body{{margin:0;padding:0}}body{{font:{size}pt '{font}',sans-serif;line-height:1.08;color:#000}}\
         p,h1,h2,h3,h4,h5,h6{{margin:0;font-size:inherit;font-weight:inherit;white-space:pre-wrap;overflow-wrap:anywhere}}\
         img{{max-width:100%;height:auto}}a{{color:#0563c1}}td p{{margin:0}}</style></head><body>{html}</body></html>"
    ))
}

/// Plain text of a .docx: one line per paragraph, table cells tab-separated.
pub fn docx_to_text(path: &Path) -> Result<String> {
    let mut z = open_zip(path)?;
    let text = read_text(&mut z, "word/document.xml").ok_or("Not a Word document")?;
    let doc = xml(&text)?;
    fn para_text(p: &Node) -> String {
        let mut s = String::new();
        for n in p.descendants().filter(|n| n.is_element()) {
            match local(&n) {
                "t" => s.push_str(n.text().unwrap_or("")),
                "tab" => s.push('\t'),
                "br" | "cr" => s.push('\n'),
                _ => {}
            }
        }
        s
    }
    fn walk(n: &Node, out: &mut Vec<String>) {
        for c in elements(n) {
            match local(&c) {
                "p" => out.push(para_text(&c)),
                "tbl" => {
                    for tr in elements(&c).filter(|r| local(r) == "tr") {
                        let cells: Vec<String> = elements(&tr)
                            .filter(|x| local(x) == "tc")
                            .map(|tc| elements(&tc).filter(|p| local(p) == "p").map(|p| para_text(&p)).collect::<Vec<_>>().join(" "))
                            .collect();
                        out.push(cells.join("\t"));
                    }
                }
                "body" | "sdt" | "sdtContent" => walk(&c, out),
                _ => {}
            }
        }
    }
    let mut lines = Vec::new();
    walk(&descendant(&doc.root(), "body").ok_or("Empty document")?, &mut lines);
    Ok(lines.join("\r\n") + "\r\n")
}
