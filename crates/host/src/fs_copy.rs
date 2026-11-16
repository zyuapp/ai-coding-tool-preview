//! Recursive copies that keep modification times and copy symlinks as links, the way `fs.cp` does.

use std::fs;
use std::io;
use std::path::Path;

/// Copies a file, symlink or folder to `to`, making its parents, overwriting what is there.
pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let metadata = fs::symlink_metadata(from)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(from)?;
        if fs::symlink_metadata(to).is_ok() {
            fs::remove_file(to)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, to)?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::set_permissions(to, metadata.permissions())?;
        return keep_times(to, &metadata);
    }
    fs::copy(from, to)?;
    keep_times(to, &metadata)
}

fn keep_times(to: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    let mut times = fs::FileTimes::new();
    if let Ok(modified) = metadata.modified() {
        times = times.set_modified(modified);
    }
    if let Ok(accessed) = metadata.accessed() {
        times = times.set_accessed(accessed);
    }
    match fs::File::options().write(true).open(to).or_else(|_| fs::File::open(to)) {
        Ok(file) => file.set_times(times).or(Ok(())),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_a_tree_with_its_links() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        fs::create_dir_all(from.join("nested")).unwrap();
        fs::write(from.join("nested/file"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("nested/file", from.join("link")).unwrap();
        let to = dir.path().join("deep/to");
        copy_recursive(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("nested/file")).unwrap(), "x");
        #[cfg(unix)]
        assert_eq!(fs::read_link(to.join("link")).unwrap(), Path::new("nested/file"));
    }
}
