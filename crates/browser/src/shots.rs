//! Where pictures of pages are kept: a temporary folder made on the first capture and removed with
//! the host, holding only the newest few.
// Only macOS has the WebKit pages these serve.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::collections::VecDeque;
use std::path::PathBuf;

/// A new picture past this deletes the oldest, so a long run is bounded.
pub const MAX_SHOTS: usize = 20;

#[derive(Default)]
pub struct Shots {
    home: Option<PathBuf>,
    files: VecDeque<PathBuf>,
}

impl Shots {
    fn directory(&mut self) -> std::io::Result<PathBuf> {
        if let Some(home) = &self.home {
            if home.is_dir() {
                return Ok(home.clone());
            }
        }
        let home = std::env::temp_dir().join(format!("aicodingtool-shots-{}", uuid::Uuid::new_v4().simple()));
        act_platform::fs::create_private_dir_all(&home)?;
        self.home = Some(home.clone());
        Ok(home)
    }

    /// Keeps the folder bounded. A caller that has yet to open its file still has the newest ones.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let file = self.directory()?.join(format!("{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&file, bytes)?;
        self.files.push_back(file.clone());
        while self.files.len() > MAX_SHOTS {
            if let Some(oldest) = self.files.pop_front() {
                let _ = std::fs::remove_file(oldest);
            }
        }
        Ok(file)
    }

    /// Pictures only serve the run that asked for them, so none outlives the host.
    pub fn discard(&mut self) {
        self.files.clear();
        if let Some(home) = self.home.take() {
            let _ = std::fs::remove_dir_all(home);
        }
    }
}

impl Drop for Shots {
    fn drop(&mut self) {
        self.discard();
    }
}

/// What a PNG really measures, read from its header.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[1..4] != b"PNG" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_keeps_only_the_newest_pictures() {
        let mut shots = Shots::default();
        let first = shots.write(b"a").unwrap();
        for _ in 0..MAX_SHOTS {
            shots.write(b"b").unwrap();
        }
        assert!(!first.exists());
        let home = shots.home.clone().unwrap();
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), MAX_SHOTS);
        shots.discard();
        assert!(!home.exists());
    }
}
