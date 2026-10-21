//! ZIP and tar.gz handling, plus file hashing.

use crate::convert::Result;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

pub fn is_zip(p: &Path) -> bool {
    has_ext(p, "zip")
}

pub fn is_tar_gz(p: &Path) -> bool {
    let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    n.ends_with(".tar.gz") || n.ends_with(".tgz")
}

fn has_ext(p: &Path, ext: &str) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn files_under(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        // Links are skipped so a loop can't make the walk run forever.
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            files_under(&path, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Zips a file or a whole folder next to it. Returns the archive and entry count.
pub fn zip_path(src: &Path) -> Result<(PathBuf, usize)> {
    let name = src.file_name().and_then(|n| n.to_str()).ok_or("bad name")?.to_string();
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    // Folders keep their full name ("v1.2"); files drop their extension.
    let stem = if src.is_dir() { name.clone() } else { src.file_stem().and_then(|s| s.to_str()).unwrap_or(&name).to_string() };
    let mut dst = parent.join(format!("{stem}.zip"));
    let mut n = 1;
    while dst.exists() {
        dst = parent.join(format!("{stem} ({n}).zip"));
        n += 1;
    }

    let mut files = Vec::new();
    if src.is_dir() {
        files_under(src, &mut files).map_err(|e| e.to_string())?;
    } else {
        files.push(src.to_path_buf());
    }

    let result = (|| -> Result<usize> {
        let mut zip = zip::ZipWriter::new(File::create(&dst).map_err(|e| e.to_string())?);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let base = if src.is_dir() { src } else { parent };
        let mut buf = Vec::new();
        for f in &files {
            let rel = f.strip_prefix(base.parent().unwrap_or(base)).unwrap_or(f);
            let entry = if src.is_dir() {
                rel.to_string_lossy().replace('\\', "/")
            } else {
                name.clone()
            };
            zip.start_file(entry, opts).map_err(|e| e.to_string())?;
            buf.clear();
            File::open(f).and_then(|mut h| h.read_to_end(&mut buf)).map_err(|e| format!("{}: {e}", f.display()))?;
            zip.write_all(&buf).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
        Ok(files.len())
    })();
    match result {
        Ok(n) => Ok((dst, n)),
        Err(e) => {
            let _ = std::fs::remove_file(&dst); // don't leave a half-written archive
            Err(e)
        }
    }
}

/// Zips several files and folders into one archive beside the first item.
pub fn zip_many(paths: &[PathBuf]) -> Result<(PathBuf, usize)> {
    let first = paths.first().ok_or("Nothing selected")?;
    let parent = first.parent().unwrap_or_else(|| Path::new("."));
    let stem = first.file_stem().and_then(|s| s.to_str()).unwrap_or("files");
    let label = if paths.len() > 1 { format!("{stem} + {} more", paths.len() - 1) } else { stem.to_string() };
    let mut dst = parent.join(format!("{label}.zip"));
    let mut n = 1;
    while dst.exists() {
        dst = parent.join(format!("{label} ({n}).zip"));
        n += 1;
    }

    let result = (|| -> Result<usize> {
        let mut zip = zip::ZipWriter::new(File::create(&dst).map_err(|e| e.to_string())?);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut count = 0;
        let mut taken: Vec<String> = Vec::new();
        for item in paths {
            let mut files = Vec::new();
            if item.is_dir() {
                files_under(item, &mut files).map_err(|e| e.to_string())?;
            } else {
                files.push(item.clone());
            }
            for f in files {
                let rel = if item.is_dir() {
                    f.strip_prefix(item.parent().unwrap_or(item)).unwrap_or(&f).to_string_lossy().replace('\\', "/")
                } else {
                    f.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
                };
                // Two selected files can share a name; keep both.
                let mut name = rel.clone();
                let mut k = 1;
                while taken.contains(&name) {
                    name = format!("{k}_{rel}");
                    k += 1;
                }
                taken.push(name.clone());
                zip.start_file(name, opts).map_err(|e| e.to_string())?;
                let mut buf = Vec::new();
                File::open(&f).and_then(|mut h| h.read_to_end(&mut buf)).map_err(|e| format!("{}: {e}", f.display()))?;
                zip.write_all(&buf).map_err(|e| e.to_string())?;
                count += 1;
            }
        }
        zip.finish().map_err(|e| e.to_string())?;
        Ok(count)
    })();
    match result {
        Ok(n) => Ok((dst, n)),
        Err(e) => {
            let _ = std::fs::remove_file(&dst);
            Err(e)
        }
    }
}

fn extract_dir_for(src: &Path) -> PathBuf {
    let mut stem = src.file_name().and_then(|n| n.to_str()).unwrap_or("archive").to_string();
    for suffix in [".tar.gz", ".tgz", ".zip"] {
        if stem.to_ascii_lowercase().ends_with(suffix) {
            stem.truncate(stem.len() - suffix.len());
        }
    }
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let mut dir = parent.join(&stem);
    let mut n = 1;
    while dir.exists() {
        dir = parent.join(format!("{stem} ({n})"));
        n += 1;
    }
    dir
}

/// Extracts into a new sibling folder; entries that would escape it are skipped.
pub fn unzip(src: &Path) -> Result<(PathBuf, usize)> {
    let dir = extract_dir_for(src);
    let mut archive = zip::ZipArchive::new(File::open(src).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut count = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let out = dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut f = File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        count += 1;
    }
    Ok((dir, count))
}

pub fn untar_gz(src: &Path) -> Result<PathBuf> {
    let dir = extract_dir_for(src);
    let gz = flate2::read::GzDecoder::new(File::open(src).map_err(|e| e.to_string())?);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // `unpack` refuses entries that would land outside `dir`.
    tar::Archive::new(gz).unpack(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

pub fn sha256_hex(src: &Path) -> Result<String> {
    let mut f = File::open(src).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radialactions_arc_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn sha256_known_vector() {
        let d = dir("sha");
        let f = d.join("a.txt");
        std::fs::write(&f, "abc").unwrap();
        assert_eq!(
            sha256_hex(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn folder_zip_round_trips() {
        let d = dir("zip");
        let folder = d.join("proj");
        std::fs::create_dir_all(folder.join("deep/er")).unwrap();
        std::fs::write(folder.join("a.txt"), "hello").unwrap();
        std::fs::write(folder.join("deep/er/b.bin"), vec![7u8; 5000]).unwrap();

        let (zip, n) = zip_path(&folder).unwrap();
        assert_eq!(n, 2);
        assert_eq!(zip.file_name().unwrap(), "proj.zip");

        std::fs::remove_dir_all(&folder).unwrap();
        let (out, count) = unzip(&zip).unwrap();
        assert_eq!(count, 2);
        assert_eq!(out.file_name().unwrap(), "proj");
        assert_eq!(std::fs::read_to_string(out.join("proj/a.txt")).unwrap(), "hello");
        assert_eq!(std::fs::read(out.join("proj/deep/er/b.bin")).unwrap().len(), 5000);
    }

    #[test]
    fn zips_a_mixed_selection_and_keeps_duplicate_names() {
        let d = dir("many");
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("same.txt"), "one").unwrap();
        std::fs::write(b.join("same.txt"), "two").unwrap();
        let folder = d.join("dir");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("x.txt"), "x").unwrap();
        let (zip, n) = zip_many(&[a.join("same.txt"), b.join("same.txt"), folder]).unwrap();
        assert_eq!(n, 3);
        assert_eq!(zip.file_name().unwrap(), "same + 2 more.zip");
        let mut z = zip::ZipArchive::new(File::open(&zip).unwrap()).unwrap();
        assert_eq!(z.len(), 3);
        let names: Vec<String> = (0..3).map(|i| z.by_index(i).unwrap().name().unwrap().to_string()).collect();
        assert!(names.contains(&"same.txt".to_string()) && names.contains(&"1_same.txt".to_string()));
        assert!(names.contains(&"dir/x.txt".to_string()));
    }

    #[test]
    fn single_file_zip() {
        let d = dir("one");
        let f = d.join("note.txt");
        std::fs::write(&f, "x").unwrap();
        let (zip, n) = zip_path(&f).unwrap();
        assert_eq!(n, 1);
        let (out, _) = unzip(&zip).unwrap();
        assert!(out.join("note.txt").exists());
    }

    #[test]
    fn unzip_blocks_path_traversal() {
        let d = dir("slip");
        let zip_path = d.join("evil.zip");
        {
            let mut z = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            z.start_file("../escaped.txt", SimpleFileOptions::default()).unwrap();
            z.write_all(b"bad").unwrap();
            z.start_file("ok.txt", SimpleFileOptions::default()).unwrap();
            z.write_all(b"fine").unwrap();
            z.finish().unwrap();
        }
        let (out, count) = unzip(&zip_path).unwrap();
        assert_eq!(count, 1);
        assert!(out.join("ok.txt").exists());
        assert!(!d.join("escaped.txt").exists());
    }

    #[test]
    fn tar_gz_extracts() {
        let d = dir("tgz");
        let tgz = d.join("pack.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(File::create(&tgz).unwrap(), flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            let data = b"inside";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "dir/f.txt", &data[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = untar_gz(&tgz).unwrap();
        assert_eq!(out.file_name().unwrap(), "pack");
        assert_eq!(std::fs::read_to_string(out.join("dir/f.txt")).unwrap(), "inside");
    }
}
