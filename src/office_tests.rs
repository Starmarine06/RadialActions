//! Tests for the Office-free readers, using small hand-built documents.

use crate::{ooxml, pptx, xlsx};
use std::io::Write;
use std::path::{Path, PathBuf};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("radialactions_ooxml_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn make_zip(path: &Path, files: &[(&str, &str)]) {
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    for (name, body) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";

fn rich_docx(path: &Path) {
    let document = format!(
        "<w:document {W}><w:body>\
         <w:p><w:pPr><w:pStyle w:val=\"Title\"/></w:pPr><w:r><w:t>Quarterly Report</w:t></w:r></w:p>\
         <w:p><w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr><w:r><w:t>Overview</w:t></w:r></w:p>\
         <w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>\
           <w:r><w:t xml:space=\"preserve\">Plain </w:t></w:r>\
           <w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>\
           <w:r><w:rPr><w:i/><w:u w:val=\"single\"/></w:rPr><w:t xml:space=\"preserve\"> italic</w:t></w:r>\
           <w:r><w:rPr><w:color w:val=\"FF0000\"/><w:sz w:val=\"28\"/></w:rPr><w:t xml:space=\"preserve\"> red</w:t></w:r>\
           <w:hyperlink r:id=\"rId9\"><w:r><w:t>site</w:t></w:r></w:hyperlink></w:p>\
         <w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t>First item</w:t></w:r></w:p>\
         <w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr></w:pPr><w:r><w:t>Second item</w:t></w:r></w:p>\
         <w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"2\"/></w:numPr></w:pPr><w:r><w:t>Step one</w:t></w:r></w:p>\
         <w:p><w:pPr><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"2\"/></w:numPr></w:pPr><w:r><w:t>Step two</w:t></w:r></w:p>\
         <w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/></w:tblPr><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid>\
           <w:tr><w:tc><w:tcPr><w:shd w:fill=\"D9E2F3\"/></w:tcPr><w:p><w:r><w:t>Region</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Sales</w:t></w:r></w:p></w:tc></w:tr>\
           <w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>Merged across</w:t></w:r></w:p></w:tc></w:tr>\
         </w:tbl>\
         <w:p><w:r><w:t>Tom &amp; Jerry &lt;3</w:t></w:r></w:p>\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"720\" w:right=\"1080\" w:bottom=\"720\" w:left=\"1080\"/></w:sectPr>\
         </w:body></w:document>"
    );
    let styles = format!(
        "<w:styles {W}><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\"/><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault>\
         <w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
         <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:b/><w:sz w:val=\"56\"/></w:rPr></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:rPr><w:b/><w:color w:val=\"2F5496\"/><w:sz w:val=\"32\"/></w:rPr></w:style>\
         </w:styles>"
    );
    let numbering = format!(
        "<w:numbering {W}>\
         <w:abstractNum w:abstractNumId=\"0\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"x\"/><w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr></w:lvl></w:abstractNum>\
         <w:abstractNum w:abstractNumId=\"1\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/><w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr></w:lvl></w:abstractNum>\
         <w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num><w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>"
    );
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId9\" Type=\"hyperlink\" Target=\"https://example.com\" TargetMode=\"External\"/></Relationships>";
    make_zip(
        path,
        &[
            ("word/document.xml", &document),
            ("word/styles.xml", &styles),
            ("word/numbering.xml", &numbering),
            ("word/_rels/document.xml.rels", rels),
        ],
    );
}

#[test]
fn docx_keeps_styles_lists_tables_links_and_page_setup() {
    let d = dir("docx");
    let f = d.join("rich.docx");
    rich_docx(&f);
    let html = ooxml::docx_to_html(&f).unwrap();

    assert!(html.contains("<h1 "), "Title style becomes a heading");
    assert!(html.matches("<h1 ").count() == 2, "Title and Heading 1");
    assert!(html.contains("font-size:28pt"), "title size 56 half-points");
    assert!(html.contains("color:#2F5496"), "heading colour comes from its style");
    let bold_at = html.find(">bold</span>").expect("bold run");
    assert!(html[..bold_at].rsplit("<span").next().unwrap().contains("font-weight:bold"), "bold run is bold");
    assert!(html.contains("font-style:italic;text-decoration:underline;"));
    assert!(html.contains("color:#FF0000;"));
    assert!(html.contains("text-align:center"));
    assert!(html.contains("href=\"https://example.com\">"));
    // two bullets, then a numbered list that starts at 1.
    assert_eq!(html.matches("\u{2022}</span>").count(), 2);
    assert!(html.contains(">1.</span>") && html.contains(">2.</span>"));
    // table: grid borders, shading, a spanning cell.
    assert!(html.contains("border:0.5pt solid #000"));
    assert!(html.contains("background:#D9E2F3"));
    assert!(html.contains("colspan=\"2\""));
    // text is escaped, never injected as markup.
    assert!(html.contains("Tom &amp; Jerry &lt;3"));
    // Letter page, 0.5in top/bottom, 0.75in sides.
    assert!(html.contains("@page s0{size:612pt 792pt;margin:36pt 54pt 36pt 54pt}"), "{}", &html[..300]);
}

#[test]
fn docx_text_export_has_one_line_per_paragraph_and_tab_separated_cells() {
    let d = dir("docxtext");
    let f = d.join("rich.docx");
    rich_docx(&f);
    let text = ooxml::docx_to_text(&f).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Quarterly Report");
    assert!(lines.contains(&"Plain bold italic redsite"));
    assert!(lines.contains(&"Region\tSales"));
    assert!(lines.contains(&"Tom & Jerry <3"));
    assert!(text.contains("\r\n"), "Windows line endings");
}

#[test]
fn damaged_and_wrong_files_give_clear_errors() {
    let d = dir("bad");
    let junk = d.join("a.docx");
    std::fs::write(&junk, b"not a zip").unwrap();
    assert!(ooxml::docx_to_html(&junk).unwrap_err().contains("isn't a valid Office file"));
    let empty = d.join("b.docx");
    make_zip(&empty, &[("hello.txt", "x")]);
    assert!(ooxml::docx_to_html(&empty).unwrap_err().contains("no word/document.xml"));
    let broken = d.join("c.docx");
    make_zip(&broken, &[("word/document.xml", "<w:document><w:body>")]);
    assert!(ooxml::docx_to_html(&broken).unwrap_err().starts_with("Damaged document"));
}

fn sample_xlsx(path: &Path) {
    let ns = "xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    let workbook = format!(
        "<workbook {ns}><sheets><sheet name=\"Data\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Secret\" sheetId=\"2\" state=\"hidden\" r:id=\"rId2\"/></sheets></workbook>"
    );
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Target=\"worksheets/sheet2.xml\"/></Relationships>";
    let shared = format!("<sst {ns}><si><t>Name</t></si><si><t>Date</t></si><si><t>Pct</t></si><si><t>Money</t></si><si><t>Ann, \"A\"</t></si><si><r><t>Rich </t></r><r><t>text</t></r></si></sst>");
    let styles = format!(
        "<styleSheet {ns}><numFmts count=\"1\"><numFmt numFmtId=\"164\" formatCode=\"&quot;$&quot;#,##0.00\"/></numFmts>\
         <fonts><font/><font><b/><color rgb=\"FFFF0000\"/></font></fonts>\
         <fills><fill/><fill><patternFill patternType=\"solid\"><fgColor rgb=\"FFFFFF00\"/></patternFill></fill></fills>\
         <borders><border/><border><left style=\"thin\"/></border></borders>\
         <cellXfs><xf numFmtId=\"0\"/><xf numFmtId=\"14\"/><xf numFmtId=\"10\"/><xf numFmtId=\"164\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"1\" borderId=\"1\"/></cellXfs></styleSheet>"
    );
    let sheet = format!(
        "<worksheet {ns}><cols><col min=\"1\" max=\"1\" width=\"20\"/></cols><sheetData>\
         <row r=\"1\"><c r=\"A1\" t=\"s\" s=\"4\"><v>0</v></c><c r=\"B1\" t=\"s\"><v>1</v></c><c r=\"C1\" t=\"s\"><v>2</v></c><c r=\"D1\" t=\"s\"><v>3</v></c></row>\
         <row r=\"2\"><c r=\"A2\" t=\"s\"><v>4</v></c><c r=\"B2\" s=\"1\"><v>45000</v></c><c r=\"C2\" s=\"2\"><v>0.256</v></c><c r=\"D2\" s=\"3\"><v>1234.5</v></c></row>\
         <row r=\"3\"><c r=\"A3\" t=\"s\"><v>5</v></c><c r=\"C3\" t=\"b\"><v>1</v></c><c r=\"D3\"><v>0.30000000000000004</v></c></row>\
         <row r=\"4\" hidden=\"1\"><c r=\"A4\" t=\"inlineStr\"><is><t>hidden row</t></is></c></row>\
         </sheetData><mergeCells><mergeCell ref=\"A3:B3\"/></mergeCells></worksheet>"
    );
    make_zip(
        path,
        &[
            ("xl/workbook.xml", &workbook),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", &shared),
            ("xl/styles.xml", &styles),
            ("xl/worksheets/sheet1.xml", &sheet),
            ("xl/worksheets/sheet2.xml", &sheet),
        ],
    );
}

#[test]
fn xlsx_formats_values_like_excel_and_skips_hidden_things() {
    let d = dir("xlsx");
    let f = d.join("book.xlsx");
    sample_xlsx(&f);
    let html = xlsx::xlsx_to_html(&f).unwrap();
    assert!(html.contains(">2023-03-15<"), "date serial 45000");
    assert!(html.contains(">25.60%<"), "percent with two decimals");
    assert!(html.contains(">$1,234.50<"), "custom currency format with thousands");
    assert!(html.contains(">TRUE<"));
    assert!(html.contains(">0.3<"), "floating noise is trimmed like Excel");
    assert!(html.contains("Rich text"), "rich shared strings are joined");
    assert!(html.contains("Ann, &quot;A&quot;"));
    assert!(!html.contains("hidden row"), "hidden rows are skipped");
    assert!(!html.contains("Secret"), "hidden sheets are skipped");
    assert!(html.contains("colspan=\"2\""), "merged A3:B3");
    assert!(html.contains("background:#FFFF00") && html.contains("color:#FF0000") && html.contains("font-weight:bold") && html.contains("border-left:0.75pt solid"));
    assert!(html.contains("text-align:right"), "numbers align right");
    assert!(html.contains("white-space:nowrap"), "cells don't wrap unless the style asks for it");
    assert!(html.contains("width:"), "column widths carried over");
}

#[test]
fn xlsx_csv_uses_raw_numbers_and_quotes_properly() {
    let d = dir("csv");
    let f = d.join("book.xlsx");
    sample_xlsx(&f);
    let csv = xlsx::xlsx_to_csv(&f).unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "Name,Date,Pct,Money");
    assert_eq!(lines[1], "\"Ann, \"\"A\"\"\",2023-03-15,0.256,1234.5");
    assert!(lines[2].starts_with("Rich text,") && lines[2].contains("TRUE"));
}

fn sample_pptx(path: &Path) {
    let ns = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    let presentation = format!(
        "<p:presentation {ns}><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/><p:sldId id=\"257\" r:id=\"rId2\"/></p:sldIdLst><p:sldSz cx=\"12192000\" cy=\"6858000\"/></p:presentation>"
    );
    let pres_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Target=\"slides/slide1.xml\"/><Relationship Id=\"rId2\" Target=\"slides/slide2.xml\"/></Relationships>";
    let slide1 = format!(
        "<p:sld {ns}><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val=\"102030\"/></a:solidFill></p:bgPr></p:bg><p:spTree>\
         <p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"Title\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr><p:spPr/>\
           <p:txBody><a:bodyPr/><a:p><a:r><a:rPr lang=\"en-US\"/><a:t>Big Title</a:t></a:r></a:p></p:txBody></p:sp>\
         <p:sp><p:nvSpPr><p:cNvPr id=\"3\" name=\"Box\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>\
           <p:spPr><a:xfrm><a:off x=\"1270000\" y=\"2540000\"/><a:ext cx=\"5080000\" cy=\"1270000\"/></a:xfrm><a:prstGeom prst=\"ellipse\"/><a:solidFill><a:srgbClr val=\"FF8800\"/></a:solidFill></p:spPr>\
           <p:txBody><a:bodyPr anchor=\"ctr\"/><a:p><a:pPr algn=\"ctr\"/><a:r><a:rPr sz=\"3200\" b=\"1\"/><a:t>Hello &amp; welcome</a:t></a:r></a:p>\
           <a:p><a:pPr lvl=\"1\"><a:buChar char=\"x\"/></a:pPr><a:r><a:t>bullet</a:t></a:r></a:p></p:txBody></p:sp>\
         </p:spTree></p:cSld></p:sld>"
    );
    let slide1_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
        <Relationship Id=\"rId1\" Target=\"../slideLayouts/slideLayout1.xml\"/></Relationships>";
    let layout = format!(
        "<p:sldLayout {ns}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"T\"/><p:cNvSpPr/><p:nvPr><p:ph type=\"title\"/></p:nvPr></p:nvSpPr>\
         <p:spPr><a:xfrm><a:off x=\"914400\" y=\"457200\"/><a:ext cx=\"10363200\" cy=\"1143000\"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"
    );
    let slide2 = format!("<p:sld {ns}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"x\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"1270000\" cy=\"635000\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>");
    make_zip(
        path,
        &[
            ("ppt/presentation.xml", &presentation),
            ("ppt/_rels/presentation.xml.rels", pres_rels),
            ("ppt/slides/slide1.xml", &slide1),
            ("ppt/slides/_rels/slide1.xml.rels", slide1_rels),
            ("ppt/slides/slide2.xml", &slide2),
            ("ppt/slideLayouts/slideLayout1.xml", &layout),
        ],
    );
}

#[test]
fn pptx_places_shapes_inherits_layout_positions_and_paginates() {
    let d = dir("pptx");
    let f = d.join("deck.pptx");
    sample_pptx(&f);
    let html = pptx::pptx_to_html(&f).unwrap();
    // 12192000 x 6858000 EMU is 960 x 540 pt.
    assert!(html.contains("@page{size:960pt 540pt;margin:0}"), "{}", &html[..200]);
    assert_eq!(html.matches("<section").count(), 2);
    assert!(html.contains("background:#102030"), "slide background");
    // The title has no position of its own: it takes the layout's (72pt, 36pt).
    assert!(html.contains("left:72pt;top:36pt;width:816pt;height:90pt"), "layout placeholder position");
    assert!(html.contains("Big Title") && html.contains("font-size:40pt"), "title defaults to a large size");
    // The explicit box.
    assert!(html.contains("left:100pt;top:200pt;width:400pt;height:100pt"));
    assert!(html.contains("border-radius:50%") && html.contains("background:#FF8800"));
    assert!(html.contains("font-size:32pt") && html.contains("Hello &amp; welcome"));
    assert!(html.contains("justify-content:center") && html.contains("text-align:center"));
    assert!(html.contains("&#8226;"), "bullet marker");
    // All but the last slide break the page.
    assert_eq!(html.matches("break-after:page").count(), 1);
    assert!(html.contains("Second"));
}

#[test]
fn dark_template_text_is_light_via_the_master_colour_map() {
    let d = dir("dark");
    let f = d.join("dark.pptx");
    let ns = "xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";
    let pres = format!("<p:presentation {ns}><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/></p:sldIdLst><p:sldSz cx=\"9144000\" cy=\"6858000\"/></p:presentation>");
    let pres_rels = "<Relationships xmlns=\"x\"><Relationship Id=\"rId1\" Target=\"slides/slide1.xml\"/></Relationships>";
    let slide = format!("<p:sld {ns}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id=\"2\" name=\"t\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"1270000\" cy=\"635000\"/></a:xfrm></p:spPr><p:txBody><a:bodyPr/><a:p><a:r><a:t>Light text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>");
    let slide_rels = "<Relationships xmlns=\"x\"><Relationship Id=\"rId1\" Target=\"../slideLayouts/slideLayout1.xml\"/></Relationships>";
    let layout = format!("<p:sldLayout {ns}><p:cSld><p:spTree/></p:cSld></p:sldLayout>");
    let layout_rels = "<Relationships xmlns=\"x\"><Relationship Id=\"rId1\" Target=\"../slideMasters/slideMaster1.xml\"/></Relationships>";
    let master = format!("<p:sldMaster {ns}><p:cSld><p:spTree/></p:cSld><p:clrMap bg1=\"dk1\" tx1=\"lt1\" bg2=\"dk2\" tx2=\"lt2\"/></p:sldMaster>");
    let theme = "<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:themeElements><a:clrScheme name=\"t\">\
        <a:dk1><a:srgbClr val=\"111111\"/></a:dk1><a:lt1><a:srgbClr val=\"F0F0F0\"/></a:lt1><a:dk2><a:srgbClr val=\"222222\"/></a:dk2><a:lt2><a:srgbClr val=\"EEEEEE\"/></a:lt2>\
        </a:clrScheme></a:themeElements></a:theme>";
    make_zip(&f, &[
        ("ppt/presentation.xml", &pres), ("ppt/_rels/presentation.xml.rels", pres_rels),
        ("ppt/slides/slide1.xml", &slide), ("ppt/slides/_rels/slide1.xml.rels", slide_rels),
        ("ppt/slideLayouts/slideLayout1.xml", &layout), ("ppt/slideLayouts/_rels/slideLayout1.xml.rels", layout_rels),
        ("ppt/slideMasters/slideMaster1.xml", &master), ("ppt/theme/theme1.xml", theme),
    ]);
    let html = pptx::pptx_to_html(&f).unwrap();
    assert!(html.contains("background:#111111"), "background is the master's bg1 (dk1)");
    assert!(html.contains("color:#F0F0F0"), "text is the master's tx1 (lt1)");
}

#[test]
fn xlsx_and_pptx_reject_the_wrong_kind_of_file() {
    let d = dir("wrong");
    let docx = d.join("a.docx");
    rich_docx(&docx);
    assert!(xlsx::xlsx_to_html(&docx).unwrap_err().contains("Not an Excel workbook"));
    assert!(pptx::pptx_to_html(&docx).unwrap_err().contains("Not a PowerPoint file"));
}

/// Writes the three rich samples where the rest of the manual checks can pick them up.
#[test]
#[ignore = "writes samples to %TEMP%\\ra_samples for manual conversion checks"]
fn write_samples_for_manual_checks() {
    let d = std::env::temp_dir().join("ra_samples");
    std::fs::create_dir_all(&d).unwrap();
    rich_docx(&d.join("rich.docx"));
    sample_xlsx(&d.join("rich.xlsx"));
    sample_pptx(&d.join("rich.pptx"));
}

#[test]
#[ignore = "debug: prints the HTML for the .docx named by RA_DUMP"]
fn dump_docx_html() {
    let path = std::env::var("RA_DUMP").expect("set RA_DUMP");
    let html = ooxml::docx_to_html(Path::new(&path)).unwrap();
    // Long base64 payloads make the dump unreadable.
    let short = regex_lite_strip(&html);
    println!("{short}");
}

fn regex_lite_strip(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(i) = rest.find("base64,") {
        out.push_str(&rest[..i + 7]);
        let tail = &rest[i + 7..];
        let end = tail.find('"').unwrap_or(tail.len());
        out.push_str("...");
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}
