# RadialActions

Hold **Shift** and rest the mouse over a file in Explorer or on the desktop. A radial wheel of actions for that file pops up: convert, resize, merge, zip and more. Pick a slice with a click (or flick and release Shift, from the tray menu).

Small and fast: about 5 MB on disk, under 1 MB working set while idle, no Office, no runtime, no background services.

## What it does

- **Images**: convert (JPG, PNG, WEBP, BMP, ICO, GIF, TIFF), resize, rotate and flip, crop, grayscale, to PDF.
- **PDF**: to Word (keeps the layout, reads scanned pages with Windows' built-in OCR), to text, to images, split, rotate, shrink, merge.
- **Word, Excel, PowerPoint**: to PDF without opening Office. Optional Office-based conversions sit in an "Office" group.
- **Data and text**: JSON / YAML / TOML / CSV, Markdown to HTML, line endings, UTF-8, text to PDF.
- **Archives and more**: zip, unzip, tar.gz, SHA-256, copy path, reveal.
- **Several files selected**: merge PDFs, combine mixed files into one PDF, convert each, zip all.
- The wheel learns what you use most and puts it first (the star slice).

## Install

Download `RadialActions-Setup-<version>.exe` from the Releases page and run it. It installs for the current user only, with no admin prompt.

## Build

Rust stable (MSVC), Windows 10/11:

```
cargo build --release
```

Installer: install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then `ISCC installer\radialactions.iss`.

## Command line

```
radialactions --list <files>          show the wheel's actions for files
radialactions --run "<id>" <files>    run an action, e.g. "Convert/JPG"
radialactions --autostart on|off|status
```

Settings live in `%APPDATA%\RadialActions\config.txt`.

## License

MIT. See [LICENSE](LICENSE).
