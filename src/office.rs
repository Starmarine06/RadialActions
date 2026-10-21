//! Slow conversions that borrow an installed program: Microsoft Office,
//! LibreOffice, or Edge. These take seconds, not milliseconds, so the wheel
//! shows a working toast while they run.

use crate::convert::{unique_sibling, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NO_WINDOW: u32 = 0x0800_0000;
const OFFICE_TIMEOUT: Duration = Duration::from_secs(120);
const EDGE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    Word,
    Excel,
    PowerPoint,
}

pub fn app_for(ext: &str) -> Option<App> {
    match ext {
        "doc" | "docx" | "docm" | "rtf" | "odt" => Some(App::Word),
        "xls" | "xlsx" | "xlsm" | "ods" => Some(App::Excel),
        "ppt" | "pptx" | "pptm" | "odp" => Some(App::PowerPoint),
        _ => None,
    }
}

/// Runs `cmd` to completion or kills it after `limit`, returning stderr on failure.
fn run_limited(mut cmd: Command, limit: Duration) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let mut child = cmd
        .creation_flags(NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut s, &mut err);
                }
                return if status.success() { Ok(()) } else { Err(first_line(&err)) };
            }
            None if start.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(80)),
        }
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Conversion failed");
    line.chars().take(120).collect()
}

/// What a conversion produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Out {
    Pdf,
    Docx,
    Doc,
    Rtf,
    Txt,
    Html,
    Odt,
    Xlsx,
    Xls,
    Csv,
    Ods,
    Pptx,
    Ppt,
    Odp,
    /// One PNG per slide, in a new folder.
    Slides,
}

impl Out {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Doc => "doc",
            Self::Rtf => "rtf",
            Self::Txt => "txt",
            Self::Html => "html",
            Self::Odt => "odt",
            Self::Xlsx => "xlsx",
            Self::Xls => "xls",
            Self::Csv => "csv",
            Self::Ods => "ods",
            Self::Pptx => "pptx",
            Self::Ppt => "ppt",
            Self::Odp => "odp",
            Self::Slides => "png",
        }
    }

    /// The program's own SaveAs format number for this output.
    fn code(self, app: App) -> i32 {
        match (app, self) {
            (App::Word, Self::Docx) => 16,
            (App::Word, Self::Doc) => 0,
            (App::Word, Self::Rtf) => 6,
            (App::Word, Self::Txt) => 7, // Unicode text; re-saved as UTF-8 afterwards
            (App::Word, Self::Html) => 10,
            (App::Word, Self::Odt) => 23,
            (App::Excel, Self::Xlsx) => 51,
            (App::Excel, Self::Xls) => 56,
            (App::Excel, Self::Csv) => 62, // CSV UTF-8
            (App::Excel, Self::Ods) => 60,
            (App::PowerPoint, Self::Pptx) => 24,
            (App::PowerPoint, Self::Ppt) => 1,
            (App::PowerPoint, Self::Odp) => 35,
            (App::PowerPoint, Self::Pdf) => 32,
            (App::PowerPoint, Self::Slides) => 18,
            _ => -1,
        }
    }
}

/// PowerShell that converts one document with an installed Office program.
/// Reads RA_IN, RA_OUT and RA_FMT (a format number, or "pdf" for Word and Excel).
///
/// If the user already has the program open the script reuses that session and
/// leaves it running; it only quits an instance it started itself, so nobody's
/// unsaved work is touched. Documents are opened read-only.
fn script(app: App) -> &'static str {
    match app {
        App::Word => {
            "$ErrorActionPreference='Stop'; $was=[bool](Get-Process WINWORD -ErrorAction SilentlyContinue); \
             $a=New-Object -ComObject Word.Application; \
             try { if(-not $was){$a.Visible=$false; $a.DisplayAlerts=0}; \
                   $d=$a.Documents.Open($env:RA_IN,$false,$true,$false); \
                   try { if($env:RA_FMT -eq 'pdf'){ $d.ExportAsFixedFormat($env:RA_OUT,17) } \
                         else { $d.SaveAs2($env:RA_OUT,[int]$env:RA_FMT) } } finally { $d.Close($false) } } \
             finally { if(-not $was){ $a.Quit() } }"
        }
        App::Excel => {
            "$ErrorActionPreference='Stop'; $was=[bool](Get-Process EXCEL -ErrorAction SilentlyContinue); \
             $a=New-Object -ComObject Excel.Application; \
             try { if(-not $was){$a.Visible=$false; $a.DisplayAlerts=$false}; \
                   $b=$a.Workbooks.Open($env:RA_IN,0,$true); \
                   try { if($env:RA_FMT -eq 'pdf'){ $b.ExportAsFixedFormat(0,$env:RA_OUT) } \
                         else { $b.SaveAs($env:RA_OUT,[int]$env:RA_FMT) } } finally { $b.Close($false) } } \
             finally { if(-not $was){ $a.Quit() } }"
        }
        App::PowerPoint => {
            "$ErrorActionPreference='Stop'; $was=[bool](Get-Process POWERPNT -ErrorAction SilentlyContinue); \
             $a=New-Object -ComObject PowerPoint.Application; \
             try { $p=$a.Presentations.Open($env:RA_IN,-1,0,0); \
                   try { $p.SaveAs($env:RA_OUT,[int]$env:RA_FMT) } finally { $p.Close() } } \
             finally { if(-not $was){ $a.Quit() } }"
        }
    }
}

fn soffice() -> Option<PathBuf> {
    ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|d| PathBuf::from(d).join("LibreOffice").join("program").join("soffice.exe"))
        .find(|p| p.exists())
}

fn edge() -> Option<PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|d| PathBuf::from(d).join("Microsoft").join("Edge").join("Application").join("msedge.exe"))
        .find(|p| p.exists())
}

/// Formats we can read ourselves, without opening Office.
pub fn is_native(ext: &str) -> bool {
    matches!(ext, "docx" | "docm" | "xlsx" | "xlsm" | "pptx" | "pptm")
}

/// PDF from any Office file: our own reader plus Edge when the format allows it
/// (nothing visible opens), otherwise the installed Office program.
pub fn to_pdf(src: &Path) -> Result<PathBuf> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if is_native(&ext) {
        native_pdf(src)
    } else {
        convert(src, Out::Pdf)
    }
}

fn render_html(src: &Path) -> Result<String> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "docx" | "docm" => crate::ooxml::docx_to_html(src),
        "xlsx" | "xlsm" => crate::xlsx::xlsx_to_html(src),
        "pptx" | "pptm" => crate::pptx::pptx_to_html(src),
        _ => Err("This old format needs Office".into()),
    }
}

/// Renders the file to HTML in-process and prints it to `dst` with headless Edge.
fn native_pdf_to(src: &Path, dst: &Path) -> Result<()> {
    let html = render_html(src)?;
    let tmp = std::env::temp_dir().join(format!("radialactions_native_{}_{}", std::process::id(), src.file_stem().and_then(|s| s.to_str()).unwrap_or("x").len()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let page = tmp.join("page.html");
    let result = std::fs::write(&page, html).map_err(|e| e.to_string()).and_then(|_| print_with_edge(&page, dst));
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

pub fn native_pdf(src: &Path) -> Result<PathBuf> {
    let dst = unique_sibling(src, "", "pdf");
    native_pdf_to(src, &dst)?;
    Ok(dst)
}

/// One PNG per slide, without PowerPoint: print to a scratch PDF, then render its pages.
pub fn native_slides(src: &Path) -> Result<(PathBuf, usize)> {
    let tmp = std::env::temp_dir().join(format!("radialactions_slides_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let pdf = tmp.join("slides.pdf");
    let result = native_pdf_to(src, &pdf).and_then(|_| {
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("slides");
        crate::pdf::to_images_named(&pdf, crate::pdf::Img::Png, src.parent().unwrap_or_else(|| Path::new(".")), &format!("{stem} slides"), "slide")
    });
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The same conversion through the installed Office program (opens it in the background).
pub fn to_pdf_via_office(src: &Path) -> Result<PathBuf> {
    convert(src, Out::Pdf)
}

/// Which program does the work: Word also opens PDFs (its "PDF reflow").
fn host_app(src: &Path) -> Result<App> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "pdf" {
        Ok(App::Word)
    } else {
        app_for(&ext).ok_or_else(|| "Not an Office file".to_string())
    }
}

pub fn convert(src: &Path, out: Out) -> Result<PathBuf> {
    let app = host_app(src)?;
    let fmt = if out == Out::Pdf && app != App::PowerPoint { "pdf".to_string() } else { out.code(app).to_string() };
    if fmt == "-1" {
        return Err(format!("{} can't save as {}", app_name(app), out.ext()));
    }
    let dst = if out == Out::Slides {
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("slides");
        let parent = src.parent().unwrap_or_else(|| Path::new("."));
        let mut d = parent.join(format!("{stem} slides"));
        let mut n = 1;
        while d.exists() {
            d = parent.join(format!("{stem} slides ({n})"));
            n += 1;
        }
        d
    } else {
        unique_sibling(src, "", out.ext())
    };

    let mut ps = Command::new("powershell.exe");
    ps.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script(app)])
        .env("RA_IN", src)
        .env("RA_OUT", &dst)
        .env("RA_FMT", &fmt);
    let office = run_limited(ps, OFFICE_TIMEOUT);

    if office.is_ok() && dst.exists() {
        if out == Out::Txt {
            crate::textconv::normalize_to_utf8(&dst)?;
        }
        return Ok(dst);
    }
    let partial = |p: &Path| {
        let _ = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    };
    partial(&dst); // never leave a partial result behind
    let from_pdf = src.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    match (soffice(), office) {
        (Some(exe), office_err) if out == Out::Pdf && !from_pdf => libreoffice(&exe, src, &dst)
            .map(|_| dst.clone())
            .map_err(|lo| format!("{} / {lo}", office_err.err().unwrap_or_default())),
        (_, Err(e)) => Err(format!("{e} (is {} installed?)", app_name(app))),
        (_, Ok(())) => Err(format!("{} produced no file", app_name(app))),
    }
}

fn app_name(a: App) -> &'static str {
    match a {
        App::Word => "Word",
        App::Excel => "Excel",
        App::PowerPoint => "PowerPoint",
    }
}

fn libreoffice(exe: &Path, src: &Path, dst: &Path) -> Result<()> {
    let out = std::env::temp_dir().join(format!("radialactions_lo_{}", std::process::id()));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(["--headless", "--convert-to", "pdf", "--outdir"]).arg(&out).arg(src);
    let ran = run_limited(cmd, OFFICE_TIMEOUT);
    let produced = out.join(src.file_stem().unwrap_or_default()).with_extension("pdf");
    let moved = ran.and_then(|_| std::fs::copy(&produced, dst).map(|_| ()).map_err(|e| e.to_string()));
    let _ = std::fs::remove_dir_all(&out);
    moved
}

fn file_url(p: &Path) -> String {
    let mut url = String::from("file:///");
    for ch in p.to_string_lossy().replace('\\', "/").chars() {
        match ch {
            ' ' => url.push_str("%20"),
            '#' => url.push_str("%23"),
            '%' => url.push_str("%25"),
            '?' => url.push_str("%3F"),
            c => url.push(c),
        }
    }
    url
}

pub fn html_to_pdf(src: &Path) -> Result<PathBuf> {
    let dst = unique_sibling(src, "", "pdf");
    print_with_edge(src, &dst)?;
    Ok(dst)
}

fn print_with_edge(html: &Path, dst: &Path) -> Result<()> {
    let exe = edge().ok_or("Microsoft Edge not found")?;
    // A throwaway profile, otherwise a running Edge swallows the command.
    let profile = std::env::temp_dir().join(format!("radialactions_edge_{}", std::process::id()));
    let mut cmd = Command::new(exe);
    cmd.args(["--headless=new", "--disable-gpu", "--no-pdf-header-footer", "--no-first-run"])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--print-to-pdf={}", dst.display()))
        .arg(file_url(html));
    let result = run_limited(cmd, EDGE_TIMEOUT);
    let _ = std::fs::remove_dir_all(&profile);
    match result {
        Ok(()) if dst.exists() => Ok(()),
        Ok(()) => Err("Edge produced no PDF".into()),
        Err(e) => {
            let _ = std::fs::remove_file(dst);
            Err(e)
        }
    }
}

/// Markdown rendered to HTML, printed through Edge. Falls back to the plain
/// monospaced text PDF if Edge isn't there.
pub fn markdown_to_pdf(src: &Path) -> Result<PathBuf> {
    if edge().is_none() {
        return crate::textconv::text_to_pdf(src);
    }
    let tmp = std::env::temp_dir().join(format!("radialactions_md_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let staged = tmp.join(src.file_name().unwrap_or_default());
    std::fs::copy(src, &staged).map_err(|e| e.to_string())?;
    let result = (|| {
        let html = crate::textconv::markdown_to_html(&staged)?;
        let dst = unique_sibling(src, "", "pdf");
        print_with_edge(&html, &dst)?;
        Ok(dst)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_modern_formats_are_read_natively() {
        for e in ["docx", "xlsx", "pptx"] {
            assert!(is_native(e), "{e}");
        }
        for e in ["doc", "xls", "ppt", "rtf", "odt", "pdf"] {
            assert!(!is_native(e), "{e}");
        }
    }

    #[test]
    fn maps_extensions_to_apps() {
        assert_eq!(app_for("docx"), Some(App::Word));
        assert_eq!(app_for("xlsx"), Some(App::Excel));
        assert_eq!(app_for("pptx"), Some(App::PowerPoint));
        assert_eq!(app_for("png"), None);
    }

    #[test]
    fn save_format_numbers_match_each_program() {
        assert_eq!(Out::Docx.code(App::Word), 16);
        assert_eq!(Out::Csv.code(App::Excel), 62);
        assert_eq!(Out::Slides.code(App::PowerPoint), 18);
        assert_eq!(Out::Pdf.code(App::PowerPoint), 32);
        assert_eq!(Out::Docx.code(App::Excel), -1); // nonsense combos are rejected
    }

    #[test]
    fn pdf_input_is_handled_by_word() {
        assert_eq!(host_app(Path::new("a.pdf")).unwrap(), App::Word);
        assert_eq!(host_app(Path::new("a.XLSX")).unwrap(), App::Excel);
        assert!(host_app(Path::new("a.png")).is_err());
    }

    #[test]
    fn file_urls_escape_awkward_characters() {
        let u = file_url(Path::new("C:\\My Docs\\a#1 100%.html"));
        assert_eq!(u, "file:///C:/My%20Docs/a%231%20100%25.html");
    }

    #[test]
    fn error_text_is_trimmed_to_one_short_line() {
        assert_eq!(first_line("\n\n  boom happened \nsecond"), "boom happened");
        assert_eq!(first_line(""), "Conversion failed");
        assert_eq!(first_line(&"x".repeat(500)).len(), 120);
    }

    #[test]
    fn a_hung_process_is_killed_at_the_limit() {
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-Command", "Start-Sleep 30"]);
        let t = Instant::now();
        let err = run_limited(cmd, Duration::from_millis(600)).unwrap_err();
        assert_eq!(err, "Timed out");
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_failing_process_reports_its_stderr() {
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-Command", "[Console]::Error.WriteLine('bad thing'); exit 3"]);
        assert_eq!(run_limited(cmd, Duration::from_secs(20)).unwrap_err(), "bad thing");
    }
}
