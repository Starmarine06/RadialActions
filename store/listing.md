# Microsoft Store listing

## Product name
RadialActions

## Short description
Hold Shift, rest on a file, pick a slice. Convert, resize, merge, zip, move and rename without opening a thing.

## Description
Hold Shift and rest your mouse on a file. A wheel of actions opens right there. Pick a slice and you're done, with the result sitting next to the original.

Turn a photo into a PNG or shrink it to half size. Make an editable Word file from a PDF, split it, merge a few, or squeeze one down. Save Word, Excel and PowerPoint files as PDF without opening Office. Zip a folder. Copy a path. Select twenty pictures and treat them as one.

Send moves or copies files to your favourite folders, and renames a whole batch with a preview of every new name. Changed your mind? Undo is right there in the pop-up.

You decide how it looks and behaves. Match Windows in light or dark with your accent colour, or switch to neon. Pin, hide and reorder slices for each kind of file. Add your own slices that open a file in whatever program you like. Pick Shift, Ctrl or Alt as the trigger. It keeps quiet while a game or presentation is full screen.

Everything happens on your PC. No account, no ads, nothing uploaded. Free and open source.

RadialActions lives in the tray next to the clock and works in File Explorer and on the desktop.

## What's new in this version
First release on the Microsoft Store. A new look with light, dark and neon themes, a settings window, your own actions, move, copy and bulk rename, Undo, and a fix for acting on the wrong file when file extensions are hidden.

## Features
- A wheel of file actions under your mouse
- Convert, resize, rotate and crop images
- PDF to Word, text and images, plus split, merge, rotate and shrink
- Word, Excel and PowerPoint to PDF
- JSON, YAML, TOML, CSV and Markdown conversions
- Zip, unzip and SHA-256
- Batch actions on many files
- Move, copy and bulk rename
- Undo for the last action
- Light, dark and neon themes
- Your own actions
- Works offline

## Search terms (up to 7)
file converter, radial menu, pdf to word, image converter, bulk rename, pie menu, pdf merge

## Category
Utilities and tools

## Price
Free

## Privacy policy URL
https://github.com/Starmarine06/RadialActions/blob/master/PRIVACY.md

## Website and support
https://github.com/Starmarine06/RadialActions
https://github.com/Starmarine06/RadialActions/issues

## Additional licence terms
MIT licence.

## Notes for certification
RadialActions is a tray utility. It has no main window.

1. Launch it. A welcome window opens. Click "Try it on sample files" to open a folder of samples in File Explorer.
2. Hold Shift and rest the mouse on "Sunset" for a moment. The wheel opens.
3. Click Convert, then JPG. A new file appears and the pop-up offers Undo.

To open settings later, click the tray icon (it may be under the ^ overflow arrow) or launch the app again.

The app uses a low-level mouse hook only to notice the trigger key plus mouse movement. It reads the item under the pointer in File Explorer through UI Automation and the shell. Nothing is recorded or sent anywhere.

## runFullTrust justification
RadialActions is a Win32 desktop app. It needs full trust to show a wheel over File Explorer (low-level mouse hook, shell and UI Automation access to the hovered file), to read and write the user's chosen files wherever they are, and to start local programs the user selects (Office, Edge for PDF printing, custom actions).

## Screenshots
`screenshots/1-wheel.png` to `8-rename.png`, 1920x1080. Trailer `trailer.mp4`, thumbnail `trailer-thumbnail.png`.
