//! Text recognition through Windows' built-in OCR engine (no models to ship).

use crate::convert::Result;

/// One recognised line, in the pixel coordinates of the image it came from.
#[derive(Clone, Debug)]
pub struct OcrLine {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub text: String,
}

pub struct Recognised {
    pub width: f32,
    pub height: f32,
    pub lines: Vec<OcrLine>,
}

/// Reads the text in an encoded image (PNG, JPEG...). Runs on its own thread:
/// Windows' async APIs never finish on a COM single-threaded one.
pub fn recognise(image: &[u8]) -> Result<Recognised> {
    std::thread::scope(|scope| scope.spawn(|| recognise_here(image)).join().map_err(|_| "Text recognition failed".to_string())?)
}

fn recognise_here(image: &[u8]) -> Result<Recognised> {
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    let err = |e: windows::core::Error| e.message().trim().to_string();
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| "No text recognition language is installed (Windows Settings > Time & language > Language)".to_string())?;

    let stream = InMemoryRandomAccessStream::new().map_err(err)?;
    let writer = DataWriter::CreateDataWriter(&stream).map_err(err)?;
    writer.WriteBytes(image).map_err(err)?;
    writer.StoreAsync().map_err(err)?.join().map_err(err)?;
    writer.FlushAsync().map_err(err)?.join().map_err(err)?;
    writer.DetachStream().map_err(err)?;
    stream.Seek(0).map_err(err)?;

    let decoder = BitmapDecoder::CreateAsync(&stream).map_err(err)?.join().map_err(err)?;
    let bitmap = decoder.GetSoftwareBitmapAsync().map_err(err)?.join().map_err(err)?;
    let (width, height) = (bitmap.PixelWidth().map_err(err)? as f32, bitmap.PixelHeight().map_err(err)? as f32);
    if width.max(height) as u32 > OcrEngine::MaxImageDimension().map_err(err)? {
        return Err("Image is too large to read".into());
    }
    let result = engine.RecognizeAsync(&bitmap).map_err(err)?.join().map_err(err)?;

    let mut lines = Vec::new();
    for line in result.Lines().map_err(err)? {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for word in line.Words().map_err(err)? {
            let r = word.BoundingRect().map_err(err)?;
            x0 = x0.min(r.X);
            y0 = y0.min(r.Y);
            x1 = x1.max(r.X + r.Width);
            y1 = y1.max(r.Y + r.Height);
        }
        let text = line.Text().map_err(err)?.to_string();
        if x1 > x0 && !text.trim().is_empty() {
            lines.push(OcrLine { x: x0, y: y0, w: x1 - x0, h: y1 - y0, text });
        }
    }
    Ok(Recognised { width, height, lines })
}
