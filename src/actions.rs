//! What the wheel offers for a given file.
//!
//! Actions form a tree: a node is either a runnable leaf or a group that opens
//! a nested wheel. Adding a conversion is one `leaf(..)` entry below.

use crate::archive;
use crate::convert::{self, ImgTarget};
use crate::sys;
use crate::office::{self, Out};
use crate::ooxml;
use crate::pdfword;
use crate::xlsx;
use crate::pdf::{self, Img};
use crate::recent;
use crate::textconv::{self, Data};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type Runner = Arc<dyn Fn(&Path) -> Result<String, String> + Send + Sync>;

pub enum Kind {
    Run(Runner),
    Group(Vec<Node>),
}

pub struct Node {
    /// Short text drawn inside the slice.
    pub label: String,
    /// Longer description shown in the header while hovered.
    pub tip: String,
    pub kind: Kind,
    /// Path of labels from the root ("Convert/JPG"); filled in by `root_for`.
    pub id: String,
    /// Takes more than a moment (Office, ffmpeg), so the UI shows a working toast.
    pub slow: bool,
}

fn leaf(
    label: &str,
    tip: &str,
    f: impl Fn(&Path) -> Result<String, String> + Send + Sync + 'static,
) -> Node {
    Node { label: label.into(), tip: tip.into(), kind: Kind::Run(Arc::new(f)), id: String::new(), slow: false }
}

fn group(label: &str, tip: &str, children: Vec<Node>) -> Node {
    Node { label: label.into(), tip: tip.into(), kind: Kind::Group(children), id: String::new(), slow: false }
}

fn slow(mut n: Node) -> Node {
    n.slow = true;
    n
}

fn saved(out: PathBuf) -> String {
    format!("Saved {}", out.file_name().and_then(|n| n.to_str()).unwrap_or("file"))
}

fn ext_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// The wheel's actions for `path`, without any usage history applied.
pub fn root_for(path: &Path) -> Vec<Node> {
    let mut nodes = build_root(path);
    assign_ids(&mut nodes, "");
    nodes
}

/// Everything the overlay needs for the hovered file or the selection it belongs to:
/// the header title, the history key, and the slices (with a leading "your usual"
/// slice once the user has run something on this kind of item before).
pub fn wheel_for(paths: &[PathBuf]) -> (String, String, Vec<Node>) {
    let (title, key, mut nodes) = if paths.len() == 1 {
        (name_of(&paths[0]), type_key(&paths[0]), build_root(&paths[0]))
    } else {
        multi_root(paths)
    };
    assign_ids(&mut nodes, "");
    if let Some(id) = recent::top(&key) {
        if let Some((label, tip, runner, is_slow)) = find_leaf(&nodes, &id) {
            nodes.insert(
                0,
                Node {
                    label: format!("\u{2605} {label}"),
                    tip: format!("{tip} (your usual)"),
                    kind: Kind::Run(runner),
                    id,
                    slow: is_slow,
                },
            );
        }
    }
    (title, key, nodes)
}

/// Wheel for a multi-file selection, without usage history.
#[cfg(test)]
pub fn root_for_many(paths: &[PathBuf]) -> Vec<Node> {
    let mut nodes = multi_root(paths).2;
    assign_ids(&mut nodes, "");
    nodes
}

fn multi_root(paths: &[PathBuf]) -> (String, String, Vec<Node>) {
    let n = paths.len();
    let mut sorted = paths.to_vec();
    sorted.sort_by(|a, b| pdf::natural_cmp(a, b));
    let keys: Vec<String> = sorted.iter().map(|p| type_key(p)).collect();
    let same = keys.iter().all(|k| *k == keys[0]);
    let all_pdf = same && keys[0] == "pdf";
    let combinable = sorted.iter().all(|p| pdf::can_combine(p));
    let needs_app = sorted.iter().any(|p| {
        let e = ext_of(p);
        office::app_for(&e).is_some() || matches!(e.as_str(), "md" | "markdown" | "html" | "htm")
    });
    let shared = Arc::new(sorted.clone());

    let mut nodes: Vec<Node> = Vec::new();
    if all_pdf {
        let s = shared.clone();
        nodes.push(leaf("Merge", &format!("Merge {n} PDFs into one"), move |_| {
            pdf::merge(&s).map(|(out, pages)| format!("Merged {n} PDFs ({pages} pages) into {}", name_of(&out)))
        }));
    } else if combinable {
        let s = shared.clone();
        let node = leaf("To PDF", &format!("Combine {n} files into one PDF"), move |_| {
            pdf::combine(&s).map(|(out, pages)| format!("Combined into {} ({pages} pages)", name_of(&out)))
        });
        nodes.push(if needs_app { slow(node) } else { node });
    }
    if same {
        let mut each = build_root(&sorted[0]);
        each.retain(|node| node.label != "More");
        if !each.is_empty() {
            nodes.push(group("Each", "Run on every selected file", batch(each, shared.clone())));
        }
    }
    let s = shared.clone();
    nodes.push(leaf("Zip all", &format!("Compress {n} items into one .zip"), move |_| {
        archive::zip_many(&s).map(|(z, count)| format!("Zipped {count} files to {}", name_of(&z)))
    }));
    let s = shared.clone();
    nodes.push(leaf("Copy paths", "Copy every path, one per line", move |_| {
        let text: Vec<String> = s.iter().map(|p| p.display().to_string()).collect();
        sys::copy_text(&text.join("\r\n")).map(|_| format!("{} paths copied", s.len()))
    }));

    let key = if same { format!("many:{}", keys[0]) } else { "many:mixed".to_string() };
    (format!("{n} files selected"), key, nodes)
}

/// Makes every runnable leaf apply to all of `paths` instead of the one it was built for.
fn batch(nodes: Vec<Node>, paths: Arc<Vec<PathBuf>>) -> Vec<Node> {
    nodes
        .into_iter()
        .map(|mut node| {
            node.kind = match node.kind {
                Kind::Group(children) => Kind::Group(batch(children, paths.clone())),
                Kind::Run(runner) => {
                    let paths = paths.clone();
                    Kind::Run(Arc::new(move |_| run_each(&runner, &paths)))
                }
            };
            node
        })
        .collect()
}

fn run_each(runner: &Runner, paths: &[PathBuf]) -> Result<String, String> {
    let (mut done, mut failed, mut first_err) = (0, 0, None::<String>);
    for p in paths {
        match runner(p) {
            Ok(_) => done += 1,
            Err(e) => {
                failed += 1;
                first_err.get_or_insert(format!("{}: {e}", name_of(p)));
            }
        }
    }
    match first_err {
        None => Ok(format!("Done for {done} files")),
        Some(e) => Err(format!("{failed} of {} failed - {e}", paths.len())),
    }
}

/// Groups files by what they are, so history applies to every `.png`, not one file.
pub fn type_key(path: &Path) -> String {
    if path.is_dir() {
        "dir".into()
    } else if archive::is_tar_gz(path) {
        "tar.gz".into()
    } else {
        ext_of(path)
    }
}

fn assign_ids(nodes: &mut [Node], prefix: &str) {
    for n in nodes {
        n.id = if prefix.is_empty() { n.label.clone() } else { format!("{prefix}/{}", n.label) };
        if let Kind::Group(children) = &mut n.kind {
            let id = n.id.clone();
            assign_ids(children, &id);
        }
    }
}

type Found = (String, String, Runner, bool);

fn find_leaf(nodes: &[Node], id: &str) -> Option<Found> {
    for n in nodes {
        match &n.kind {
            Kind::Run(r) if n.id == id => return Some((n.label.clone(), n.tip.clone(), r.clone(), n.slow)),
            Kind::Group(children) if id.starts_with(&format!("{}/", n.id)) => return find_leaf(children, id),
            _ => {}
        }
    }
    None
}

/// Runs the action at `id` (e.g. `Convert/JPG`) on `path`. Used by `--run`.
pub fn run_by_id(path: &Path, id: &str) -> Result<String, String> {
    let nodes = root_for(path);
    let (_, _, runner, _) = find_leaf(&nodes, id).ok_or_else(|| format!("no action '{id}' for this file"))?;
    runner(path)
}

/// Runs the action at `id` on several files at once. Used by `--run`.
pub fn run_by_id_many(paths: &[PathBuf], id: &str) -> Result<String, String> {
    let nodes = wheel_for(paths).2;
    let (_, _, runner, _) = find_leaf(&nodes, id).ok_or_else(|| format!("no action '{id}' for this selection"))?;
    runner(&paths[0])
}

/// Prints the wheel as an indented outline. Used by `--list`.
pub fn outline(paths: &[PathBuf]) -> String {
    fn walk(nodes: &[Node], depth: usize, out: &mut String) {
        for n in nodes {
            out.push_str(&format!("{}{}{}\n", "  ".repeat(depth), n.label, if n.slow { "  (slow)" } else { "" }));
            if let Kind::Group(children) = &n.kind {
                walk(children, depth + 1, out);
            }
        }
    }
    let (title, key, nodes) = wheel_for(paths);
    let mut out = format!("{title}  [{key}]\n");
    walk(&nodes, 1, &mut out);
    out
}

fn build_root(path: &Path) -> Vec<Node> {
    let ext = ext_of(path);
    if path.is_dir() {
        // Zip is the likely intent for a folder, so it takes the top slice.
        let mut v = universal();
        v.rotate_right(1);
        return v;
    }
    if convert::is_image(path) {
        return image_root(path);
    }
    if archive::is_zip(path) {
        return with_more(vec![leaf("Unzip", "Extract to a new folder", |p| {
            archive::unzip(p).map(|(d, n)| format!("Extracted {n} files to {}", name_of(&d)))
        })]);
    }
    if archive::is_tar_gz(path) {
        return with_more(vec![leaf("Extract", "Extract to a new folder", |p| {
            archive::untar_gz(p).map(|d| format!("Extracted to {}", name_of(&d)))
        })]);
    }
    if let Some(from) = Data::from_ext(&ext) {
        return with_more(data_nodes(from));
    }
    if let Some(app) = office::app_for(&ext) {
        return with_more(office_nodes(app, &ext));
    }
    if ext == "pdf" {
        return with_more(pdf_nodes());
    }
    match ext.as_str() {
        "md" | "markdown" => with_more(vec![
            leaf("HTML", "Convert to HTML", |p| textconv::markdown_to_html(p).map(saved)),
            slow(leaf("PDF", "Convert to a formatted PDF", |p| office::markdown_to_pdf(p).map(saved))),
        ]),
        "html" | "htm" => with_more(vec![slow(leaf("PDF", "Print page to PDF", |p| {
            office::html_to_pdf(p).map(saved)
        }))]),
        "txt" | "log" | "ini" | "cfg" | "xml" | "css" | "js" | "rs" | "py" | "ts" => {
            with_more(vec![pdf_leaf(), text_fix_group()])
        }
        _ => universal(),
    }
}

fn office_leaf(label: &str, tip: &str, out: Out) -> Node {
    slow(leaf(label, tip, move |p| office::convert(p, out).map(saved)))
}

fn office_nodes(app: office::App, own_ext: &str) -> Vec<Node> {
    use office::App;
    let program = match app {
        App::Word => "Word",
        App::Excel => "Excel",
        App::PowerPoint => "PowerPoint",
    };
    let targets: Vec<(&str, Out)> = match app {
        App::Word => vec![("DOCX", Out::Docx), ("DOC", Out::Doc), ("RTF", Out::Rtf), ("TXT", Out::Txt), ("HTML", Out::Html), ("ODT", Out::Odt)],
        App::Excel => vec![("XLSX", Out::Xlsx), ("XLS", Out::Xls), ("CSV", Out::Csv), ("ODS", Out::Ods)],
        App::PowerPoint => vec![("PPTX", Out::Pptx), ("PPT", Out::Ppt), ("ODP", Out::Odp)],
    };
    let via_office_pdf = || slow(leaf("PDF", &format!("PDF using {program} (opens it in the background)"), |p| office::to_pdf_via_office(p).map(saved)));

    // Old binary formats can only be read by Office itself.
    if !office::is_native(own_ext) {
        let others: Vec<Node> = targets
            .into_iter()
            .filter(|(_, o)| o.ext() != own_ext)
            .map(|(label, o)| office_leaf(label, &format!("Convert to {label} (opens {program})"), o))
            .collect();
        let mut nodes = vec![via_office_pdf(), group("Convert", &format!("Convert with {program}"), others)];
        if app == App::PowerPoint {
            nodes.push(office_leaf("Slides", "Each slide as a PNG (opens PowerPoint)", Out::Slides));
        }
        return nodes;
    }

    // Modern formats are read directly: nothing opens, no Office needed.
    let mut extra: Vec<Node> = vec![via_office_pdf()];
    extra.extend(
        targets
            .iter()
            .filter(|(label, o)| o.ext() != own_ext && !matches!(*label, "TXT" | "HTML" | "CSV" | "XLSX" | "DOCX" | "PPTX"))
            .map(|(label, o)| office_leaf(label, &format!("Convert to {label} (opens {program})"), *o)),
    );
    if app == App::PowerPoint {
        extra.push(office_leaf("Slides", "Each slide as a PNG (opens PowerPoint)", Out::Slides));
    }
    let office_group = group("Office", &format!("Use {program} itself (opens it in the background)"), extra);

    let pdf = slow(leaf("PDF", "Convert to PDF", |p| office::native_pdf(p).map(saved)));
    match app {
        App::Word => vec![
            pdf,
            leaf("Text", "Extract the text to a .txt", |p| {
                textconv::save_sibling(p, "", "txt", &ooxml::docx_to_text(p)?).map(saved)
            }),
            leaf("HTML", "Convert to a web page", |p| {
                textconv::save_sibling(p, "", "html", &ooxml::docx_to_html(p)?).map(saved)
            }),
            office_group,
        ],
        App::Excel => vec![
            pdf,
            leaf("CSV", "First sheet as CSV", |p| {
                textconv::save_sibling(p, "", "csv", &xlsx::xlsx_to_csv(p)?).map(saved)
            }),
            office_group,
        ],
        App::PowerPoint => vec![
            pdf,
            slow(leaf("Slides", "Each slide as a PNG", |p| {
                office::native_slides(p).map(|(dir, n)| format!("Saved {n} slides to {}", name_of(&dir)))
            })),
            office_group,
        ],
    }
}

fn pdf_nodes() -> Vec<Node> {
    let image_leaf = |label: &str, fmt: Img| {
        leaf(label, &format!("Pages to {label} images"), move |p| {
            pdf::to_images(p, fmt).map(|(out, n)| {
                if n == 1 { saved(out) } else { format!("Saved {n} pages to {}", name_of(&out)) }
            })
        })
    };
    let rotate = vec![
        leaf("90\u{b0} right", "Rotate all pages 90\u{b0} clockwise", |p| pdf::rotate(p, 1).map(saved)),
        leaf("90\u{b0} left", "Rotate all pages 90\u{b0} counter-clockwise", |p| pdf::rotate(p, 3).map(saved)),
        leaf("180\u{b0}", "Rotate all pages 180\u{b0}", |p| pdf::rotate(p, 2).map(saved)),
    ];
    vec![
        leaf("Word", "Editable Word file that keeps the layout", |p| pdfword::convert(p).map(saved)),
        leaf("Text", "Extract the text to a .txt", |p| pdf::to_text(p).map(saved)),
        group("Image", "Pages to images", vec![image_leaf("PNG", Img::Png), image_leaf("JPG", Img::Jpg)]),
        leaf("Split", "Every page as its own PDF", |p| {
            pdf::split(p).map(|(dir, n)| format!("Split into {n} files in {}", name_of(&dir)))
        }),
        group("Rotate", "Rotate every page", rotate),
        leaf("Shrink", "Compress and clean up", |p| pdf::optimize(p)),
    ]
}

fn name_of(p: &Path) -> String {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("folder").to_string()
}

fn pdf_leaf() -> Node {
    leaf("PDF", "Convert text to PDF", |p| textconv::text_to_pdf(p).map(saved))
}

fn text_fix_group() -> Node {
    group(
        "Fix",
        "Line endings and encoding",
        vec![
            leaf("LF", "Use Unix line endings", |p| textconv::line_endings(p, false).map(saved)),
            leaf("CRLF", "Use Windows line endings", |p| textconv::line_endings(p, true).map(saved)),
            leaf("UTF-8", "Re-save as UTF-8", |p| textconv::to_utf8(p).map(saved)),
        ],
    )
}

fn data_nodes(from: Data) -> Vec<Node> {
    let targets: Vec<Node> = [Data::Json, Data::Yaml, Data::Toml, Data::Csv]
        .into_iter()
        .filter(|t| *t != from)
        .map(|t| {
            let up = t.ext().to_uppercase();
            leaf(&up, &format!("Convert to {up}"), move |p| textconv::convert_data(p, from, t).map(saved))
        })
        .collect();
    let mut nodes = vec![group("Convert", "Convert to another format", targets)];
    if from == Data::Json {
        nodes.push(leaf("Pretty", "Re-indent for reading", |p| textconv::json_pretty(p).map(saved)));
        nodes.push(leaf("Minify", "Remove all whitespace", |p| textconv::json_minify(p).map(saved)));
    }
    if from == Data::Csv {
        nodes.push(pdf_leaf());
    }
    nodes
}

fn with_more(mut nodes: Vec<Node>) -> Vec<Node> {
    nodes.push(group("More", "Copy path, hash, zip...", universal()));
    nodes
}

fn zip_leaf() -> Node {
    leaf("Zip", "Compress to a .zip", |p| {
        archive::zip_path(p).map(|(z, n)| format!("Zipped {n} file{} to {}", if n == 1 { "" } else { "s" }, name_of(&z)))
    })
}

fn universal() -> Vec<Node> {
    vec![
        leaf("Copy path", "Copy full path", |p| {
            sys::copy_text(&p.display().to_string()).map(|_| "Path copied".into())
        }),
        leaf("Copy name", "Copy file name", |p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            sys::copy_text(name).map(|_| "Name copied".into())
        }),
        leaf("Reveal", "Show in Explorer", |p| sys::reveal(p).map(|_| "Opened folder".into())),
        leaf("SHA-256", "Copy SHA-256 hash", |p| {
            let h = archive::sha256_hex(p)?;
            sys::copy_text(&h)?;
            Ok(format!("SHA-256 copied ({}...)", &h[..8]))
        }),
        zip_leaf(),
    ]
}

fn image_root(path: &Path) -> Vec<Node> {
    let own = ext_of(path);
    let same = |t: ImgTarget| t.ext() == own || (t == ImgTarget::Jpg && own == "jpeg") || (t == ImgTarget::Tiff && own == "tif");

    let convert_to: Vec<Node> = [
        ImgTarget::Png,
        ImgTarget::Jpg,
        ImgTarget::Webp,
        ImgTarget::Bmp,
        ImgTarget::Ico,
        ImgTarget::Gif,
        ImgTarget::Tiff,
    ]
    .into_iter()
    .filter(|t| !same(*t))
    .map(|t| {
        let up = t.ext().to_uppercase();
        leaf(&up, &format!("Convert to {up}"), move |p| convert::convert_image(p, t).map(saved))
    })
    .collect();

    let resize: Vec<Node> = [25u32, 50, 75, 150, 200]
        .into_iter()
        .map(|pct| {
            leaf(&format!("{pct}%"), &format!("Resize to {pct}%"), move |p| {
                convert::resize_image(p, pct).map(saved)
            })
        })
        .collect();

    let rotate = vec![
        leaf("90\u{b0} right", "Rotate 90\u{b0} clockwise", |p| convert::rotate_image(p, 1).map(saved)),
        leaf("90\u{b0} left", "Rotate 90\u{b0} counter-clockwise", |p| convert::rotate_image(p, 3).map(saved)),
        leaf("180\u{b0}", "Rotate 180\u{b0}", |p| convert::rotate_image(p, 2).map(saved)),
        leaf("Flip H", "Mirror left to right", |p| convert::flip_image(p, true).map(saved)),
        leaf("Flip V", "Mirror top to bottom", |p| convert::flip_image(p, false).map(saved)),
    ];

    let crop: Vec<Node> = [(1u32, 1u32), (16, 9), (4, 3), (3, 2), (9, 16)]
        .into_iter()
        .map(|(w, h)| {
            leaf(&format!("{w}:{h}"), &format!("Centre-crop to {w}:{h}"), move |p| {
                convert::crop_aspect(p, w, h).map(saved)
            })
        })
        .collect();

    vec![
        group("Convert", "Convert to another format", convert_to),
        group("Resize", "Scale the image", resize),
        group("Rotate", "Rotate or flip the image", rotate),
        group("Crop", "Crop to an aspect ratio", crop),
        leaf("PDF", "Convert to PDF", |p| convert::image_to_pdf(p).map(saved)),
        leaf("Gray", "Make grayscale", |p| convert::grayscale_image(p).map(saved)),
        group("More", "Copy path, reveal...", universal()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_leave_room_for_the_usual_slice() {
        let n = root_for(Path::new("C:\\x\\photo.png"));
        assert_eq!(n.len(), 7);
    }

    #[test]
    fn convert_skips_own_format() {
        let n = root_for(Path::new("C:\\x\\photo.jpeg"));
        let Kind::Group(g) = &n[0].kind else { panic!() };
        assert!(g.iter().all(|c| c.label != "JPG"));
        assert_eq!(g.len(), 6);
    }

    #[test]
    fn unknown_files_get_universal_actions() {
        let n = root_for(Path::new("C:\\x\\thing.xyz"));
        assert_eq!(n.len(), 5);
    }

    #[test]
    fn data_files_offer_the_other_formats() {
        let n = root_for(Path::new("C:\\x\\a.json"));
        let Kind::Group(g) = &n[0].kind else { panic!() };
        let labels: Vec<&str> = g.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["YAML", "TOML", "CSV"]);
        assert!(n.iter().any(|c| c.label == "Pretty") && n.iter().any(|c| c.label == "More"));
    }

    #[test]
    fn folder_leads_with_zip_without_duplicates() {
        let n = root_for(&std::env::temp_dir());
        assert_eq!(n[0].label, "Zip");
        assert_eq!(n.iter().filter(|c| c.label == "Zip").count(), 1);
    }

    #[test]
    fn ids_are_label_paths_and_findable() {
        let n = root_for(Path::new("photo.png"));
        assert!(find_leaf(&n, "Convert/JPG").is_some());
        assert!(find_leaf(&n, "Rotate/Flip H").is_some());
        assert!(find_leaf(&n, "Convert").is_none()); // groups aren't runnable
        assert!(find_leaf(&n, "Nope/JPG").is_none());
    }

    #[test]
    fn office_and_html_files_get_pdf() {
        for f in ["a.docx", "a.xlsx", "a.pptx", "a.html", "a.md"] {
            let n = root_for(Path::new(f));
            assert!(n.iter().any(|c| c.label == "PDF" && c.slow), "{f}");
        }
    }

    #[test]
    fn pdfs_get_real_pdf_actions() {
        let n = root_for(Path::new("a.pdf"));
        let labels: Vec<&str> = n.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Word", "Text", "Image", "Split", "Rotate", "Shrink", "More"]);
        assert!(!find_leaf(&n, "Word").unwrap().3, "PDF to Word needs no app and is quick");
        assert!(find_leaf(&n, "Image/PNG").is_some() && find_leaf(&n, "Rotate/180\u{b0}").is_some());
    }

    fn top_labels(f: &str) -> Vec<String> {
        root_for(Path::new(f)).iter().map(|c| c.label.clone()).collect()
    }

    fn group_labels(f: &str, group: &str) -> Vec<String> {
        let n = root_for(Path::new(f));
        let Kind::Group(g) = &n.iter().find(|c| c.label == group).unwrap().kind else { panic!() };
        g.iter().map(|c| c.label.clone()).collect()
    }

    #[test]
    fn modern_office_files_open_nothing_by_default() {
        assert_eq!(top_labels("a.docx"), ["PDF", "Text", "HTML", "Office", "More"]);
        assert_eq!(top_labels("a.xlsx"), ["PDF", "CSV", "Office", "More"]);
        assert_eq!(top_labels("a.pptx"), ["PDF", "Slides", "Office", "More"]);
        // The Office group is the only place that launches the real program.
        assert_eq!(group_labels("a.docx", "Office"), ["PDF", "DOC", "RTF", "ODT"]);
        assert_eq!(group_labels("a.xlsx", "Office"), ["PDF", "XLS", "ODS"]);
        assert_eq!(group_labels("a.pptx", "Office"), ["PDF", "PPT", "ODP", "Slides"]);
        let n = root_for(Path::new("a.docx"));
        assert!(find_leaf(&n, "PDF").unwrap().3 && find_leaf(&n, "Office/PDF").unwrap().3);
        assert!(!find_leaf(&n, "Text").unwrap().3, "text export is instant");
    }

    #[test]
    fn old_formats_can_only_use_office() {
        assert_eq!(top_labels("a.doc"), ["PDF", "Convert", "More"]);
        assert_eq!(group_labels("a.doc", "Convert"), ["DOCX", "RTF", "TXT", "HTML", "ODT"]);
        assert_eq!(group_labels("a.xls", "Convert"), ["XLSX", "CSV", "ODS"]);
        assert!(top_labels("a.ppt").contains(&"Slides".to_string()));
    }

    fn many(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn selected_pdfs_can_be_merged() {
        let n = root_for_many(&many(&["b.pdf", "a.pdf", "c.pdf"]));
        let labels: Vec<&str> = n.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Merge", "Each", "Zip all", "Copy paths"]);
    }

    #[test]
    fn mixed_documents_combine_into_one_pdf() {
        let n = root_for_many(&many(&["a.png", "b.docx", "c.pdf"]));
        let node = n.iter().find(|c| c.label == "To PDF").expect("combine offered");
        assert!(node.slow, "an Office file in the mix makes it slow");
        assert!(n.iter().all(|c| c.label != "Each"), "mixed types have no shared Each");
        let imgs = root_for_many(&many(&["a.png", "b.jpg"]));
        assert!(!imgs.iter().find(|c| c.label == "To PDF").unwrap().slow);
    }

    #[test]
    fn unsupported_mixes_only_get_zip_and_copy() {
        let n = root_for_many(&many(&["a.exe", "b.png"]));
        let labels: Vec<&str> = n.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Zip all", "Copy paths"]);
    }

    #[test]
    fn each_runs_the_action_on_every_file() {
        let d = std::env::temp_dir().join(format!("radialactions_each_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let files: Vec<PathBuf> = ["one.png", "two.png", "three.png"]
            .iter()
            .map(|n| {
                let p = d.join(n);
                image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255])).save(&p).unwrap();
                p
            })
            .collect();
        let nodes = root_for_many(&files);
        let (_, _, runner, _) = find_leaf(&nodes, "Each/Convert/JPG").unwrap();
        assert_eq!(runner(&files[0]).unwrap(), "Done for 3 files");
        for n in ["one.jpg", "two.jpg", "three.jpg"] {
            assert!(d.join(n).exists(), "{n}");
        }
        // one bad file is reported without hiding the others
        std::fs::write(d.join("bad.png"), b"not an image").unwrap();
        let mut with_bad = files.clone();
        with_bad.push(d.join("bad.png"));
        let nodes = root_for_many(&with_bad);
        let (_, _, runner, _) = find_leaf(&nodes, "Each/Resize/50%").unwrap();
        let err = runner(&with_bad[0]).unwrap_err();
        assert!(err.starts_with("1 of 4 failed") && err.contains("bad.png"), "{err}");
        assert!(d.join("one_50pct.png").exists() && d.join("three_50pct.png").exists());
    }

    #[test]
    fn archives_get_extract() {
        assert_eq!(root_for(Path::new("C:\\x\\a.zip"))[0].label, "Unzip");
        assert_eq!(root_for(Path::new("C:\\x\\a.tar.gz"))[0].label, "Extract");
    }

    #[test]
    fn no_wheel_exceeds_eight_slices() {
        for name in ["a.png", "a.json", "a.csv", "a.md", "a.txt", "a.zip", "a.xyz", "a.yaml"] {
            assert!(root_for(Path::new(name)).len() <= 8, "{name}");
        }
    }
}
