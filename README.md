# RadialActions

Hold **Shift** and rest the mouse over a file in Explorer or on the desktop. A wheel of actions for that file opens right there: convert, resize, merge, zip, move, rename and more. Pick a slice and the result lands next to the original.

No account, no ads, nothing uploaded. Your files never leave your PC.

## What it does

- **Images**: convert (JPG, PNG, WEBP, BMP, ICO, GIF, TIFF), resize, rotate and flip, crop, grayscale, to PDF. Camera rotation is respected.
- **PDF**: to Word (keeps the layout, reads scanned pages with Windows' built-in OCR), to text, to images, split, rotate, shrink, merge.
- **Word, Excel, PowerPoint**: to PDF without opening Office. Conversions that need Office sit in their own group.
- **Data and text**: JSON / YAML / TOML / CSV, Markdown to HTML, line endings, UTF-8, text to PDF.
- **Archives and more**: zip, unzip, tar.gz, SHA-256, copy path, reveal.
- **Several files**: merge PDFs, combine mixed files into one PDF, run an action on each, zip all.
- **Send**: move or copy to your folders, or rename a batch with a live preview.
- **Undo**: the result pop-up has an Undo button, and the tray menu keeps it until your next action. Created files go to the Recycle Bin, moved and renamed files go back.

## Make it yours

Click the tray icon for settings.

- **Look**: follow Windows (light or dark, your accent colour), or go neon.
- **Wheel**: reorder, pin or hide slices for each kind of file. The star slice is the action you use most.
- **Right-click options**: put another app's menu entry on the wheel, such as sending with Blip or resizing with PowerToys.
- **My actions**: add slices that start your own program with the file.
- **Trigger**: Shift, Ctrl, Alt or Ctrl+Shift, click or flick-and-release, adjustable delay. It stays quiet while a game or presentation is full screen.

## Install

Download `RadialActions-Setup-<version>.exe` from the Releases page and run it. It installs for the current user only, with no admin prompt. Windows 10 or 11, 64-bit. The settings windows need the [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/), which Windows 11 already has.

## Build

Rust stable (MSVC):

```
cargo build --release
cargo test --release
```

- Installer: install [Inno Setup 6](https://jrsoftware.org/isinfo.php), then `ISCC installer\radialactions.iss`.
- Store package: `.\packaging\build-msix.ps1` (needs the Windows SDK).
- Icons: `python assets/make_icons.py` (needs Pillow).

## Command line

```
radialactions --list <files>          show the wheel's actions for files
radialactions --run "<id>" <files>    run an action, e.g. "Convert/JPG"
radialactions --undo                  undo the last action
radialactions --autostart on|off|status
```

Settings live in `%APPDATA%\RadialActions\settings.toml`.

## Privacy and license

[Privacy policy](PRIVACY.md). MIT, see [LICENSE](LICENSE).
