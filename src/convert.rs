use image::{DynamicImage, ImageFormat, ImageReader};
use std::io::Write;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImgTarget {
    Png,
    Jpg,
    Webp,
    Bmp,
    Ico,
    Gif,
    Tiff,
}

impl ImgTarget {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpg => "jpg",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
            Self::Ico => "ico",
            Self::Gif => "gif",
            Self::Tiff => "tiff",
        }
    }

    fn format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpg => ImageFormat::Jpeg,
            Self::Webp => ImageFormat::WebP,
            Self::Bmp => ImageFormat::Bmp,
            Self::Ico => ImageFormat::Ico,
            Self::Gif => ImageFormat::Gif,
            Self::Tiff => ImageFormat::Tiff,
        }
    }
}

pub fn is_image(path: &Path) -> bool {
    matches!(
        ext_lower(path).as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "ico" | "gif" | "tif" | "tiff"
    )
}

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Returns `<stem>.<ext>` next to `src`, or `<stem> (n).<ext>` if taken.
pub fn unique_sibling(src: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    let mut out = dir.join(format!("{stem}{suffix}.{ext}"));
    let mut n = 1;
    while out.exists() {
        out = dir.join(format!("{stem}{suffix} ({n}).{ext}"));
        n += 1;
    }
    out
}

fn load(src: &Path) -> Result<DynamicImage> {
    ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

fn save(img: &DynamicImage, dst: &Path, fmt: ImageFormat) -> Result<()> {
    // JPEG and BMP have no alpha channel, and ICO caps at 256 px.
    let prepared = match fmt {
        ImageFormat::Jpeg | ImageFormat::Bmp => DynamicImage::ImageRgb8(img.to_rgb8()),
        ImageFormat::Ico if img.width() > 256 || img.height() > 256 => {
            img.resize(256, 256, image::imageops::FilterType::Triangle)
        }
        _ => img.clone(),
    };
    prepared
        .save_with_format(dst, fmt)
        .map_err(|e| e.to_string())
}

pub fn convert_image(src: &Path, target: ImgTarget) -> Result<PathBuf> {
    let img = load(src)?;
    let dst = unique_sibling(src, "", target.ext());
    save(&img, &dst, target.format())?;
    Ok(dst)
}

/// Crop to `(x, y, w, h)`, clamped to the image bounds. Kept for the planned drag-to-crop tool.
#[allow(dead_code)]
pub fn crop_image(src: &Path, x: u32, y: u32, w: u32, h: u32) -> Result<PathBuf> {
    let img = load(src)?;
    let x = x.min(img.width().saturating_sub(1));
    let y = y.min(img.height().saturating_sub(1));
    let w = w.min(img.width() - x).max(1);
    let h = h.min(img.height() - y).max(1);
    let cropped = img.crop_imm(x, y, w, h);
    let fmt = ImageFormat::from_path(src).map_err(|e| e.to_string())?;
    let dst = unique_sibling(src, "_crop", &ext_lower(src));
    save(&cropped, &dst, fmt)?;
    Ok(dst)
}

/// Largest centred crop with aspect ratio `aw:ah`.
pub fn crop_aspect(src: &Path, aw: u32, ah: u32) -> Result<PathBuf> {
    let img = load(src)?;
    let (w, h) = (img.width(), img.height());
    // Compare aw/ah with w/h in integers: wider than target means trim width.
    let (cw, ch) = if (w as u64) * (ah as u64) > (h as u64) * (aw as u64) {
        (((h as u64) * (aw as u64) / (ah as u64)) as u32, h)
    } else {
        (w, ((w as u64) * (ah as u64) / (aw as u64)) as u32)
    };
    let (cw, ch) = (cw.max(1), ch.max(1));
    let cropped = img.crop_imm((w - cw) / 2, (h - ch) / 2, cw, ch);
    let fmt = ImageFormat::from_path(src).map_err(|e| e.to_string())?;
    let dst = unique_sibling(src, &format!("_{aw}x{ah}"), &ext_lower(src));
    save(&cropped, &dst, fmt)?;
    Ok(dst)
}

pub fn resize_image(src: &Path, percent: u32) -> Result<PathBuf> {
    let img = load(src)?;
    let w = (img.width() * percent / 100).max(1);
    let h = (img.height() * percent / 100).max(1);
    let out = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
    let fmt = ImageFormat::from_path(src).map_err(|e| e.to_string())?;
    let dst = unique_sibling(src, &format!("_{percent}pct"), &ext_lower(src));
    save(&out, &dst, fmt)?;
    Ok(dst)
}

pub fn rotate_image(src: &Path, clockwise_quarters: u8) -> Result<PathBuf> {
    let img = load(src)?;
    let out = match clockwise_quarters % 4 {
        1 => img.rotate90(),
        2 => img.rotate180(),
        3 => img.rotate270(),
        _ => img,
    };
    let fmt = ImageFormat::from_path(src).map_err(|e| e.to_string())?;
    let dst = unique_sibling(src, "_rot", &ext_lower(src));
    save(&out, &dst, fmt)?;
    Ok(dst)
}

pub fn flip_image(src: &Path, horizontal: bool) -> Result<PathBuf> {
    let img = load(src)?;
    let out = if horizontal { img.fliph() } else { img.flipv() };
    let fmt = ImageFormat::from_path(src).map_err(|e| e.to_string())?;
    let dst = unique_sibling(src, "_flip", &ext_lower(src));
    save(&out, &dst, fmt)?;
    Ok(dst)
}

pub fn grayscale_image(src: &Path) -> Result<PathBuf> {
    let img = load(src)?;
    let fmt = ImageFormat::from_path(src).map_err(|e| e.to_string())?;
    let dst = unique_sibling(src, "_gray", &ext_lower(src));
    save(&img.grayscale(), &dst, fmt)?;
    Ok(dst)
}

/// Image to a one-page PDF sized to the image. Written directly, with no
/// external tools: JPEGs are embedded as-is (DCT), everything else is
/// deflated RGB.
pub fn image_to_pdf(src: &Path) -> Result<PathBuf> {
    let (w, h, filter, data) = if matches!(ext_lower(src).as_str(), "jpg" | "jpeg") {
        let bytes = std::fs::read(src).map_err(|e| e.to_string())?;
        let img = load(src)?;
        let colors = img.color().channel_count();
        // CMYK/gray-with-alpha JPEGs are rare; re-encode anything but RGB or gray.
        if colors == 3 || colors == 1 {
            (img.width(), img.height(), "/DCTDecode", bytes)
        } else {
            raw_rgb(&img)?
        }
    } else {
        raw_rgb(&load(src)?)?
    };

    let gray = filter == "/DCTDecode" && is_gray_jpeg(src);
    let cs = if gray { "/DeviceGray" } else { "/DeviceRGB" };

    let dst = unique_sibling(src, "", "pdf");
    let content = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q");
    let mut buf: Vec<u8> = Vec::with_capacity(data.len() + 1024);
    let mut offsets = Vec::new();

    buf.extend_from_slice(b"%PDF-1.4
");
    offsets.push(buf.len());
    buf.extend_from_slice(b"1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
");
    offsets.push(buf.len());
    buf.extend_from_slice(b"2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
");
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}]              /Resources << /XObject << /Im0 4 0 R >> >> /Contents 5 0 R >>
endobj
"
        )
        .as_bytes(),
    );
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "4 0 obj
<< /Type /XObject /Subtype /Image /Width {w} /Height {h}              /ColorSpace {cs} /BitsPerComponent 8 /Filter {filter} /Length {} >>
stream
",
            data.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(&data);
    buf.extend_from_slice(b"
endstream
endobj
");
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "5 0 obj
<< /Length {} >>
stream
{content}
endstream
endobj
",
            content.len()
        )
        .as_bytes(),
    );

    let xref = buf.len();
    let mut table = format!("xref
0 {}
0000000000 65535 f 
", offsets.len() + 1);
    for o in &offsets {
        table.push_str(&format!("{o:010} 00000 n 
"));
    }
    table.push_str(&format!(
        "trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
",
        offsets.len() + 1
    ));
    buf.extend_from_slice(table.as_bytes());
    std::fs::write(&dst, &buf).map_err(|e| e.to_string())?;
    Ok(dst)
}

fn is_gray_jpeg(src: &Path) -> bool {
    load(src).map(|i| i.color().channel_count() == 1).unwrap_or(false)
}

fn raw_rgb(img: &DynamicImage) -> Result<(u32, u32, &'static str, Vec<u8>)> {
    use flate2::{write::ZlibEncoder, Compression};
    let rgb = img.to_rgb8();
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(rgb.as_raw()).map_err(|e| e.to_string())?;
    let data = enc.finish().map_err(|e| e.to_string())?;
    Ok((rgb.width(), rgb.height(), "/FlateDecode", data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        let img = RgbaImage::from_fn(64, 48, |x, y| Rgba([(x * 4) as u8, (y * 5) as u8, 128, 255]));
        DynamicImage::ImageRgba8(img).save(&p).unwrap();
        p
    }

    #[test]
    fn converts_between_formats() {
        let d = temp_dir("fmt");
        let png = sample(&d, "a.png");
        for t in [ImgTarget::Jpg, ImgTarget::Webp, ImgTarget::Bmp, ImgTarget::Gif, ImgTarget::Tiff, ImgTarget::Ico] {
            let out = convert_image(&png, t).unwrap();
            assert!(out.exists(), "{t:?}");
            assert_eq!(out.extension().unwrap(), t.ext());
        }
    }

    #[test]
    fn never_overwrites() {
        let d = temp_dir("uniq");
        let png = sample(&d, "a.png");
        let a = convert_image(&png, ImgTarget::Jpg).unwrap();
        let b = convert_image(&png, ImgTarget::Jpg).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn crop_resize_rotate() {
        let d = temp_dir("edit");
        let png = sample(&d, "a.png");
        let c = crop_image(&png, 10, 10, 20, 20).unwrap();
        assert_eq!(image::open(&c).unwrap().width(), 20);
        let r = resize_image(&png, 50).unwrap();
        assert_eq!(image::open(&r).unwrap().width(), 32);
        let o = rotate_image(&png, 1).unwrap();
        assert_eq!(image::open(&o).unwrap().width(), 48);
    }

    #[test]
    fn crop_aspect_is_centred() {
        let d = temp_dir("aspect");
        let png = sample(&d, "a.png"); // 64x48
        let sq = image::open(crop_aspect(&png, 1, 1).unwrap()).unwrap();
        assert_eq!((sq.width(), sq.height()), (48, 48));
        let wide = image::open(crop_aspect(&png, 16, 9).unwrap()).unwrap();
        assert_eq!((wide.width(), wide.height()), (64, 36));
        let tall = image::open(crop_aspect(&png, 9, 16).unwrap()).unwrap();
        assert_eq!((tall.width(), tall.height()), (27, 48));
    }

    #[test]
    fn crop_clamps_out_of_bounds() {
        let d = temp_dir("clamp");
        let png = sample(&d, "a.png");
        let c = crop_image(&png, 60, 40, 500, 500).unwrap();
        let img = image::open(&c).unwrap();
        assert_eq!((img.width(), img.height()), (4, 8));
    }

    #[test]
    fn pdf_is_well_formed_and_fast() {
        let d = temp_dir("pdf");
        let png = sample(&d, "a.png");
        let jpg = convert_image(&png, ImgTarget::Jpg).unwrap();
        for src in [&png, &jpg] {
            let t = std::time::Instant::now();
            let pdf = image_to_pdf(src).unwrap();
            assert!(t.elapsed().as_millis() < 100);
            let bytes = std::fs::read(&pdf).unwrap();
            assert!(bytes.starts_with(b"%PDF-1.4"));
            assert!(bytes.ends_with(b"%%EOF\n"));
            // startxref must point at the "xref" keyword
            let s = String::from_utf8_lossy(&bytes);
            let off: usize = s.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
            assert_eq!(&bytes[off..off + 4], b"xref");
        }
    }
}
