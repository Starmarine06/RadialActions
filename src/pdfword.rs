//! PDF -> Word with the original layout, without Word or any other program.
//!
//! The PDF's content streams are interpreted to recover every piece of text
//! (position, font, size, colour), picture, and drawn line or rectangle. Text is
//! grouped into lines and blocks, and the result is written as a .docx where each
//! block is a positioned frame holding real, editable text, pictures are
//! anchored where they were, and rectangles and rules are exact-size shaded
//! frames. Every page keeps its own size.

use crate::convert::{unique_sibling, Result};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

type M = [f32; 6];
const IDENTITY: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` applied first, then `b` (PDF's row-vector convention).
fn mul(a: M, b: M) -> M {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn apply(m: &M, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn number(o: &Object) -> Option<f32> {
    match o {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

fn deref<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    match o {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(o),
        other => other,
    }
}

type Rgb = (u8, u8, u8);

fn to_rgb(c: (f32, f32, f32)) -> Rgb {
    let f = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (f(c.0), f(c.1), f(c.2))
}

// ---- fonts --------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Face {
    family: String,
    bold: bool,
    italic: bool,
}

/// "ABCDEF+TimesNewRomanPS-BoldMT" -> Times New Roman, bold.
fn face_from_name(base: &str, flags: i64) -> Face {
    let name = base.split('+').last().unwrap_or(base);
    let lower = name.to_lowercase();
    let bold = lower.contains("bold") || lower.contains("black") || lower.contains("heavy") || lower.contains("semibold") || flags & (1 << 18) != 0;
    let italic = lower.contains("italic") || lower.contains("oblique") || flags & 64 != 0;
    let family_part = name.split(|c| c == '-' || c == ',').next().unwrap_or(name);
    let mut family = family_part.to_string();
    for tail in ["PSMT", "MT", "PS"] {
        if family.len() > tail.len() + 2 && family.ends_with(tail) {
            family.truncate(family.len() - tail.len());
            break;
        }
    }
    let mapped = match family.to_lowercase().as_str() {
        "helvetica" | "arial" => "Arial".to_string(),
        "times" | "timesroman" | "timesnewroman" => "Times New Roman".to_string(),
        "courier" | "couriernew" => "Courier New".to_string(),
        "symbol" => "Symbol".to_string(),
        _ => {
            // CamelCase -> words: "SegoeUI" -> "Segoe UI", "ArialNarrow" -> "Arial Narrow".
            let mut out = String::new();
            let chars: Vec<char> = family.chars().collect();
            for (i, c) in chars.iter().enumerate() {
                if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
                    out.push(' ');
                }
                out.push(*c);
            }
            if out.is_empty() { "Arial".to_string() } else { out }
        }
    };
    Face { family: mapped, bold, italic }
}

struct Widths {
    first: i64,
    list: Vec<f32>,
    missing: f32,
    cid: HashMap<u32, f32>,
    dw: f32,
    default: f32,
}

impl Widths {
    /// Advance of a character code in 1/1000 em.
    fn of(&self, code: u32, two_byte: bool) -> f32 {
        if two_byte {
            return self.cid.get(&code).copied().unwrap_or(self.dw);
        }
        let i = code as i64 - self.first;
        if i >= 0 && (i as usize) < self.list.len() {
            let w = self.list[i as usize];
            if w > 0.0 { w } else { self.missing }
        } else if self.list.is_empty() {
            self.default
        } else {
            self.missing
        }
    }
}

struct FontEntry<'a> {
    enc: Option<Encoding<'a>>,
    two_byte: bool,
    widths: Widths,
    face: Face,
}

fn load_font<'a>(doc: &'a Document, dict: &'a Dictionary) -> FontEntry<'a> {
    let subtype = dict.get(b"Subtype").and_then(|o| o.as_name()).unwrap_or(b"");
    let two_byte = subtype == b"Type0";
    let target: &Dictionary = if two_byte {
        dict.get(b"DescendantFonts")
            .ok()
            .map(|o| deref(doc, o))
            .and_then(|o| o.as_array().ok())
            .and_then(|a| a.first())
            .map(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())
            .unwrap_or(dict)
    } else {
        dict
    };
    let descriptor = target.get(b"FontDescriptor").ok().map(|o| deref(doc, o)).and_then(|o| o.as_dict().ok());
    let flags = descriptor.and_then(|d| d.get(b"Flags").ok()).and_then(|o| o.as_i64().ok()).unwrap_or(0);
    let base = dict
        .get(b"BaseFont")
        .and_then(|o| o.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();
    let mut face = face_from_name(&base, flags);
    if descriptor.and_then(|d| d.get(b"ItalicAngle").ok()).and_then(number).is_some_and(|a| a != 0.0) {
        face.italic = true;
    }

    let mut w = Widths {
        first: dict.get(b"FirstChar").and_then(|o| o.as_i64()).unwrap_or(0),
        list: Vec::new(),
        missing: descriptor.and_then(|d| d.get(b"MissingWidth").ok()).and_then(number).unwrap_or(0.0),
        cid: HashMap::new(),
        dw: target.get(b"DW").ok().and_then(number).unwrap_or(1000.0),
        default: if face.family == "Courier New" { 600.0 } else { 520.0 },
    };
    if let Some(arr) = dict.get(b"Widths").ok().map(|o| deref(doc, o)).and_then(|o| o.as_array().ok()) {
        w.list = arr.iter().map(|o| number(deref(doc, o)).unwrap_or(0.0)).collect();
    }
    if two_byte {
        if let Some(arr) = target.get(b"W").ok().map(|o| deref(doc, o)).and_then(|o| o.as_array().ok()) {
            let mut i = 0;
            while i < arr.len() {
                let Some(start) = number(deref(doc, &arr[i])) else { break };
                match arr.get(i + 1).map(|o| deref(doc, o)) {
                    Some(Object::Array(list)) => {
                        for (k, v) in list.iter().enumerate() {
                            if let Some(x) = number(deref(doc, v)) {
                                w.cid.insert(start as u32 + k as u32, x);
                            }
                        }
                        i += 2;
                    }
                    Some(other) => {
                        let (Some(end), Some(width)) = (number(other), arr.get(i + 2).and_then(|o| number(deref(doc, o)))) else { break };
                        for c in (start as u32)..=(end as u32).min(start as u32 + 70000) {
                            w.cid.insert(c, width);
                        }
                        i += 3;
                    }
                    None => break,
                }
            }
        }
    }
    FontEntry { enc: dict.get_font_encoding(doc).ok(), two_byte, widths: w, face }
}

fn load_fonts<'a>(doc: &'a Document, resources: &'a Dictionary) -> HashMap<Vec<u8>, FontEntry<'a>> {
    let mut out = HashMap::new();
    let Some(fonts) = resources.get(b"Font").ok().map(|o| deref(doc, o)).and_then(|o| o.as_dict().ok()) else { return out };
    for (name, value) in fonts.iter() {
        if let Ok(dict) = deref(doc, value).as_dict() {
            out.insert(name.clone(), load_font(doc, dict));
        }
    }
    out
}

// ---- page model ------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Run {
    x: f32,
    y: f32, // baseline, page coordinates with the origin at the top left
    w: f32,
    size: f32,
    face: Face,
    color: Rgb,
    text: String,
}

#[derive(Clone, Debug)]
struct Shape {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Rgb,
    /// Position in the page's paint order, shared with pictures.
    order: usize,
}

struct Pic {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    ext: &'static str,
    data: Vec<u8>,
    order: usize,
}

struct Page {
    w: f32,
    h: f32,
    runs: Vec<Run>,
    shapes: Vec<Shape>,
    pics: Vec<Pic>,
}

#[derive(Clone)]
struct Gs {
    ctm: M,
    fill: (f32, f32, f32),
    stroke: (f32, f32, f32),
    line_width: f32,
}

struct SubPath {
    pts: Vec<(f32, f32)>,
    /// Came from `re` and is axis-aligned.
    rect: bool,
}

fn flush_sub(subpaths: &mut Vec<SubPath>, cur: &mut Vec<(f32, f32)>) {
    if cur.len() >= 2 {
        subpaths.push(SubPath { pts: std::mem::take(cur), rect: false });
    } else {
        cur.clear();
    }
}

struct Walker<'a> {
    doc: &'a Document,
    page: Page,
    x0: f32,
    y1: f32,
    seen_images: HashMap<ObjectId, Option<(&'static str, Vec<u8>)>>,
    seq: usize,
}

const MAX_SHAPES: usize = 1500;
const MAX_VECTOR_PICS: usize = 250;

impl<'a> Walker<'a> {
    fn to_page(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.x0, self.y1 - y)
    }

    fn walk(&mut self, ops: &[lopdf::content::Operation], resources: &'a Dictionary, base: M, depth: usize) {
        let fonts = load_fonts(self.doc, resources);
        let mut gs = Gs { ctm: base, fill: (0.0, 0.0, 0.0), stroke: (0.0, 0.0, 0.0), line_width: 1.0 };
        let mut stack: Vec<Gs> = Vec::new();
        let (mut tm, mut tlm) = (IDENTITY, IDENTITY);
        let (mut font_name, mut font_size): (Vec<u8>, f32) = (Vec::new(), 12.0);
        let (mut tc, mut tw, mut th, mut tl, mut rise) = (0.0f32, 0.0f32, 1.0f32, 0.0f32, 0.0f32);
        // Paths are kept in page coordinates (origin top left).
        let mut subpaths: Vec<SubPath> = Vec::new();
        let mut cur: Vec<(f32, f32)> = Vec::new();
        let mut fill_cs_components = 1usize;

        for op in ops {
            let a = &op.operands;
            let n = |i: usize| a.get(i).and_then(number).unwrap_or(0.0);
            match op.operator.as_str() {
                "q" => stack.push(gs.clone()),
                "Q" => {
                    if let Some(s) = stack.pop() {
                        gs = s;
                    }
                }
                "cm" if a.len() >= 6 => gs.ctm = mul([n(0), n(1), n(2), n(3), n(4), n(5)], gs.ctm),
                "w" => gs.line_width = n(0),
                // colour
                "g" => gs.fill = (n(0), n(0), n(0)),
                "G" => gs.stroke = (n(0), n(0), n(0)),
                "rg" => gs.fill = (n(0), n(1), n(2)),
                "RG" => gs.stroke = (n(0), n(1), n(2)),
                "k" => gs.fill = cmyk(n(0), n(1), n(2), n(3)),
                "K" => gs.stroke = cmyk(n(0), n(1), n(2), n(3)),
                "cs" => {
                    fill_cs_components = match a.first().and_then(|o| o.as_name().ok()) {
                        Some(b"DeviceRGB") | Some(b"CalRGB") => 3,
                        Some(b"DeviceCMYK") => 4,
                        Some(b"DeviceGray") | Some(b"CalGray") => 1,
                        _ => 3,
                    }
                }
                "sc" | "scn" => {
                    let vals: Vec<f32> = a.iter().filter_map(number).collect();
                    gs.fill = match vals.len() {
                        1 => (vals[0], vals[0], vals[0]),
                        3 => (vals[0], vals[1], vals[2]),
                        4 => cmyk(vals[0], vals[1], vals[2], vals[3]),
                        _ if fill_cs_components == 3 && vals.len() >= 3 => (vals[0], vals[1], vals[2]),
                        _ => gs.fill,
                    };
                }
                "SC" | "SCN" => {
                    let vals: Vec<f32> = a.iter().filter_map(number).collect();
                    gs.stroke = match vals.len() {
                        1 => (vals[0], vals[0], vals[0]),
                        3 => (vals[0], vals[1], vals[2]),
                        4 => cmyk(vals[0], vals[1], vals[2], vals[3]),
                        _ => gs.stroke,
                    };
                }
                // text state
                "BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                "Tf" => {
                    font_name = a.first().and_then(|o| o.as_name().ok()).map(|s| s.to_vec()).unwrap_or_default();
                    font_size = n(1);
                }
                "Tc" => tc = n(0),
                "Tw" => tw = n(0),
                "Tz" => th = n(0) / 100.0,
                "TL" => tl = n(0),
                "Ts" => rise = n(0),
                "Td" => {
                    tlm = mul([1.0, 0.0, 0.0, 1.0, n(0), n(1)], tlm);
                    tm = tlm;
                }
                "TD" => {
                    tl = -n(1);
                    tlm = mul([1.0, 0.0, 0.0, 1.0, n(0), n(1)], tlm);
                    tm = tlm;
                }
                "Tm" if a.len() >= 6 => {
                    tm = [n(0), n(1), n(2), n(3), n(4), n(5)];
                    tlm = tm;
                }
                "T*" => {
                    tlm = mul([1.0, 0.0, 0.0, 1.0, 0.0, -tl], tlm);
                    tm = tlm;
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    if matches!(op.operator.as_str(), "'" | "\"") {
                        if op.operator == "\"" {
                            tw = n(0);
                            tc = n(1);
                        }
                        tlm = mul([1.0, 0.0, 0.0, 1.0, 0.0, -tl], tlm);
                        tm = tlm;
                    }
                    let Some(font) = fonts.get(&font_name) else { continue };
                    let pieces: Vec<&Object> = match op.operator.as_str() {
                        "TJ" => a.first().and_then(|o| o.as_array().ok()).map(|v| v.iter().collect()).unwrap_or_default(),
                        "\"" => a.get(2).into_iter().collect(),
                        _ => a.first().into_iter().collect(),
                    };
                    let mut text = String::new();
                    let start = apply(&mul(tm, gs.ctm), 0.0, rise);
                    let mut advance_total = 0.0f32;
                    for piece in pieces {
                        match piece {
                            Object::String(bytes, _) => {
                                if let Some(enc) = &font.enc {
                                    if let Ok(s) = Document::decode_text(enc, bytes) {
                                        text.push_str(&s);
                                    }
                                }
                                let step = if font.two_byte { 2 } else { 1 };
                                for chunk in bytes.chunks(step) {
                                    let code = chunk.iter().fold(0u32, |acc, b| acc << 8 | *b as u32);
                                    let w0 = font.widths.of(code, font.two_byte) / 1000.0;
                                    let space = if !font.two_byte && code == 32 { tw } else { 0.0 };
                                    let tx = (w0 * font_size + tc + space) * th;
                                    advance_total += tx;
                                    tm = mul([1.0, 0.0, 0.0, 1.0, tx, 0.0], tm);
                                }
                            }
                            other => {
                                if let Some(adj) = number(other) {
                                    let tx = -adj / 1000.0 * font_size * th;
                                    // A big forward jump is a word gap.
                                    if adj < -180.0 && !text.ends_with(' ') && !text.is_empty() {
                                        text.push(' ');
                                    }
                                    advance_total += tx;
                                    tm = mul([1.0, 0.0, 0.0, 1.0, tx, 0.0], tm);
                                }
                            }
                        }
                    }
                    // Text matrix and CTM both scale the font size; either may carry it.
                    let render = mul(tm, gs.ctm);
                    let size = font_size * render[2].hypot(render[3]);
                    let hscale = render[0].hypot(render[1]).max(0.0001);
                    if !text.trim().is_empty() && size > 0.5 {
                        let (px, py) = self.to_page(start.0, start.1);
                        self.page.runs.push(Run {
                            x: px,
                            y: py,
                            w: advance_total.abs() * hscale,
                            size,
                            face: font.face.clone(),
                            color: to_rgb(gs.fill),
                            text,
                        });
                    }
                }
                // paths
                "m" => {
                    flush_sub(&mut subpaths, &mut cur);
                    let (x, y) = apply(&gs.ctm, n(0), n(1));
                    cur.push(self.to_page(x, y));
                }
                "l" => {
                    let (x, y) = apply(&gs.ctm, n(0), n(1));
                    cur.push(self.to_page(x, y));
                }
                "c" | "v" | "y" if !cur.is_empty() => {
                    let p0 = *cur.last().unwrap();
                    let pt = |i: usize| {
                        let (x, y) = apply(&gs.ctm, n(i), n(i + 1));
                        self.to_page(x, y)
                    };
                    let (p1, p2, p3) = match op.operator.as_str() {
                        "c" => (pt(0), pt(2), pt(4)),
                        "v" => (p0, pt(0), pt(2)),
                        _ => (pt(0), pt(2), pt(2)),
                    };
                    for i in 1..=14 {
                        let t = i as f32 / 14.0;
                        let u = 1.0 - t;
                        let (a0, a1, a2, a3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                        cur.push((a0 * p0.0 + a1 * p1.0 + a2 * p2.0 + a3 * p3.0, a0 * p0.1 + a1 * p1.1 + a2 * p2.1 + a3 * p3.1));
                    }
                }
                "h" => {
                    if let Some(first) = cur.first().copied() {
                        cur.push(first);
                    }
                }
                "re" => {
                    flush_sub(&mut subpaths, &mut cur);
                    let (x, y, w, h) = (n(0), n(1), n(2), n(3));
                    let c: Vec<(f32, f32)> = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
                        .iter()
                        .map(|(px, py)| {
                            let (dx, dy) = apply(&gs.ctm, *px, *py);
                            self.to_page(dx, dy)
                        })
                        .collect();
                    let axis = (c[0].1 - c[1].1).abs() < 0.01 && (c[1].0 - c[2].0).abs() < 0.01;
                    let mut pts = c.clone();
                    pts.push(c[0]);
                    subpaths.push(SubPath { pts, rect: axis });
                }
                "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "S" | "s" => {
                    flush_sub(&mut subpaths, &mut cur);
                    let op_name = op.operator.as_str();
                    let fills = matches!(op_name, "f" | "F" | "f*" | "B" | "B*" | "b" | "b*");
                    let strokes = matches!(op_name, "S" | "s" | "B" | "B*" | "b" | "b*");
                    let even_odd = op_name.ends_with('*');
                    let lw = (gs.line_width * gs.ctm[0].hypot(gs.ctm[1])).max(0.4);
                    self.paint(&subpaths, fills.then_some(gs.fill), strokes.then_some((gs.stroke, lw)), even_odd);
                    subpaths.clear();
                }
                "n" => {
                    subpaths.clear();
                    cur.clear();
                }
                "W" | "W*" => {}
                "Do" => {
                    let Some(name) = a.first().and_then(|o| o.as_name().ok()) else { continue };
                    let xobj = resources
                        .get(b"XObject")
                        .ok()
                        .map(|o| deref(self.doc, o))
                        .and_then(|o| o.as_dict().ok())
                        .and_then(|d| d.get(name).ok());
                    let Some(xobj) = xobj else { continue };
                    let id = xobj.as_reference().ok();
                    let Ok(stream) = deref(self.doc, xobj).as_stream() else { continue };
                    match stream.dict.get(b"Subtype").and_then(|o| o.as_name()).unwrap_or(b"") {
                        b"Image" => self.add_image(id, stream, &gs),
                        b"Form" if depth < 4 => {
                            let matrix = stream
                                .dict
                                .get(b"Matrix")
                                .ok()
                                .and_then(|o| o.as_array().ok())
                                .map(|m| {
                                    let v: Vec<f32> = m.iter().filter_map(number).collect();
                                    if v.len() == 6 { [v[0], v[1], v[2], v[3], v[4], v[5]] } else { IDENTITY }
                                })
                                .unwrap_or(IDENTITY);
                            let form_res = stream.dict.get(b"Resources").ok().map(|o| deref(self.doc, o)).and_then(|o| o.as_dict().ok()).unwrap_or(resources);
                            if let Ok(data) = stream.decompressed_content() {
                                if let Ok(content) = Content::decode(&data) {
                                    self.walk(&content.operations, form_res, mul(matrix, gs.ctm), depth + 1);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    /// A filled box in page coordinates. White boxes are skipped: the page is white already.
    fn add_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: (f32, f32, f32)) {
        let rgb = to_rgb(color);
        if rgb == (255, 255, 255) || w < 0.2 || h < 0.2 || self.page.shapes.len() >= MAX_SHAPES {
            return;
        }
        self.seq += 1;
        self.page.shapes.push(Shape { x, y, w, h, color: rgb, order: self.seq });
    }

    /// Fills and strokes a finished path. Boxes and horizontal/vertical rules become
    /// exact shaded frames; anything curved or diagonal is drawn into a transparent picture.
    fn paint(&mut self, subs: &[SubPath], fill: Option<(f32, f32, f32)>, stroke: Option<((f32, f32, f32), f32)>, even_odd: bool) {
        if subs.is_empty() {
            return;
        }
        let simple = subs.iter().all(|s| {
            s.rect
                || (s.pts.len() == 2 && ((s.pts[0].1 - s.pts[1].1).abs() < 0.01 || (s.pts[0].0 - s.pts[1].0).abs() < 0.01))
        });
        if simple {
            for s in subs {
                if s.rect {
                    let (x0, x1) = (s.pts[0].0.min(s.pts[2].0), s.pts[0].0.max(s.pts[2].0));
                    let (y0, y1) = (s.pts[0].1.min(s.pts[2].1), s.pts[0].1.max(s.pts[2].1));
                    let (x, y, w, h) = (x0, y0, x1 - x0, y1 - y0);
                    if let Some(f) = fill {
                        self.add_rect(x, y, w, h, f);
                    }
                    if let Some((c, lw)) = stroke {
                        self.add_rect(x - lw / 2.0, y - lw / 2.0, w + lw, lw, c);
                        self.add_rect(x - lw / 2.0, y + h - lw / 2.0, w + lw, lw, c);
                        self.add_rect(x - lw / 2.0, y, lw, h, c);
                        self.add_rect(x + w - lw / 2.0, y, lw, h, c);
                    }
                } else if let Some((c, lw)) = stroke {
                    let (p, q) = (s.pts[0], s.pts[1]);
                    if (p.1 - q.1).abs() < 0.01 {
                        self.add_rect(p.0.min(q.0), p.1 - lw / 2.0, (p.0 - q.0).abs(), lw, c);
                    } else {
                        self.add_rect(p.0 - lw / 2.0, p.1.min(q.1), lw, (p.1 - q.1).abs(), c);
                    }
                }
            }
            return;
        }
        if self.page.pics.len() >= MAX_VECTOR_PICS || (fill.is_none() && stroke.is_none()) {
            return;
        }
        if let Some(mut pic) = rasterize(subs, fill.map(to_rgb), stroke.map(|(c, w)| (to_rgb(c), w)), even_odd) {
            self.seq += 1;
            pic.order = self.seq;
            self.page.pics.push(pic);
        }
    }

    fn add_image(&mut self, id: Option<ObjectId>, stream: &lopdf::Stream, gs: &Gs) {
        let corners = [apply(&gs.ctm, 0.0, 0.0), apply(&gs.ctm, 1.0, 0.0), apply(&gs.ctm, 1.0, 1.0), apply(&gs.ctm, 0.0, 1.0)];
        let (x0, x1) = (corners.iter().map(|c| c.0).fold(f32::MAX, f32::min), corners.iter().map(|c| c.0).fold(f32::MIN, f32::max));
        let (y0, y1) = (corners.iter().map(|c| c.1).fold(f32::MAX, f32::min), corners.iter().map(|c| c.1).fold(f32::MIN, f32::max));
        if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
            return;
        }
        let decoded = match id.and_then(|i| self.seen_images.get(&i).cloned()) {
            Some(d) => d,
            None => {
                let d = decode_image(self.doc, stream);
                if let Some(i) = id {
                    self.seen_images.insert(i, d.clone());
                }
                d
            }
        };
        if let Some((ext, data)) = decoded {
            let (px, py) = self.to_page(x0, y1);
            self.seq += 1;
            self.page.pics.push(Pic { x: px, y: py, w: x1 - x0, h: y1 - y0, ext, data, order: self.seq });
        }
    }
}

fn cmyk(c: f32, m: f32, y: f32, k: f32) -> (f32, f32, f32) {
    ((1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k))
}

/// PDF image -> a PNG or JPEG file's bytes, when it's a kind we can carry over.
fn decode_image(doc: &Document, stream: &lopdf::Stream) -> Option<(&'static str, Vec<u8>)> {
    let d = &stream.dict;
    if d.get(b"ImageMask").ok().and_then(|o| o.as_bool().ok()).unwrap_or(false) {
        return None;
    }
    let (w, h) = (d.get(b"Width").ok()?.as_i64().ok()? as u32, d.get(b"Height").ok()?.as_i64().ok()? as u32);
    let filters: Vec<String> = match d.get(b"Filter").ok().map(|o| deref(doc, o)) {
        Some(Object::Name(n)) => vec![String::from_utf8_lossy(n).to_string()],
        Some(Object::Array(a)) => a.iter().filter_map(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).to_string()).collect(),
        _ => Vec::new(),
    };
    if filters.last().map(String::as_str) == Some("DCTDecode") && filters.len() == 1 {
        // Re-encode when the JPEG isn't plain RGB/gray so colours stay right in Word.
        let img = image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg).ok()?;
        if matches!(img.color().channel_count(), 1 | 3) && !stream.content.windows(5).take(2000).any(|w| w == b"Adobe") {
            return Some(("jpeg", stream.content.clone()));
        }
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
        return Some(("png", out));
    }
    if filters.iter().any(|f| matches!(f.as_str(), "JBIG2Decode" | "JPXDecode" | "CCITTFaxDecode")) {
        return None;
    }
    let raw = stream.decompressed_content().ok()?;
    let bpc = d.get(b"BitsPerComponent").ok().and_then(|o| o.as_i64().ok()).unwrap_or(8);
    if bpc != 8 {
        return None;
    }
    let cs = d.get(b"ColorSpace").ok().map(|o| deref(doc, o));
    let comps = match cs {
        Some(Object::Name(n)) => match n.as_slice() {
            b"DeviceRGB" | b"CalRGB" => 3,
            b"DeviceGray" | b"CalGray" => 1,
            _ => return None,
        },
        Some(Object::Array(a)) => match a.first().and_then(|o| o.as_name().ok()) {
            Some(b"ICCBased") => a
                .get(1)
                .map(|o| deref(doc, o))
                .and_then(|o| o.as_stream().ok())
                .and_then(|s| s.dict.get(b"N").ok()?.as_i64().ok())
                .unwrap_or(3) as usize,
            Some(b"CalRGB") => 3,
            Some(b"CalGray") => 1,
            _ => return None,
        },
        _ => return None,
    };
    let expected = (w * h) as usize * comps;
    if raw.len() < expected || !(comps == 1 || comps == 3) {
        return None;
    }
    // Optional soft mask becomes the alpha channel.
    let alpha: Option<Vec<u8>> = d
        .get(b"SMask")
        .ok()
        .map(|o| deref(doc, o))
        .and_then(|o| o.as_stream().ok())
        .filter(|s| s.dict.get(b"Width").ok().and_then(|o| o.as_i64().ok()) == Some(w as i64) && s.dict.get(b"Height").ok().and_then(|o| o.as_i64().ok()) == Some(h as i64))
        .and_then(|s| s.decompressed_content().ok())
        .filter(|a| a.len() >= (w * h) as usize);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for i in 0..(w * h) as usize {
        let (r, g, b) = if comps == 3 { (raw[i * 3], raw[i * 3 + 1], raw[i * 3 + 2]) } else { (raw[i], raw[i], raw[i]) };
        rgba.extend_from_slice(&[r, g, b, alpha.as_ref().map(|a| a[i]).unwrap_or(255)]);
    }
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    Some(("png", out))
}

fn media_box(doc: &Document, page_id: ObjectId) -> (f32, f32, f32, f32) {
    let mut id = page_id;
    for _ in 0..16 {
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Some(arr) = dict.get(b"MediaBox").ok().map(|o| deref(doc, o)).and_then(|o| o.as_array().ok()) {
            let v: Vec<f32> = arr.iter().filter_map(|o| number(deref(doc, o))).collect();
            if v.len() == 4 {
                return (v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3]));
            }
        }
        match dict.get(b"Parent").and_then(|o| o.as_reference()) {
            Ok(p) => id = p,
            Err(_) => break,
        }
    }
    (0.0, 0.0, 595.0, 842.0)
}

fn page_resources<'a>(doc: &'a Document, page_id: ObjectId) -> Option<&'a Dictionary> {
    let mut id = page_id;
    for _ in 0..16 {
        let dict = doc.get_dictionary(id).ok()?;
        if let Some(r) = dict.get(b"Resources").ok().map(|o| deref(doc, o)).and_then(|o| o.as_dict().ok()) {
            return Some(r);
        }
        id = dict.get(b"Parent").and_then(|o| o.as_reference()).ok()?;
    }
    None
}

fn read_pages(doc: &Document) -> Vec<Page> {
    let mut pages = Vec::new();
    for (_, id) in doc.get_pages() {
        let (x0, y0, x1, y1) = media_box(doc, id);
        let mut walker = Walker {
            doc,
            page: Page { w: x1 - x0, h: y1 - y0, runs: Vec::new(), shapes: Vec::new(), pics: Vec::new() },
            x0,
            y1,
            seen_images: HashMap::new(),
            seq: 0,
        };
        let empty = Dictionary::new();
        // Leaked per page so fonts can borrow it for the walk; pages are few and short-lived.
        let resources = page_resources(doc, id).unwrap_or(Box::leak(Box::new(empty)));
        if let Ok(content) = doc.get_and_decode_page_content(id) {
            walker.walk(&content.operations, resources, IDENTITY, 0);
        }
        let _ = y0;
        pages.push(walker.page);
    }
    pages
}

// ---- vector art -> picture ---------------------------------------------------------------------------

const SUB: usize = 4; // sub-scanlines per pixel row

/// Fills polygons (non-zero or even-odd) with horizontal exact coverage and `SUB` vertical samples.
fn fill_coverage(polys: &[Vec<(f32, f32)>], w: usize, h: usize, even_odd: bool) -> Vec<f32> {
    let mut cov = vec![0.0f32; w * h];
    let mut crossings: Vec<Vec<(f32, i32)>> = vec![Vec::new(); h * SUB];
    for poly in polys {
        for e in poly.windows(2).chain(std::iter::once(&[*poly.last().unwrap(), poly[0]][..])) {
            let ((x0, y0), (x1, y1)) = (e[0], e[1]);
            if (y0 - y1).abs() < 1e-6 {
                continue;
            }
            let (dir, (xa, ya), (xb, yb)) = if y0 < y1 { (1, (x0, y0), (x1, y1)) } else { (-1, (x1, y1), (x0, y0)) };
            let first = ((ya * SUB as f32 - 0.5).ceil().max(0.0)) as usize;
            let last = ((yb * SUB as f32 - 0.5).floor().min((h * SUB) as f32 - 1.0)) as i64;
            if last < first as i64 {
                continue;
            }
            for sy in first..=(last as usize) {
                let y = (sy as f32 + 0.5) / SUB as f32;
                let t = (y - ya) / (yb - ya);
                crossings[sy].push((xa + t * (xb - xa), dir));
            }
        }
    }
    for (sy, row) in crossings.iter_mut().enumerate() {
        if row.len() < 2 {
            continue;
        }
        row.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let (mut wind, mut start) = (0i32, 0.0f32);
        for (x, dir) in row.iter() {
            let inside_before = if even_odd { wind % 2 != 0 } else { wind != 0 };
            wind += dir;
            let inside_after = if even_odd { wind % 2 != 0 } else { wind != 0 };
            if !inside_before && inside_after {
                start = *x;
            } else if inside_before && !inside_after {
                add_span(&mut cov[(sy / SUB) * w..(sy / SUB + 1) * w], start, *x, 1.0 / SUB as f32);
            }
        }
    }
    cov
}

fn add_span(row: &mut [f32], x0: f32, x1: f32, weight: f32) {
    let (a, b) = (x0.max(0.0), x1.min(row.len() as f32));
    if b <= a {
        return;
    }
    let (ia, ib) = (a as usize, (b.ceil() as usize).min(row.len()));
    for (i, px) in row.iter_mut().enumerate().take(ib).skip(ia) {
        let cover = (b.min(i as f32 + 1.0) - a.max(i as f32)).max(0.0);
        *px += cover * weight;
    }
}

/// Draws the path (fill, then outline) into a transparent PNG placed at its page position.
fn rasterize(subs: &[SubPath], fill: Option<Rgb>, stroke: Option<(Rgb, f32)>, even_odd: bool) -> Option<Pic> {
    let pad = stroke.map(|(_, w)| w).unwrap_or(0.0) + 1.0;
    let all: Vec<(f32, f32)> = subs.iter().flat_map(|s| s.pts.iter().copied()).collect();
    let (min_x, max_x) = (all.iter().map(|p| p.0).fold(f32::MAX, f32::min), all.iter().map(|p| p.0).fold(f32::MIN, f32::max));
    let (min_y, max_y) = (all.iter().map(|p| p.1).fold(f32::MAX, f32::min), all.iter().map(|p| p.1).fold(f32::MIN, f32::max));
    let (bx, by) = (min_x - pad, min_y - pad);
    let (bw, bh) = (max_x - min_x + 2.0 * pad, max_y - min_y + 2.0 * pad);
    if !(bw.is_finite() && bh.is_finite()) || bw < 1.0 || bh < 1.0 || bw > 4000.0 || bh > 4000.0 {
        return None;
    }
    let scale = (3.0f32).min(2400.0 / bw.max(bh));
    let (w, h) = ((bw * scale).ceil() as usize, (bh * scale).ceil() as usize);
    let to_px = |p: &(f32, f32)| ((p.0 - bx) * scale, (p.1 - by) * scale);

    let fill_cov = fill.map(|_| {
        let polys: Vec<Vec<(f32, f32)>> = subs.iter().filter(|s| s.pts.len() >= 3).map(|s| s.pts.iter().map(to_px).collect()).collect();
        fill_coverage(&polys, w, h, even_odd)
    });
    let stroke_cov = stroke.map(|(_, lw)| {
        let half = lw * scale / 2.0;
        let mut quads: Vec<Vec<(f32, f32)>> = Vec::new();
        for s in subs {
            for seg in s.pts.windows(2) {
                let (p, q) = (to_px(&seg[0]), to_px(&seg[1]));
                let (dx, dy) = (q.0 - p.0, q.1 - p.1);
                let len = dx.hypot(dy);
                if len < 1e-4 {
                    continue;
                }
                let (ux, uy) = (dx / len, dy / len);
                let (nx, ny) = (-uy * half, ux * half);
                let (ex, ey) = (ux * half, uy * half); // square caps close the joins
                quads.push(vec![(p.0 - ex + nx, p.1 - ey + ny), (q.0 + ex + nx, q.1 + ey + ny), (q.0 + ex - nx, q.1 + ey - ny), (p.0 - ex - nx, p.1 - ey - ny)]);
            }
        }
        // Each quad on its own so overlapping winding directions can't cancel.
        let mut total = vec![0.0f32; w * h];
        for chunk in quads.chunks(64) {
            for (t, c) in total.iter_mut().zip(fill_coverage(chunk, w, h, false)) {
                *t = (*t + c).min(1.0);
            }
        }
        total
    });

    let mut rgba = vec![0u8; w * h * 4];
    for i in 0..w * h {
        // Straight-alpha "over": outline on top of the fill.
        let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut put = |c: Rgb, cov: f32| {
            let cov = cov.clamp(0.0, 1.0);
            if cov <= 0.0 {
                return;
            }
            let out_a = cov + a * (1.0 - cov);
            let mix = |old: f32, new: f32| if out_a > 0.0 { (new * cov + old * a * (1.0 - cov)) / out_a } else { 0.0 };
            r = mix(r, c.0 as f32);
            g = mix(g, c.1 as f32);
            b = mix(b, c.2 as f32);
            a = out_a;
        };
        if let (Some(c), Some(cov)) = (fill, &fill_cov) {
            put(c, cov[i]);
        }
        if let (Some((c, _)), Some(cov)) = (stroke, &stroke_cov) {
            put(c, cov[i]);
        }
        rgba[i * 4..i * 4 + 4].copy_from_slice(&[r.round() as u8, g.round() as u8, b.round() as u8, (a * 255.0).round() as u8]);
    }
    let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(Pic { x: bx, y: by, w: bw, h: bh, ext: "png", data: png, order: 0 })
}

// ---- layout -------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Span {
    face: Face,
    size: f32,
    color: Rgb,
    text: String,
}

#[derive(Clone, Debug)]
struct Line {
    x: f32,
    x2: f32,
    y: f32,
    size: f32,
    spans: Vec<Span>,
}

struct Block {
    lines: Vec<Line>,
}

impl Block {
    fn x(&self) -> f32 {
        self.lines.iter().map(|l| l.x).fold(f32::MAX, f32::min)
    }
    fn x2(&self) -> f32 {
        self.lines.iter().map(|l| l.x2).fold(f32::MIN, f32::max)
    }
    fn last(&self) -> &Line {
        self.lines.last().unwrap()
    }
}

fn same_style(a: &Span, b: &Span) -> bool {
    a.face == b.face && (a.size - b.size).abs() < 0.3 && a.color == b.color
}

fn push_span(spans: &mut Vec<Span>, s: Span) {
    match spans.last_mut() {
        Some(last) if same_style(last, &s) => last.text.push_str(&s.text),
        _ => spans.push(s),
    }
}

/// Runs -> text lines. Lines split wherever a wide gap separates columns or cells.
fn build_lines(runs: &[Run], shapes: &[Shape]) -> Vec<Line> {
    // Thin vertical rules (table borders) always separate text on either side.
    let rules: Vec<&Shape> = shapes.iter().filter(|s| s.w <= 3.0 && s.h >= 4.0).collect();
    let mut sorted: Vec<&Run> = runs.iter().collect();
    sorted.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap().then(a.x.partial_cmp(&b.x).unwrap()));
    // Cluster by baseline.
    let mut clusters: Vec<Vec<&Run>> = Vec::new();
    for r in sorted {
        match clusters.iter_mut().rev().take(6).find(|c| {
            let base = c[0];
            (r.y - base.y).abs() <= 0.3 * r.size.max(base.size)
        }) {
            Some(c) => c.push(r),
            None => clusters.push(vec![r]),
        }
    }
    let mut lines = Vec::new();
    for mut c in clusters {
        c.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap());
        let mut cur: Option<Line> = None;
        let mut prev_end = 0.0f32;
        for r in c {
            let gap = r.x - prev_end;
            let start_new = match &cur {
                None => true,
                Some(l) => {
                    let crosses_rule = rules.iter().any(|s| s.x > prev_end - 1.0 && s.x < r.x + 1.0 && s.y <= r.y && s.y + s.h + 0.3 * r.size >= r.y);
                    gap > 0.9 * r.size.max(l.size) || crosses_rule
                }
            };
            if start_new {
                if let Some(l) = cur.take() {
                    lines.push(l);
                }
                cur = Some(Line { x: r.x, x2: r.x + r.w, y: r.y, size: r.size, spans: Vec::new() });
            } else if let Some(l) = cur.as_mut() {
                let needs_space = gap > 0.16 * r.size
                    && !l.spans.last().is_some_and(|s| s.text.ends_with(' '))
                    && !r.text.starts_with(' ');
                if needs_space {
                    // Keep the gap's width: roughly one space per quarter em.
                    let spaces = ((gap / (0.27 * r.size)).round() as usize).clamp(1, 8);
                    if let Some(last) = l.spans.last_mut() {
                        last.text.push_str(&" ".repeat(spaces));
                    }
                }
                l.x2 = l.x2.max(r.x + r.w);
                l.size = l.size.max(r.size);
            }
            if let Some(l) = cur.as_mut() {
                push_span(&mut l.spans, Span { face: r.face.clone(), size: r.size, color: r.color, text: r.text.clone() });
            }
            prev_end = r.x + r.w;
        }
        if let Some(l) = cur {
            lines.push(l);
        }
    }
    lines
}

/// Lines -> paragraphs: stacked lines with matching size and left edge belong together.
fn build_blocks(mut lines: Vec<Line>) -> Vec<Block> {
    lines.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap().then(a.x.partial_cmp(&b.x).unwrap()));
    let mut blocks: Vec<Block> = Vec::new();
    for line in lines {
        let best = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                let last = b.last();
                let step = line.y - last.y;
                let overlap = line.x < last.x2 && line.x2 > last.x;
                let size_ok = (line.size - last.size).abs() <= 0.2 * last.size.max(line.size);
                let aligned = (line.x - b.x()).abs() < 2.5 * line.size || (line.x - last.x).abs() < 0.8 * line.size;
                step > 0.55 * line.size && step < 1.9 * line.size.max(last.size) && overlap && size_ok && aligned
            })
            .min_by(|(_, a), (_, b)| (line.y - a.last().y).partial_cmp(&(line.y - b.last().y)).unwrap())
            .map(|(i, _)| i);
        match best {
            Some(i) => blocks[i].lines.push(line),
            None => blocks.push(Block { lines: vec![line] }),
        }
    }
    blocks
}

// ---- writing the .docx ----------------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

fn tw(pt: f32) -> i64 {
    (pt * 20.0).round() as i64
}

fn hex(c: Rgb) -> String {
    format!("{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

fn run_xml(span: &Span) -> String {
    let mut rpr = format!("<w:rFonts w:ascii=\"{0}\" w:hAnsi=\"{0}\" w:cs=\"{0}\"/>", esc(&span.face.family));
    if span.face.bold {
        rpr.push_str("<w:b/><w:bCs/>");
    }
    if span.face.italic {
        rpr.push_str("<w:i/><w:iCs/>");
    }
    if span.color != (0, 0, 0) {
        rpr.push_str(&format!("<w:color w:val=\"{}\"/>", hex(span.color)));
    }
    let half = ((span.size * 2.0).round() as i64).max(2);
    rpr.push_str(&format!("<w:sz w:val=\"{half}\"/><w:szCs w:val=\"{half}\"/>"));
    format!("<w:r><w:rPr>{rpr}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(&span.text))
}

fn frame_pr(x: f32, y: f32, w: f32, h: f32, exact_h: bool) -> String {
    format!(
        "<w:framePr w:w=\"{}\" w:h=\"{}\" w:hRule=\"{}\" w:hSpace=\"0\" w:vSpace=\"0\" w:wrap=\"around\" w:vAnchor=\"page\" w:hAnchor=\"page\" w:x=\"{}\" w:y=\"{}\"/>",
        tw(w).max(1),
        tw(h).max(1),
        if exact_h { "exact" } else { "atLeast" },
        tw(x),
        tw(y)
    )
}

fn block_xml(b: &Block, page_w: f32) -> String {
    let lines = &b.lines;
    let first = &lines[0];
    let step = if lines.len() > 1 {
        let mut gaps: Vec<f32> = lines.windows(2).map(|w| w[1].y - w[0].y).collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
        gaps[gaps.len() / 2]
    } else {
        first.size * 1.2
    };
    let line_h = step.max(first.size * 0.9);
    let (x, x2) = (b.x(), b.x2());
    let top = first.y - 0.8 * line_h;
    let width = ((x2 - x) * 1.14 + 8.0).min((page_w - x).max(20.0));
    let height = line_h * lines.len() as f32;

    let rest_min = lines.iter().skip(1).map(|l| l.x).fold(f32::MAX, f32::min);
    let left = if lines.len() > 1 { (rest_min - x).max(0.0) } else { 0.0 };
    let first_indent = (first.x - x) - left;
    let ind = if first_indent >= 0.0 {
        format!("<w:ind w:left=\"{}\" w:firstLine=\"{}\"/>", tw(left), tw(first_indent))
    } else {
        format!("<w:ind w:left=\"{}\" w:hanging=\"{}\"/>", tw(left), tw(-first_indent))
    };
    let mut body = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            body.push_str("<w:r><w:br/></w:r>");
        }
        for span in &line.spans {
            body.push_str(&run_xml(span));
        }
    }
    format!(
        "<w:p><w:pPr>{}<w:spacing w:before=\"0\" w:after=\"0\" w:line=\"{}\" w:lineRule=\"exact\"/>{ind}</w:pPr>{body}</w:p>",
        frame_pr(x, top, width, height, false),
        tw(line_h)
    )
}

/// A picture (or a stretched 1x1 colour swatch) pinned to page coordinates behind the text.
fn anchor_xml(x: f32, y: f32, w: f32, h: f32, rid: &str, id: usize, order: usize) -> String {
    let (cx, cy) = (((w * 12700.0) as i64).max(1), ((h * 12700.0) as i64).max(1));
    let (px, py) = ((x * 12700.0) as i64, (y * 12700.0) as i64);
    format!(
        "<w:r><w:drawing><wp:anchor distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\" simplePos=\"0\" relativeHeight=\"{}\" behindDoc=\"1\" locked=\"0\" layoutInCell=\"1\" allowOverlap=\"1\">\
         <wp:simplePos x=\"0\" y=\"0\"/><wp:positionH relativeFrom=\"page\"><wp:posOffset>{px}</wp:posOffset></wp:positionH>\
         <wp:positionV relativeFrom=\"page\"><wp:posOffset>{py}</wp:posOffset></wp:positionV>\
         <wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/><wp:wrapNone/>\
         <wp:docPr id=\"{id}\" name=\"Picture {id}\"/><wp:cNvGraphicFramePr/>\
         <a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:pic><pic:nvPicPr><pic:cNvPr id=\"{id}\" name=\"Picture {id}\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill><a:blip r:embed=\"{rid}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>\
         <pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic>\
         </a:graphicData></a:graphic></wp:anchor></w:drawing></w:r>",
        251658240 + order
    )
}

fn png_1x1(c: Rgb) -> Vec<u8> {
    let mut out = Vec::new();
    let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([c.0, c.1, c.2, 255]));
    let _ = image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

fn sect_pr(w: f32, h: f32) -> String {
    format!(
        "<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"/><w:pgMar w:top=\"0\" w:right=\"0\" w:bottom=\"0\" w:left=\"0\" w:header=\"0\" w:footer=\"0\" w:gutter=\"0\"/></w:sectPr>",
        tw(w),
        tw(h)
    )
}

fn write_docx(dst: &Path, pages: &[Page]) -> Result<()> {
    let mut media: Vec<(String, Vec<u8>)> = Vec::new();
    let mut rels = String::new();
    let mut body = String::new();
    let mut pic_id = 0usize;
    let mut solids: HashMap<Rgb, String> = HashMap::new();
    // A tiny paragraph keeps each page's frames anchored without adding height.
    let anchor_p = "<w:spacing w:before=\"0\" w:after=\"0\" w:line=\"20\" w:lineRule=\"exact\"/>";
    for (pi, page) in pages.iter().enumerate() {
        let last = pi + 1 == pages.len();
        // Every rectangle, rule and picture sits in one layer behind the text, in paint order.
        enum Gfx<'a> {
            Solid(&'a Shape),
            Picture(&'a Pic),
        }
        let mut layer: Vec<(usize, Gfx)> = page.shapes.iter().map(|s| (s.order, Gfx::Solid(s))).collect();
        layer.extend(page.pics.iter().map(|p| (p.order, Gfx::Picture(p))));
        layer.sort_by_key(|(order, _)| *order);
        let mut pics = String::new();
        for (order, g) in layer {
            pic_id += 1;
            match g {
                Gfx::Solid(s) => {
                    let rid = solids.entry(s.color).or_insert_with(|| {
                        let name = format!("solid_{}.png", hex(s.color));
                        let rid = format!("rIdSolid{}", hex(s.color));
                        rels.push_str(&format!("<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{name}\"/>"));
                        media.push((name, png_1x1(s.color)));
                        rid
                    });
                    pics.push_str(&anchor_xml(s.x, s.y, s.w, s.h, rid, pic_id, order));
                }
                Gfx::Picture(p) => {
                    let name = format!("image{pic_id}.{}", p.ext);
                    let rid = format!("rIdImg{pic_id}");
                    rels.push_str(&format!("<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/{name}\"/>"));
                    media.push((name, p.data.clone()));
                    pics.push_str(&anchor_xml(p.x, p.y, p.w, p.h, &rid, pic_id, order));
                }
            }
        }
        body.push_str(&format!("<w:p><w:pPr>{anchor_p}</w:pPr>{pics}</w:p>"));
        let mut blocks = build_blocks(build_lines(&page.runs, &page.shapes));
        blocks.sort_by(|a, b| a.lines[0].y.partial_cmp(&b.lines[0].y).unwrap().then(a.x().partial_cmp(&b.x()).unwrap()));
        for b in &blocks {
            body.push_str(&block_xml(b, page.w));
        }
        if last {
            body.push_str(&format!("<w:p><w:pPr>{anchor_p}</w:pPr></w:p>{}", sect_pr(page.w, page.h)));
        } else {
            body.push_str(&format!("<w:p><w:pPr>{anchor_p}{}</w:pPr></w:p>", sect_pr(page.w, page.h)));
        }
    }

    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
         xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" \
         xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><w:body>{body}</w:body></w:document>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
        <Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/>\
        <Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>";
    let root_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    let doc_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"
    );

    let mut zip = zip::ZipWriter::new(std::fs::File::create(dst).map_err(|e| e.to_string())?);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut put = |name: &str, data: &[u8]| -> Result<()> {
        zip.start_file(name, opts).map_err(|e| e.to_string())?;
        zip.write_all(data).map_err(|e| e.to_string())
    };
    put("[Content_Types].xml", content_types.as_bytes())?;
    put("_rels/.rels", root_rels.as_bytes())?;
    put("word/document.xml", document.as_bytes())?;
    put("word/_rels/document.xml.rels", doc_rels.as_bytes())?;
    for (name, data) in &media {
        put(&format!("word/media/{name}"), data)?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// A page that is one big picture and almost no real text.
fn is_scan(p: &Page) -> bool {
    p.runs.iter().map(|r| r.text.trim().len()).sum::<usize>() < 20 && p.pics.iter().any(|pic| pic.w * pic.h >= 0.6 * p.w * p.h)
}

/// Mean colour of the pixels in a one-rect-wide ring just outside `(x0,y0,x1,y1)`.
fn ring_colour(img: &image::RgbImage, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> (Rgb, f32) {
    let (w, h) = img.dimensions();
    let (mut sum, mut sq, mut n) = ([0u64; 3], [0u64; 3], 0u64);
    let mut take = |x: u32, y: u32| {
        let px = img.get_pixel(x.min(w - 1), y.min(h - 1));
        for c in 0..3 {
            sum[c] += px[c] as u64;
            sq[c] += (px[c] as u64).pow(2);
        }
        n += 1;
    };
    let (rx0, ry0, rx1, ry1) = (x0.saturating_sub(3), y0.saturating_sub(3), (x1 + 3).min(w - 1), (y1 + 3).min(h - 1));
    for x in rx0..=rx1 {
        take(x, ry0);
        take(x, ry1);
    }
    for y in ry0..=ry1 {
        take(rx0, y);
        take(rx1, y);
    }
    let n = n.max(1);
    let spread = (0..3).map(|c| (sq[c] as f32 / n as f32 - (sum[c] as f32 / n as f32).powi(2)).max(0.0).sqrt()).fold(0.0, f32::max);
    (((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8), spread)
}

fn luma(c: Rgb) -> i32 {
    (299 * c.0 as i32 + 587 * c.1 as i32 + 114 * c.2 as i32) / 1000
}

/// Runs text recognition on scanned pages. The scan stays as the backdrop with its text lines
/// wiped out, and the recognised text goes on top as editable text in the same place and colour.
fn read_scans(src: &Path, pages: &mut [Page]) -> Result<()> {
    let need: Vec<usize> = pages.iter().enumerate().filter(|(_, p)| is_scan(p)).map(|(i, _)| i).collect();
    if need.is_empty() {
        return Ok(());
    }
    for (i, png) in crate::pdf::render_some(src, 200.0, &need)? {
        let seen = crate::ocr::recognise(&png)?;
        if seen.lines.is_empty() {
            continue;
        }
        let mut img = image::load_from_memory(&png).map_err(|e| e.to_string())?.to_rgb8();
        let (iw, ih) = img.dimensions();
        let page = &mut pages[i];
        let (sx, sy) = (page.w / seen.width, page.h / seen.height);
        let mut runs = Vec::new();
        for l in &seen.lines {
            let rect = (
                (l.x as u32).saturating_sub(2).min(iw - 1),
                (l.y as u32).saturating_sub(2).min(ih - 1),
                ((l.x + l.w) as u32 + 2).min(iw - 1),
                ((l.y + l.h) as u32 + 2).min(ih - 1),
            );
            let (bg, spread) = ring_colour(&img, rect);
            // Text over a photo or gradient can't be wiped cleanly; leave it in the picture.
            if spread > 45.0 {
                continue;
            }
            // Text colour: the darkest pixel group inside the line, if it really contrasts.
            let mut px: Vec<Rgb> = Vec::new();
            for y in rect.1..=rect.3 {
                for x in rect.0..=rect.2 {
                    let p = img.get_pixel(x, y);
                    px.push((p[0], p[1], p[2]));
                }
            }
            px.sort_by_key(|c| (luma(*c) - luma(bg)).abs());
            let far = &px[px.len() - (px.len() / 20).max(1)..];
            let fg = (
                (far.iter().map(|c| c.0 as u32).sum::<u32>() / far.len() as u32) as u8,
                (far.iter().map(|c| c.1 as u32).sum::<u32>() / far.len() as u32) as u8,
                (far.iter().map(|c| c.2 as u32).sum::<u32>() / far.len() as u32) as u8,
            );
            let color = if (luma(fg) - luma(bg)).abs() < 60 { (0, 0, 0) } else { fg };
            for y in rect.1..=rect.3 {
                for x in rect.0..=rect.2 {
                    img.put_pixel(x, y, image::Rgb([bg.0, bg.1, bg.2]));
                }
            }
            let (h, w) = (l.h * sy, l.w * sx);
            let n = l.text.chars().count().max(1) as f32;
            // Arial averages about half an em per character; the line's height bounds the size.
            let by_height = h * 0.8;
            let size = (w / (0.5 * n)).clamp(by_height * 0.7, by_height * 1.1).max(4.0);
            runs.push(Run {
                x: l.x * sx,
                y: (l.y + l.h) * sy - h * 0.2,
                w,
                size,
                face: Face { family: "Arial".into(), bold: false, italic: false },
                color,
                text: l.text.clone(),
            });
        }
        // Lines of one size differ by ascenders and descenders; snap near-equal sizes together.
        let mut order: Vec<usize> = (0..runs.len()).collect();
        order.sort_by(|a, b| runs[*a].size.partial_cmp(&runs[*b].size).unwrap());
        let mut start = 0;
        while start < order.len() {
            let mut end = start + 1;
            while end < order.len() && runs[order[end]].size <= runs[order[start]].size * 1.15 {
                end += 1;
            }
            let median = runs[order[(start + end) / 2]].size;
            for k in start..end {
                runs[order[k]].size = median;
            }
            start = end;
        }
        let mut jpg = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
            .map_err(|e| e.to_string())?;
        page.pics.retain(|pic| pic.w * pic.h < 0.6 * page.w * page.h);
        page.pics.push(Pic { x: 0.0, y: 0.0, w: page.w, h: page.h, ext: "jpg", data: jpg, order: 0 });
        page.runs.extend(runs);
    }
    Ok(())
}

/// PDF -> .docx keeping layout, fonts, colours, pictures, rules and page sizes.
pub fn convert(src: &Path) -> Result<PathBuf> {
    let doc = Document::load(src).map_err(|e| format!("{}: {e}", src.file_name().and_then(|n| n.to_str()).unwrap_or("file")))?;
    if doc.is_encrypted() {
        return Err("The PDF is password protected".into());
    }
    let mut pages = read_pages(&doc);
    // Scans carry no text of their own; read it so the Word file is editable.
    let scan_err = read_scans(src, &mut pages).err();
    if pages.is_empty() || pages.iter().all(|p| p.runs.is_empty() && p.pics.is_empty() && p.shapes.is_empty()) {
        return Err("Nothing to convert on these pages".into());
    }
    if let Some(e) = scan_err.filter(|_| pages.iter().all(|p| p.runs.is_empty())) {
        return Err(e);
    }
    let dst = unique_sibling(src, "", "docx");
    if let Err(e) = write_docx(&dst, &pages) {
        let _ = std::fs::remove_file(&dst);
        return Err(e);
    }
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "debug: prints the drawing operators of page 1 of the PDF named by RA_DUMP"]
    fn dump_ops() {
        let path = std::env::var("RA_DUMP").expect("set RA_DUMP");
        let doc = Document::load(path).unwrap();
        let id = *doc.get_pages().values().next().unwrap();
        let content = doc.get_and_decode_page_content(id).unwrap();
        for op in content.operations.iter().take(80) {
            let args: Vec<String> = op.operands.iter().map(|o| format!("{o:?}").chars().take(40).collect()).collect();
            println!("{} {}", op.operator, args.join(" "));
        }
    }

    #[test]
    fn matrix_math_follows_pdf_row_vector_rules() {
        // Translate then scale: a point at (1,1) moves to (2,2), then doubles.
        let m = mul([1.0, 0.0, 0.0, 1.0, 1.0, 1.0], [2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
        assert_eq!(apply(&m, 1.0, 1.0), (4.0, 4.0));
    }

    #[test]
    fn font_names_become_word_font_families() {
        let f = face_from_name("ABCDEF+TimesNewRomanPS-BoldMT", 0);
        assert_eq!((f.family.as_str(), f.bold, f.italic), ("Times New Roman", true, false));
        let f = face_from_name("Calibri-Italic", 0);
        assert_eq!((f.family.as_str(), f.bold, f.italic), ("Calibri", false, true));
        assert_eq!(face_from_name("SegoeUI-Semibold", 0).family, "Segoe UI");
        assert_eq!(face_from_name("ArialMT", 0).family, "Arial");
        assert_eq!(face_from_name("Helvetica-Oblique", 0).italic, true);
        assert!(face_from_name("Whatever", 1 << 18).bold, "ForceBold flag");
    }

    fn run(x: f32, y: f32, w: f32, text: &str) -> Run {
        Run { x, y, w, size: 10.0, face: Face { family: "Arial".into(), bold: false, italic: false }, color: (0, 0, 0), text: text.into() }
    }

    #[test]
    fn runs_on_one_baseline_join_and_wide_gaps_split_columns() {
        let runs = vec![run(50.0, 100.0, 30.0, "Hello"), run(83.0, 100.0, 30.0, "world"), run(300.0, 100.0, 40.0, "Column two")];
        let lines = build_lines(&runs, &[]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans.iter().map(|s| s.text.as_str()).collect::<String>(), "Hello world");
        assert_eq!(lines[1].spans[0].text, "Column two");
    }

    #[test]
    fn table_rules_split_cells_and_gaps_keep_their_width() {
        // "Region" and "Sales" are only 6pt apart, but a vertical border runs between them.
        let runs = vec![run(60.0, 100.0, 30.0, "Region"), run(96.0, 100.0, 24.0, "Sales")];
        let rule = Shape { x: 93.0, y: 90.0, w: 0.5, h: 20.0, color: (0, 0, 0), order: 1 };
        assert_eq!(build_lines(&runs, &[rule]).len(), 2, "split at the border");
        assert_eq!(build_lines(&runs, &[]).len(), 1, "joined without it");
        // A 5pt gap at 10pt text is about two spaces wide.
        let spaced = build_lines(&[run(10.0, 50.0, 10.0, "a"), run(25.0, 50.0, 10.0, "b")], &[]);
        assert_eq!(spaced[0].spans[0].text, "a  b");
    }

    #[test]
    fn curves_and_diagonals_become_a_transparent_picture() {
        // A 40x40 pt circle-ish diamond with a hole (even-odd): corners outside, centre empty.
        let outer = SubPath { pts: vec![(0.0, 20.0), (20.0, 0.0), (40.0, 20.0), (20.0, 40.0), (0.0, 20.0)], rect: false };
        let hole = SubPath { pts: vec![(12.0, 20.0), (20.0, 12.0), (28.0, 20.0), (20.0, 28.0), (12.0, 20.0)], rect: false };
        let pic = rasterize(&[outer, hole], Some((255, 0, 0)), None, true).expect("raster");
        assert_eq!(pic.ext, "png");
        let img = image::load_from_memory(&pic.data).unwrap().to_rgba8();
        let scale = img.width() as f32 / pic.w;
        let at = |x: f32, y: f32| *img.get_pixel(((x - pic.x) * scale) as u32, ((y - pic.y) * scale) as u32);
        assert_eq!(at(20.0, 20.0)[3], 0, "the even-odd hole is transparent");
        assert_eq!(at(20.0, 6.0), image::Rgba([255, 0, 0, 255]), "filled ring is solid red");
        assert_eq!(at(2.0, 2.0)[3], 0, "outside the diamond is transparent");
        // Strokes draw an outline only.
        let line = SubPath { pts: vec![(0.0, 0.0), (30.0, 30.0)], rect: false };
        let s = rasterize(&[line], None, Some(((0, 0, 255), 4.0)), false).expect("stroke");
        let simg = image::load_from_memory(&s.data).unwrap().to_rgba8();
        let sc = simg.width() as f32 / s.w;
        let px = |x: f32, y: f32| *simg.get_pixel(((x - s.x) * sc) as u32, ((y - s.y) * sc) as u32);
        assert_eq!(px(15.0, 15.0), image::Rgba([0, 0, 255, 255]));
        assert_eq!(px(25.0, 5.0)[3], 0);
    }

    #[test]
    fn stacked_lines_form_a_paragraph_but_columns_stay_apart() {
        let mut runs = Vec::new();
        for (i, t) in ["first line", "second line", "third line"].iter().enumerate() {
            runs.push(run(50.0, 100.0 + i as f32 * 12.0, 60.0, t));
            runs.push(run(300.0, 100.0 + i as f32 * 12.0, 60.0, "right column"));
        }
        runs.push(run(50.0, 300.0, 50.0, "far below"));
        let blocks = build_blocks(build_lines(&runs, &[]));
        assert_eq!(blocks.len(), 3, "left paragraph, right paragraph, separate line");
        assert_eq!(blocks.iter().filter(|b| b.lines.len() == 3).count(), 2);
    }

    #[test]
    fn written_document_is_well_formed_xml_with_frames_pictures_and_sections() {
        let png = {
            let mut out = Vec::new();
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 0, 0, 255])))
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let page = |w: f32, h: f32| Page {
            w,
            h,
            runs: vec![run(72.0, 100.0, 60.0, "Title & <more>")],
            shapes: vec![Shape { x: 10.0, y: 10.0, w: 100.0, h: 2.0, color: (0, 0, 255), order: 2 }],
            pics: vec![Pic { x: 20.0, y: 200.0, w: 50.0, h: 40.0, ext: "png", data: png.clone(), order: 1 }],
        };
        let d = std::env::temp_dir().join(format!("radialactions_pdfword_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let out = d.join("t.docx");
        write_docx(&out, &[page(595.0, 842.0), page(842.0, 595.0)]).unwrap();

        let mut z = crate::ooxml::open_zip(&out).unwrap();
        let text = crate::ooxml::read_text(&mut z, "word/document.xml").unwrap();
        let parsed = roxmltree::Document::parse(&text).expect("document.xml is well-formed");
        let names: Vec<&str> = parsed.descendants().filter(|n| n.is_element()).map(|n| n.tag_name().name()).collect();
        assert_eq!(names.iter().filter(|n| **n == "sectPr").count(), 2, "one section per page");
        assert!(names.contains(&"framePr"));
        assert_eq!(names.iter().filter(|n| **n == "anchor").count(), 4, "a rule and a picture on each of two pages");
        assert!(text.contains("Title &amp; &lt;more&gt;"), "text is escaped");
        assert!(text.contains("w:w=\"11900\" w:h=\"16840\"") && text.contains("w:w=\"16840\" w:h=\"11900\""), "each page keeps its size");
        // The picture (order 1) is painted before the rule (order 2), so it gets the lower height.
        let h1 = text.find("relativeHeight=\"251658241\"").unwrap();
        let h2 = text.find("relativeHeight=\"251658242\"").unwrap();
        assert!(h1 < h2, "paint order is kept");
        for entry in ["word/media/image1.png", "word/media/solid_0000FF.png", "[Content_Types].xml", "word/_rels/document.xml.rels"] {
            assert!(z.by_name(entry).is_ok(), "{entry}");
        }
    }
}
