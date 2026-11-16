//! A macOS bundle's own icon, read out of its `.icns` as the PNG that suits a menu row.

use std::path::{Path, PathBuf};

const ICNS_MAGIC: &[u8] = b"icns";
const ICNS_HEADER: usize = 8;
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
/// The smallest icon worth taking for a 20 point row, which a Retina screen draws at 40 pixels.
const WANTED_WIDTH: u32 = 40;

fn is_png(record: &[u8]) -> bool {
    record.len() > 24 && record.starts_with(&PNG_MAGIC)
}

/// A PNG says how wide it is in the IHDR chunk, which always comes first.
fn png_width(record: &[u8]) -> u32 {
    u32::from_be_bytes([record[16], record[17], record[18], record[19]])
}

/// The smallest PNG at least 40 pixels across, else the widest there is. Older packed formats are skipped.
pub fn png_in_icns(data: &[u8]) -> Option<&[u8]> {
    if data.len() < ICNS_HEADER || &data[..4] != ICNS_MAGIC {
        return None;
    }
    let mut best: Option<&[u8]> = None;
    let mut offset = ICNS_HEADER;
    while offset + ICNS_HEADER <= data.len() {
        let length = u32::from_be_bytes([data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7]]) as usize;
        if length < ICNS_HEADER {
            break;
        }
        let record = &data[(offset + ICNS_HEADER).min(data.len())..(offset + length).min(data.len())];
        offset += length;
        if !is_png(record) {
            continue;
        }
        let Some(chosen) = best else {
            best = Some(record);
            continue;
        };
        let (width, chosen_width) = (png_width(record), png_width(chosen));
        let better = if chosen_width < WANTED_WIDTH { width > chosen_width } else { width >= WANTED_WIDTH && width < chosen_width };
        if better {
            best = Some(record);
        }
    }
    best
}

/// The icon a bundle names for itself. The extension is left off often enough to add it back.
fn named_icon(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>CFBundleIconFile</key>")? + "<key>CFBundleIconFile</key>".len()..];
    let after = after.trim_start().strip_prefix("<string>")?;
    let named = after[..after.find("</string>")?].trim();
    if named.is_empty() || named.contains('<') {
        return None;
    }
    Some(if named.to_lowercase().ends_with(".icns") { named.to_string() } else { format!("{named}.icns") })
}

/// A bundle that names its icon is taken at its word; one that does not, or whose property list is
/// binary, gives up its only `.icns` when it has exactly one.
fn icon_path(bundle: &Path) -> Option<PathBuf> {
    let resources = bundle.join("Contents").join("Resources");
    let plist = std::fs::read_to_string(bundle.join("Contents").join("Info.plist")).unwrap_or_default();
    if let Some(named) = named_icon(&plist) {
        return Some(resources.join(named));
    }
    let found: Vec<PathBuf> = std::fs::read_dir(&resources).ok()?.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("icns"))).collect();
    (found.len() == 1).then(|| found[0].clone())
}

/// A macOS bundle's own icon as PNG bytes, or None when it keeps one nothing here can read.
pub fn bundle_icon(bundle: &Path) -> Option<Vec<u8>> {
    let data = std::fs::read(icon_path(bundle)?).ok()?;
    png_in_icns(&data).map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32) -> Vec<u8> {
        let mut record = PNG_MAGIC.to_vec();
        record.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
        record.extend_from_slice(&width.to_be_bytes());
        record.extend_from_slice(&width.to_be_bytes());
        record.extend_from_slice(&[0; 8]);
        record
    }

    fn icns(records: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for record in records {
            body.extend_from_slice(b"ic07");
            body.extend_from_slice(&((record.len() + 8) as u32).to_be_bytes());
            body.extend_from_slice(record);
        }
        let mut data = b"icns".to_vec();
        data.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        data.extend(body);
        data
    }

    #[test]
    fn takes_the_smallest_png_wide_enough_for_a_row() {
        let data = icns(&[png(16), png(512), png(64), png(32)]);
        assert_eq!(png_width(png_in_icns(&data).unwrap()), 64);
        let small = icns(&[png(16), png(32)]);
        assert_eq!(png_width(png_in_icns(&small).unwrap()), 32);
    }

    #[test]
    fn reads_the_icon_name_and_adds_its_extension() {
        assert_eq!(named_icon("<key>CFBundleIconFile</key>\n\t<string>AppIcon</string>").as_deref(), Some("AppIcon.icns"));
        assert_eq!(named_icon("<key>Other</key>"), None);
    }
}
