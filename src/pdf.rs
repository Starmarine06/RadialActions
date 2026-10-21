//! PDF work: page rendering through Windows' built-in PDF API, and page
//! surgery (split, rotate, merge, optimise) through lopdf.

use crate::convert::{unique_sibling, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn load(path: &Path) -> Result<Document> {
    let doc = Document::load(path).map_err(|e| format!("{}: {e}", file_name(path)))?;
    if doc.is_encrypted() {
        return Err(format!("{} is password protected", file_name(path)));
    }
    Ok(doc)
}

fn file_name(p: &Path) -> String {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
}

fn stem(p: &Path) -> String {
    p.file_stem().and_then(|n| n.to_str()).unwrap_or("file").to_string()
}

fn new_dir(parent: &Path, name: &str) -> Result<PathBuf> {
    let mut dir = parent.join(name);
    let mut n = 1;
    while dir.exists() {
        dir = parent.join(format!("{name} ({n})"));
        n += 1;
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn parent_of(p: &Path) -> &Path {
    p.parent().unwrap_or_else(|| Path::new("."))
}

// ---- rendering -------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Img {
    Png,
    Jpg,
}

impl Img {
    fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
        }
    }
}

/// Renders every page at `dpi` and hands each PNG's bytes to `each`.
fn render_pages(src: &Path, dpi: f32, each: impl FnMut(usize, Vec<u8>) -> Result<()>) -> Result<usize> {
    render_pages_of(src, dpi, None, each)
}

/// Renders the pages in `only` (all when `None`) at `dpi`, as PNGs.
pub fn render_some(src: &Path, dpi: f32, only: &[usize]) -> Result<Vec<(usize, Vec<u8>)>> {
    // Same STA caveat as `to_images_inner`: give the async API its own thread.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let mut out = Vec::new();
                render_pages_of(src, dpi, Some(only), |i, png| {
                    out.push((i, png));
                    Ok(())
                })
                .map(|_| out)
            })
            .join()
            .map_err(|_| "PDF renderer failed".to_string())?
    })
}

fn render_pages_of(src: &Path, dpi: f32, only: Option<&[usize]>, mut each: impl FnMut(usize, Vec<u8>) -> Result<()>) -> Result<usize> {
    use windows::core::HSTRING;
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    use windows::Storage::StorageFile;

    let err = |e: windows::core::Error| e.message().trim().to_string();
    // The storage API rejects the \\?\ prefix that canonicalize adds.
    let full = std::fs::canonicalize(src).map_err(|e| e.to_string())?;
    let full = full.to_string_lossy().trim_start_matches(r"\\?\").to_string();

    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(full)).map_err(err)?.join().map_err(err)?;
    let doc = PdfDocument::LoadFromFileAsync(&file)
        .map_err(err)?
        .join()
        .map_err(|_| format!("Can't open {} (password protected or damaged)", file_name(src)))?;
    let pages = doc.PageCount().map_err(err)?;
    for i in 0..pages {
        if only.is_some_and(|o| !o.contains(&(i as usize))) {
            continue;
        }
        let page = doc.GetPage(i).map_err(err)?;
        let width = (page.Size().map_err(err)?.Width * dpi / 96.0).clamp(64.0, 6000.0) as u32;
        let opts = PdfPageRenderOptions::new().map_err(err)?;
        opts.SetDestinationWidth(width).map_err(err)?;
        let stream = InMemoryRandomAccessStream::new().map_err(err)?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(err)?.join().map_err(err)?;
        let len = stream.Size().map_err(err)? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(err)?).map_err(err)?;
        reader.LoadAsync(len).map_err(err)?.join().map_err(err)?;
        let mut bytes = vec![0u8; len as usize];
        reader.ReadBytes(&mut bytes).map_err(err)?;
        each(i as usize, bytes)?;
    }
    Ok(pages as usize)
}

fn write_image(png: &[u8], dst: &Path, fmt: Img) -> Result<()> {
    match fmt {
        Img::Png => std::fs::write(dst, png).map_err(|e| e.to_string()),
        Img::Jpg => image::load_from_memory(png)
            .map_err(|e| e.to_string())?
            .to_rgb8()
            .save_with_format(dst, image::ImageFormat::Jpeg)
            .map_err(|e| e.to_string()),
    }
}

/// One page gives `<name>.png`; several go into a `<name> images` folder.
pub fn to_images(src: &Path, fmt: Img) -> Result<(PathBuf, usize)> {
    to_images_inner(src, fmt, None)
}

/// Always writes into a new folder `folder` under `parent`, as `<prefix>_<n>.<ext>`.
pub fn to_images_named(src: &Path, fmt: Img, parent: &Path, folder: &str, prefix: &str) -> Result<(PathBuf, usize)> {
    to_images_inner(src, fmt, Some((parent, folder, prefix)))
}

fn to_images_inner(src: &Path, fmt: Img, named: Option<(&Path, &str, &str)>) -> Result<(PathBuf, usize)> {
    // Windows' async PDF API never completes on a COM single-threaded thread
    // (the app's main thread is one), so rendering always gets its own thread.
    let staged: Vec<Vec<u8>> = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let mut pages = Vec::new();
                render_pages(src, 150.0, |_, png| {
                    pages.push(png);
                    Ok(())
                })
                .map(|_| pages)
            })
            .join()
            .map_err(|_| "PDF renderer failed".to_string())?
    })?;
    if staged.is_empty() {
        return Err("PDF has no pages".into());
    }
    if let Some((parent, folder, prefix)) = named {
        let dir = new_dir(parent, folder)?;
        for (i, png) in staged.iter().enumerate() {
            write_image(png, &dir.join(format!("{prefix}_{}.{}", i + 1, fmt.ext())), fmt)?;
        }
        return Ok((dir, staged.len()));
    }
    if staged.len() == 1 {
        let dst = unique_sibling(src, "", fmt.ext());
        write_image(&staged[0], &dst, fmt)?;
        return Ok((dst, 1));
    }
    let dir = new_dir(parent_of(src), &format!("{} images", stem(src)))?;
    for (i, png) in staged.iter().enumerate() {
        write_image(png, &dir.join(format!("{}_p{}.{}", stem(src), i + 1, fmt.ext())), fmt)?;
    }
    Ok((dir, staged.len()))
}

// ---- page surgery ------------------------------------------------------------

pub fn page_count(src: &Path) -> Result<usize> {
    Ok(load(src)?.get_pages().len())
}

/// Every page becomes its own PDF in a `<name> pages` folder.
pub fn split(src: &Path) -> Result<(PathBuf, usize)> {
    let total = load(src)?.get_pages().len();
    if total < 2 {
        return Err("Only one page, nothing to split".into());
    }
    let dir = new_dir(parent_of(src), &format!("{} pages", stem(src)))?;
    for keep in 1..=total as u32 {
        let mut doc = load(src)?;
        let drop: Vec<u32> = (1..=total as u32).filter(|p| *p != keep).collect();
        doc.delete_pages(&drop);
        doc.prune_objects();
        doc.save(dir.join(format!("{}_p{keep}.pdf", stem(src)))).map_err(|e| e.to_string())?;
    }
    Ok((dir, total))
}

/// A page's /Rotate, inherited from its ancestors if it has none of its own.
fn effective_rotation(doc: &Document, mut id: ObjectId) -> i64 {
    for _ in 0..32 {
        let Ok(dict) = doc.get_dictionary(id) else { return 0 };
        if let Ok(r) = dict.get(b"Rotate").and_then(|o| o.as_i64()) {
            return r;
        }
        match dict.get(b"Parent").and_then(|o| o.as_reference()) {
            Ok(parent) => id = parent,
            Err(_) => return 0,
        }
    }
    0
}

pub fn rotate(src: &Path, quarters: u8) -> Result<PathBuf> {
    let mut doc = load(src)?;
    let delta = (quarters as i64 % 4) * 90;
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for id in pages {
        let now = effective_rotation(&doc, id);
        let dict = doc.get_dictionary_mut(id).map_err(|e| e.to_string())?;
        dict.set("Rotate", Object::Integer((now + delta).rem_euclid(360)));
    }
    let dst = unique_sibling(src, "_rot", "pdf");
    doc.save(&dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// Re-compresses streams and drops unused objects. Only keeps the result if it's smaller.
pub fn optimize(src: &Path) -> Result<String> {
    let before = std::fs::metadata(src).map_err(|e| e.to_string())?.len();
    let mut doc = load(src)?;
    doc.prune_objects();
    doc.delete_zero_length_streams();
    doc.compress();
    let dst = unique_sibling(src, "_small", "pdf");
    doc.save(&dst).map_err(|e| e.to_string())?;
    let after = std::fs::metadata(&dst).map_err(|e| e.to_string())?.len();
    if after >= before {
        let _ = std::fs::remove_file(&dst);
        return Ok("Already compact, no savings".into());
    }
    Ok(format!("Saved {} ({} -> {})", file_name(&dst), human(before), human(after)))
}

pub fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

// ---- text ------------------------------------------------------------------------

/// Selectable text of every page, in order. Pages with no text come back empty.
pub fn text_pages(src: &Path) -> Result<Vec<String>> {
    let doc = load(src)?;
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let out: Vec<String> = pages
        .iter()
        .map(|n| doc.extract_text(&[*n]).unwrap_or_default().replace(char::from(13u8), "").trim().to_string())
        .collect();
    if out.iter().all(|t| t.is_empty()) {
        return Err("No selectable text (is it a scanned PDF?)".into());
    }
    Ok(out)
}

pub fn to_text(src: &Path) -> Result<PathBuf> {
    let pages = text_pages(src)?;
    let dst = unique_sibling(src, "", "txt");
    let text = pages.join("\n\n").replace("\r\n", "\n").replace('\n', "\r\n");
    std::fs::write(&dst, text).map_err(|e| e.to_string())?;
    Ok(dst)
}

// ---- merging -------------------------------------------------------------------

/// Page attributes a page may inherit from its parent /Pages nodes.
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Copies inherited attributes onto the page itself so it survives losing its old parent.
fn flatten_inheritance(doc: &mut Document, page: ObjectId) {
    let mut found: Vec<(&[u8], Object)> = Vec::new();
    let mut cur = page;
    for _ in 0..32 {
        let Ok(dict) = doc.get_dictionary(cur) else { break };
        for key in INHERITED {
            if cur != page && !found.iter().any(|(k, _)| *k == key) {
                if let Ok(v) = dict.get(key) {
                    found.push((key, v.clone()));
                }
            }
        }
        match dict.get(b"Parent").and_then(|o| o.as_reference()) {
            Ok(p) => cur = p,
            Err(_) => break,
        }
    }
    if let Ok(dict) = doc.get_dictionary_mut(page) {
        for (key, value) in found {
            if !dict.has(key) {
                dict.set(key, value);
            }
        }
    }
}

/// Joins PDFs, in the order given, into one new file beside the first.
pub fn merge(paths: &[PathBuf]) -> Result<(PathBuf, usize)> {
    let first = paths.first().ok_or("Select at least two PDFs")?;
    let dst = unique_sibling(first, "_merged", "pdf");
    let pages = merge_to(paths, &dst)?;
    Ok((dst, pages))
}

/// Merges into exactly `dst`, returning the page count. Nothing else is written.
fn merge_to(paths: &[PathBuf], dst: &Path) -> Result<usize> {
    if paths.len() < 2 {
        return Err("Select at least two PDFs".into());
    }
    let mut next_id = 1u32;
    let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();
    let mut page_ids: Vec<ObjectId> = Vec::new();

    for path in paths {
        let mut doc = load(path)?;
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;
        let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
        for &p in &pages {
            flatten_inheritance(&mut doc, p);
        }
        page_ids.extend(pages);
        for (id, obj) in doc.objects {
            // The old page trees and catalogs are replaced by the new ones below.
            let kind = obj.type_name().unwrap_or(b"");
            if kind != b"Catalog" && kind != b"Pages" {
                objects.insert(id, obj);
            }
        }
    }

    let pages_id = (next_id, 0);
    let catalog_id = (next_id + 1, 0);
    for id in &page_ids {
        if let Some(Object::Dictionary(d)) = objects.get_mut(id) {
            d.set("Parent", Object::Reference(pages_id));
        }
    }
    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Count", Object::Integer(page_ids.len() as i64));
    pages.set("Kids", Object::Array(page_ids.iter().map(|id| Object::Reference(*id)).collect()));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    objects.insert(pages_id, Object::Dictionary(pages));
    objects.insert(catalog_id, Object::Dictionary(catalog));

    let mut out = Document::with_version("1.5");
    out.objects = objects;
    out.max_id = catalog_id.0;
    out.trailer.set("Root", Object::Reference(catalog_id));
    out.prune_objects();
    out.renumber_objects();
    out.compress();

    out.save(dst).map_err(|e| e.to_string())?;
    Ok(page_ids.len())
}

/// Compares file names the way Explorer does: "page2" sorts before "page10".
pub fn natural_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        for c in s.to_lowercase().chars() {
            let digit = c.is_ascii_digit();
            match out.last_mut() {
                Some((d, run)) if *d == digit => run.push(c),
                _ => out.push((digit, c.to_string())),
            }
        }
        out
    }
    let (x, y) = (chunks(&file_name(a)), chunks(&file_name(b)));
    for (p, q) in x.iter().zip(y.iter()) {
        let ord = if p.0 && q.0 {
            let (n, m) = (p.1.trim_start_matches('0'), q.1.trim_start_matches('0'));
            n.len().cmp(&m.len()).then_with(|| n.cmp(m))
        } else {
            p.1.cmp(&q.1)
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    x.len().cmp(&y.len())
}

/// What `combine` knows how to turn into pages.
pub fn can_combine(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    ext == "pdf"
        || crate::convert::is_image(p)
        || crate::office::app_for(&ext).is_some()
        || matches!(ext.as_str(), "txt" | "log" | "md" | "markdown" | "html" | "htm" | "csv" | "json" | "xml")
}

/// Turns a mixed set (PDFs, images, text, Office files) into one PDF, one
/// source after another in Explorer order.
pub fn combine(paths: &[PathBuf]) -> Result<(PathBuf, usize)> {
    let mut sorted: Vec<PathBuf> = paths.to_vec();
    sorted.sort_by(|a, b| natural_cmp(a, b));
    if let Some(bad) = sorted.iter().find(|p| !can_combine(p)) {
        return Err(format!("Can't put {} in a PDF", file_name(bad)));
    }
    let tmp = std::env::temp_dir().join(format!("radialactions_combine_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;

    let result = (|| {
        let mut pdfs: Vec<PathBuf> = Vec::new();
        for (i, p) in sorted.iter().enumerate() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext == "pdf" {
                pdfs.push(p.clone());
                continue;
            }
            // Convert a private copy so nothing is written next to the originals.
            let slot = tmp.join(i.to_string());
            std::fs::create_dir_all(&slot).map_err(|e| e.to_string())?;
            let copy = slot.join(p.file_name().unwrap_or_default());
            std::fs::copy(p, &copy).map_err(|e| e.to_string())?;
            let pdf = if crate::convert::is_image(p) {
                crate::convert::image_to_pdf(&copy)
            } else if crate::office::app_for(&ext).is_some() {
                crate::office::to_pdf(&copy)
            } else if matches!(ext.as_str(), "md" | "markdown") {
                crate::office::markdown_to_pdf(&copy)
            } else if matches!(ext.as_str(), "html" | "htm") {
                crate::office::html_to_pdf(&copy)
            } else {
                crate::textconv::text_to_pdf(&copy)
            }
            .map_err(|e| format!("{}: {e}", file_name(p)))?;
            pdfs.push(pdf);
        }
        let dst = unique_sibling(&sorted[0], "_combined", "pdf");
        if pdfs.len() == 1 {
            std::fs::copy(&pdfs[0], &dst).map_err(|e| e.to_string())?;
            let pages = page_count(&dst)?;
            return Ok((dst, pages));
        }
        let pages = merge_to(&pdfs, &dst)?;
        Ok((dst, pages))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textconv::text_to_pdf;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_pdf_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A PDF with `lines` lines of text (60 per page), so `lines = 130` is 3 pages.
    fn sample(d: &Path, name: &str, lines: usize, tag: &str) -> PathBuf {
        let txt = d.join(format!("{name}.txt"));
        let body: String = (0..lines).map(|i| format!("{tag} line {i}\n")).collect();
        std::fs::write(&txt, body).unwrap();
        text_to_pdf(&txt).unwrap()
    }

    #[test]
    fn splits_into_one_file_per_page() {
        let d = dir("split");
        let pdf = sample(&d, "doc", 130, "A");
        let (out, n) = split(&pdf).unwrap();
        assert_eq!(n, 3);
        for i in 1..=3 {
            assert_eq!(page_count(&out.join(format!("doc_p{i}.pdf"))).unwrap(), 1);
        }
    }

    #[test]
    fn split_refuses_single_page() {
        let d = dir("split1");
        let pdf = sample(&d, "one", 5, "A");
        assert!(split(&pdf).unwrap_err().contains("nothing to split"));
    }

    #[test]
    fn rotation_accumulates_and_wraps() {
        let d = dir("rot");
        let pdf = sample(&d, "doc", 10, "A");
        let r1 = rotate(&pdf, 1).unwrap();
        let r2 = rotate(&r1, 3).unwrap(); // 90 then 270 = back to 0
        let doc1 = Document::load(&r1).unwrap();
        let id = *doc1.get_pages().values().next().unwrap();
        assert_eq!(effective_rotation(&doc1, id), 90);
        let doc2 = Document::load(&r2).unwrap();
        let id = *doc2.get_pages().values().next().unwrap();
        assert_eq!(effective_rotation(&doc2, id), 0);
    }

    #[test]
    fn merge_keeps_page_order_and_counts() {
        let d = dir("merge");
        let a = sample(&d, "a", 130, "AAA"); // 3 pages
        let b = sample(&d, "b", 70, "BBB"); // 2 pages
        let (out, pages) = merge(&[a, b]).unwrap();
        assert_eq!(pages, 5);
        let doc = Document::load(&out).unwrap();
        assert_eq!(doc.get_pages().len(), 5);
        // First page is from A, fourth from B.
        let text = |n: u32| doc.extract_text(&[n]).unwrap_or_default();
        assert!(text(1).contains("AAA"), "page1: {:?}", text(1));
        assert!(text(4).contains("BBB"), "page4: {:?}", text(4));
    }

    #[test]
    #[ignore = "probe: prints what lopdf can extract from the Office/Edge sample PDFs"]
    fn probe_text_extraction_on_real_pdfs() {
        let d = std::env::temp_dir().join("ra_office");
        for name in ["report", "sheet", "deck", "page", "notes"] {
            let p = d.join(format!("{name}.pdf"));
            match text_pages(&p) {
                Ok(t) => println!("{name}: {:?}", t[0].chars().take(120).collect::<String>()),
                Err(e) => println!("{name}: ERR {e}"),
            }
        }
    }

    #[test]
    fn natural_order_matches_explorer() {
        let mut v: Vec<PathBuf> = ["page10.pdf", "Page2.pdf", "page1.pdf", "page02b.pdf"].iter().map(PathBuf::from).collect();
        v.sort_by(|a, b| natural_cmp(a, b));
        let names: Vec<_> = v.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(names, ["page1.pdf", "Page2.pdf", "page02b.pdf", "page10.pdf"]);
    }

    #[test]
    fn combines_images_text_and_pdfs_in_order() {
        let d = dir("combine");
        let pdf = sample(&d, "b_report", 70, "REPORT"); // 2 pages
        let txt = d.join("c_notes.txt");
        std::fs::write(&txt, "NOTES line\n").unwrap();
        let img = d.join("a_photo.png");
        image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 30, 30, 255])).save(&img).unwrap();
        let (out, pages) = combine(&[txt, pdf, img]).unwrap(); // given out of order
        assert_eq!(pages, 4); // image, 2 report pages, notes
        assert_eq!(out.file_name().unwrap(), "a_photo_combined.pdf");
        let doc = Document::load(&out).unwrap();
        assert!(doc.extract_text(&[2]).unwrap_or_default().contains("REPORT"));
        assert!(doc.extract_text(&[4]).unwrap_or_default().contains("NOTES"));
        // sources untouched, no stray files beside them, and exactly one result
        assert!(!d.join("a_photo.pdf").exists() && !d.join("c_notes.pdf").exists());
        let combined: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains("_combined")).collect();
        assert_eq!(combined.len(), 1, "only one combined file is written");
        // a second run makes a second, separately named file
        let again = combine(&[d.join("c_notes.txt"), d.join("b_report.pdf"), d.join("a_photo.png")]).unwrap().0;
        assert_eq!(again.file_name().unwrap(), "a_photo_combined (1).pdf");
    }

    #[test]
    fn combine_rejects_unsupported_types_before_doing_work() {
        let d = dir("combinebad");
        let pdf = sample(&d, "a", 5, "A");
        let exe = d.join("tool.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        assert!(combine(&[pdf, exe]).unwrap_err().contains("tool.exe"));
    }

    #[test]
    fn merge_needs_two_and_reports_bad_files() {
        let d = dir("mergeerr");
        let a = sample(&d, "a", 5, "A");
        assert!(merge(&[a.clone()]).is_err());
        let junk = d.join("junk.pdf");
        std::fs::write(&junk, b"not a pdf").unwrap();
        assert!(merge(&[a, junk]).unwrap_err().contains("junk.pdf"));
    }

    #[test]
    fn optimize_only_keeps_a_smaller_file() {
        let d = dir("opt");
        let pdf = sample(&d, "doc", 130, "compressible text compressible text");
        let msg = optimize(&pdf).unwrap();
        assert!(msg.starts_with("Saved") || msg.starts_with("Already"), "{msg}");
        if msg.starts_with("Already") {
            assert!(!d.join("doc_small.pdf").exists());
        }
    }

    #[test]
    fn renders_pages_to_images() {
        let d = dir("render");
        let pdf = sample(&d, "doc", 130, "A");
        let (folder, n) = to_images(&pdf, Img::Png).unwrap();
        assert_eq!(n, 3);
        let first = std::fs::read(folder.join("doc_p1.png")).unwrap();
        assert_eq!(&first[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let (jpg_dir, _) = to_images(&pdf, Img::Jpg).unwrap();
        let img = image::open(jpg_dir.join("doc_p2.jpg")).unwrap();
        assert!(img.width() > 500 && img.height() > img.width()); // portrait page at 150 dpi
    }

    #[test]
    fn rendering_works_from_a_single_threaded_apartment() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let d = dir("sta");
        let pdf = sample(&d, "doc", 70, "A");
        let handle = std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            to_images(&pdf, Img::Png).map(|(_, n)| n)
        });
        assert_eq!(handle.join().unwrap().unwrap(), 2);
    }

    #[test]
    fn single_page_renders_to_one_file() {
        let d = dir("render1");
        let pdf = sample(&d, "one", 5, "A");
        let (file, n) = to_images(&pdf, Img::Png).unwrap();
        assert_eq!((n, file.file_name().unwrap().to_str().unwrap()), (1, "one.png"));
    }
}
