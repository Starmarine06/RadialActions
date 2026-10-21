//! .pptx reading without PowerPoint: every slide becomes an absolutely
//! positioned page of text boxes, shapes, pictures and tables.

use crate::convert::Result;
use crate::ooxml::{rid, attr, descendant, elements, esc, first, image_uri, local, num, open_zip, parse_theme, read_text, rels_of, xml, Theme, Zip};
use roxmltree::Node;
use std::collections::HashMap;
use std::path::Path;

const EMU_PER_PT: f32 = 12700.0;

#[derive(Clone, Copy, Default, Debug)]
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Position of a shape in points, from its a:xfrm.
fn frame_of(sp_pr: &Node) -> Option<Frame> {
    let xf = first(sp_pr, "xfrm")?;
    let off = first(&xf, "off")?;
    let ext = first(&xf, "ext")?;
    Some(Frame {
        x: num(attr(&off, "x"))? / EMU_PER_PT,
        y: num(attr(&off, "y"))? / EMU_PER_PT,
        w: num(attr(&ext, "cx"))? / EMU_PER_PT,
        h: num(attr(&ext, "cy"))? / EMU_PER_PT,
    })
}

fn placeholder(sp: &Node) -> Option<(String, String)> {
    let ph = descendant(&first(sp, "nvSpPr").or_else(|| first(sp, "nvPicPr"))?, "ph")?;
    Some((attr(&ph, "type").unwrap_or("body").to_string(), attr(&ph, "idx").unwrap_or("").to_string()))
}

fn scheme_alias(name: &str) -> &str {
    match name {
        "tx1" => "dk1",
        "bg1" => "lt1",
        "tx2" => "dk2",
        "bg2" => "lt2",
        other => other,
    }
}

fn fill_color(parent: &Node, theme: &Theme) -> Option<String> {
    let solid = first(parent, "solidFill")?;
    let c = elements(&solid).next()?;
    match local(&c) {
        "srgbClr" => attr(&c, "val").map(|v| format!("#{v}")),
        "schemeClr" => theme.colors.get(scheme_alias(attr(&c, "val")?)).cloned(),
        "sysClr" => attr(&c, "lastClr").map(|v| format!("#{v}")),
        _ => None,
    }
}

struct Deck<'a> {
    z: &'a mut Zip,
    theme: Theme,
    rels: HashMap<String, String>,
    /// Placeholder frames found on the slide's layout and master, by (type, idx).
    inherited: HashMap<(String, String), Frame>,
    /// Default text colour from the master's colour map (light on dark templates).
    default_color: Option<String>,
}

/// (text colour, background colour) the master maps tx1/bg1 to, e.g. dk1/lt1.
fn master_colors(z: &mut Zip, master: &str, theme: &Theme) -> (Option<String>, Option<String>) {
    let Some(text) = read_text(z, master) else { return (None, None) };
    let Ok(doc) = xml(&text) else { return (None, None) };
    let Some(map) = descendant(&doc.root(), "clrMap") else { return (None, None) };
    let pick = |name: &str| attr(&map, name).and_then(|v| theme.colors.get(v).cloned());
    (pick("tx1"), pick("bg1"))
}

fn collect_placeholders(z: &mut Zip, part: &str, into: &mut HashMap<(String, String), Frame>) {
    let Some(text) = read_text(z, part) else { return };
    let Ok(doc) = xml(&text) else { return };
    for sp in doc.descendants().filter(|n| n.is_element() && local(n) == "sp") {
        if let (Some((t, i)), Some(pr)) = (placeholder(&sp), first(&sp, "spPr")) {
            if let Some(f) = frame_of(&pr) {
                into.entry((t.clone(), i)).or_insert(f);
                into.entry((t, String::new())).or_insert(f);
            }
        }
    }
}

impl Deck<'_> {
    fn text_body(&self, body: &Node, default_pt: f32, color: &Option<String>) -> String {
        let mut html = String::new();
        for p in elements(body).filter(|n| local(n) == "p") {
            let ppr = first(&p, "pPr");
            let align = ppr.and_then(|n| attr(&n, "algn")).map(|a| match a {
                "ctr" => "center",
                "r" => "right",
                "just" => "justify",
                _ => "left",
            });
            let bullet = ppr.map(|n| first(&n, "buChar").is_some() || first(&n, "buAutoNum").is_some()).unwrap_or(false);
            let level: f32 = ppr.and_then(|n| num(attr(&n, "lvl"))).unwrap_or(0.0);
            let mut runs = String::new();
            for r in elements(&p) {
                match local(&r) {
                    "r" => {
                        let Some(t) = first(&r, "t").and_then(|t| t.text()) else { continue };
                        let rpr = first(&r, "rPr");
                        let mut css = String::new();
                        let size = rpr.and_then(|n| num(attr(&n, "sz"))).map(|s| s / 100.0).unwrap_or(default_pt);
                        css.push_str(&format!("font-size:{size}pt;"));
                        if let Some(n) = rpr {
                            if attr(&n, "b") == Some("1") {
                                css.push_str("font-weight:bold;");
                            }
                            if attr(&n, "i") == Some("1") {
                                css.push_str("font-style:italic;");
                            }
                            if matches!(attr(&n, "u"), Some(u) if u != "none") {
                                css.push_str("text-decoration:underline;");
                            }
                            if let Some(c) = fill_color(&n, &self.theme).or_else(|| color.clone()) {
                                css.push_str(&format!("color:{c};"));
                            }
                            if let Some(f) = first(&n, "latin").and_then(|l| attr(&l, "typeface")).filter(|f| !f.starts_with('+')) {
                                css.push_str(&format!("font-family:'{}',sans-serif;", f.replace('\'', "")));
                            }
                        } else if let Some(c) = color {
                            css.push_str(&format!("color:{c};"));
                        }
                        runs.push_str(&format!("<span style=\"{css}\">{}</span>", esc(t)));
                    }
                    "br" => runs.push_str("<br>"),
                    _ => {}
                }
            }
            if runs.is_empty() {
                runs.push_str(&format!("<span style=\"font-size:{default_pt}pt\">&nbsp;</span>"));
            }
            let mut css = String::from("margin:0 0 2pt;");
            if let Some(a) = align {
                css.push_str(&format!("text-align:{a};"));
            }
            if bullet {
                css.push_str(&format!("padding-left:{}pt;text-indent:-14pt;margin-left:14pt;", level * 18.0));
                runs = format!("<span style=\"display:inline-block;width:14pt;text-indent:0\">&#8226;</span>{runs}");
            }
            html.push_str(&format!("<p style=\"{css}\">{runs}</p>"));
        }
        html
    }

    fn shape(&mut self, sp: &Node, offset: (f32, f32), out: &mut String) {
        let pr = first(sp, "spPr");
        let ph = placeholder(sp);
        let frame = pr
            .and_then(|p| frame_of(&p))
            .or_else(|| ph.as_ref().and_then(|k| self.inherited.get(k).or_else(|| self.inherited.get(&(k.0.clone(), String::new())))).copied());
        let Some(f) = frame else { return };
        let mut css = format!("left:{}pt;top:{}pt;width:{}pt;height:{}pt;", f.x + offset.0, f.y + offset.1, f.w, f.h);
        if let Some(fill) = pr.and_then(|p| fill_color(&p, &self.theme)) {
            css.push_str(&format!("background:{fill};"));
        }
        if let Some(ln) = pr.and_then(|p| first(&p, "ln")) {
            if let Some(c) = fill_color(&ln, &self.theme) {
                let w = num(attr(&ln, "w")).map(|w| w / EMU_PER_PT).unwrap_or(1.0);
                css.push_str(&format!("border:{w}pt solid {c};"));
            }
        }
        match pr.and_then(|p| first(&p, "prstGeom")).and_then(|g| attr(&g, "prst")) {
            Some("ellipse") => css.push_str("border-radius:50%;"),
            Some("roundRect") => css.push_str("border-radius:12pt;"),
            _ => {}
        }
        let is_title = matches!(ph.as_ref().map(|k| k.0.as_str()), Some("title") | Some("ctrTitle"));
        let default_pt = if is_title { 40.0 } else { 18.0 };
        let body = first(sp, "txBody");
        if let Some(b) = &body {
            let anchor = first(b, "bodyPr").and_then(|n| attr(&n, "anchor"));
            css.push_str(match anchor {
                Some("ctr") => "display:flex;flex-direction:column;justify-content:center;",
                Some("b") => "display:flex;flex-direction:column;justify-content:flex-end;",
                _ => "",
            });
            if is_title {
                css.push_str("font-weight:bold;");
            }
        }
        let color = self.default_color.clone();
        let text = body.map(|b| self.text_body(&b, default_pt, &color)).unwrap_or_default();
        out.push_str(&format!("<div class=\"s\" style=\"{css}\">{text}</div>"));
    }

    fn picture(&mut self, pic: &Node, offset: (f32, f32), out: &mut String) {
        let Some(f) = first(pic, "spPr").and_then(|p| frame_of(&p)) else { return };
        let Some(id) = descendant(pic, "blip").and_then(|b| attr(&b, "embed")) else { return };
        let Some(path) = self.rels.get(id).cloned() else { return };
        let Some(uri) = image_uri(self.z, &path) else { return };
        out.push_str(&format!(
            "<img src=\"{uri}\" style=\"position:absolute;left:{}pt;top:{}pt;width:{}pt;height:{}pt;object-fit:fill\">",
            f.x + offset.0,
            f.y + offset.1,
            f.w,
            f.h
        ));
    }

    fn table(&mut self, frame_node: &Node, offset: (f32, f32), out: &mut String) {
        let Some(f) = first(frame_node, "xfrm").and_then(|x| {
            let off = first(&x, "off")?;
            let ext = first(&x, "ext")?;
            Some(Frame {
                x: num(attr(&off, "x"))? / EMU_PER_PT,
                y: num(attr(&off, "y"))? / EMU_PER_PT,
                w: num(attr(&ext, "cx"))? / EMU_PER_PT,
                h: num(attr(&ext, "cy"))? / EMU_PER_PT,
            })
        }) else {
            return;
        };
        let Some(tbl) = descendant(frame_node, "tbl") else { return };
        let mut html = format!("<table class=\"t\" style=\"left:{}pt;top:{}pt;width:{}pt\">", f.x + offset.0, f.y + offset.1, f.w);
        for tr in elements(&tbl).filter(|n| local(n) == "tr") {
            html.push_str("<tr>");
            for tc in elements(&tr).filter(|n| local(n) == "tc") {
                if attr(&tc, "hMerge") == Some("1") || attr(&tc, "vMerge") == Some("1") {
                    continue;
                }
                let span = attr(&tc, "gridSpan").unwrap_or("1");
                let content = first(&tc, "txBody").map(|b| self.text_body(&b, 14.0, &None)).unwrap_or_default();
                let fill = first(&tc, "tcPr").and_then(|p| fill_color(&p, &self.theme)).map(|c| format!("background:{c};")).unwrap_or_default();
                html.push_str(&format!("<td colspan=\"{span}\" style=\"{fill}\">{content}</td>"));
            }
            html.push_str("</tr>");
        }
        html.push_str("</table>");
        out.push_str(&html);
    }

    fn tree(&mut self, tree: &Node, offset: (f32, f32), out: &mut String) {
        for c in elements(tree) {
            match local(&c) {
                "sp" => self.shape(&c, offset, out),
                "pic" => self.picture(&c, offset, out),
                "graphicFrame" => self.table(&c, offset, out),
                "grpSp" => {
                    // Children are positioned in the group's own space; map it back to the slide.
                    let xf = first(&c, "grpSpPr").and_then(|p| first(&p, "xfrm"));
                    let shift = xf
                        .and_then(|x| {
                            let (off, ch) = (first(&x, "off")?, first(&x, "chOff")?);
                            Some((
                                (num(attr(&off, "x"))? - num(attr(&ch, "x"))?) / EMU_PER_PT,
                                (num(attr(&off, "y"))? - num(attr(&ch, "y"))?) / EMU_PER_PT,
                            ))
                        })
                        .unwrap_or((0.0, 0.0));
                    self.tree(&c, (offset.0 + shift.0, offset.1 + shift.1), out);
                }
                _ => {}
            }
        }
    }
}

pub fn pptx_to_html(path: &Path) -> Result<String> {
    let mut z = open_zip(path)?;
    let pres_text = read_text(&mut z, "ppt/presentation.xml").ok_or("Not a PowerPoint file (no ppt/presentation.xml)")?;
    let pres = xml(&pres_text)?;
    let size = descendant(&pres.root(), "sldSz");
    let w = size.and_then(|s| num(attr(&s, "cx"))).unwrap_or(9144000.0) / EMU_PER_PT;
    let h = size.and_then(|s| num(attr(&s, "cy"))).unwrap_or(6858000.0) / EMU_PER_PT;
    let pres_rels = rels_of(&mut z, "ppt/presentation.xml");
    let slide_parts: Vec<String> = descendant(&pres.root(), "sldIdLst")
        .map(|l| elements(&l).filter_map(|s| rid(&s)).filter_map(|id| pres_rels.get(id).cloned()).collect())
        .unwrap_or_default();
    if slide_parts.is_empty() {
        return Err("The presentation has no slides".into());
    }
    let theme = parse_theme(&mut z, "ppt/theme/theme1.xml");

    let mut slides_html = String::new();
    let count = slide_parts.len();
    for (i, part) in slide_parts.iter().enumerate() {
        let Some(text) = read_text(&mut z, part) else { continue };
        let doc = xml(&text)?;
        let rels = rels_of(&mut z, part);

        // Placeholder positions come from the layout, then the master.
        let mut inherited = HashMap::new();
        let (mut default_text, mut default_bg) = (None, None);
        if let Some(layout) = rels.values().find(|v| v.contains("slideLayouts/")).cloned() {
            collect_placeholders(&mut z, &layout, &mut inherited);
            let layout_rels = rels_of(&mut z, &layout);
            if let Some(master) = layout_rels.values().find(|v| v.contains("slideMasters/")).cloned() {
                collect_placeholders(&mut z, &master, &mut inherited);
                (default_text, default_bg) = master_colors(&mut z, &master, &theme);
            }
        }

        let bg = descendant(&doc.root(), "bg")
            .and_then(|b| first(&b, "bgPr"))
            .and_then(|p| fill_color(&p, &theme))
            .or(default_bg)
            .unwrap_or_else(|| "#ffffff".to_string());
        let mut deck = Deck { z: &mut z, theme: Theme { major: None, minor: None, colors: theme.colors.clone() }, rels, inherited, default_color: default_text };
        let mut shapes = String::new();
        if let Some(tree) = descendant(&doc.root(), "spTree") {
            deck.tree(&tree, (0.0, 0.0), &mut shapes);
        }
        let brk = if i + 1 < count { "break-after:page;" } else { "" };
        slides_html.push_str(&format!("<section style=\"width:{w}pt;height:{h}pt;background:{bg};{brk}\">{shapes}</section>"));
    }

    let font = theme.minor.clone().unwrap_or_else(|| "Calibri".into());
    Ok(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><style>@page{{size:{w}pt {h}pt;margin:0}}\
         html,body{{margin:0;padding:0}}body{{font-family:'{font}',sans-serif;color:#000}}\
         section{{position:relative;overflow:hidden;box-sizing:border-box}}\
         .s{{position:absolute;box-sizing:border-box;padding:3.6pt 7.2pt;overflow:hidden}}\
         .t{{position:absolute;border-collapse:collapse}}.t td{{border:0.75pt solid #888;padding:3pt 6pt;vertical-align:top}}\
         p{{white-space:pre-wrap}}</style></head><body>{slides_html}</body></html>"
    ))
}
