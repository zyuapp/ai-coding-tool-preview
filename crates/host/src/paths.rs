//! Lexical path helpers with Node's `path` semantics, which the stored records were written with.

use std::path::{Component, Path, PathBuf};

/// `path.resolve`: absolute against the current directory, with `.` and `..` folded away lexically.
pub fn resolve(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")).join(path) };
    normalize(&joined)
}

/// `path.resolve(base, path)`.
pub fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    resolve(&base.join(path))
}

/// Folds `.` and `..` without touching the disk. A `..` above the root stays at the root.
pub fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else if !matches!(parts.last(), Some(Component::RootDir) | Some(Component::Prefix(_))) {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    let mut normalized = PathBuf::new();
    for part in parts {
        normalized.push(part.as_os_str());
    }
    if normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    normalized
}

/// Whether `candidate` is `root` or lies beneath it, compared lexically after resolving both.
pub fn is_path_inside(root: &Path, candidate: &Path) -> bool {
    resolve(candidate).starts_with(resolve(root))
}

/// Whether `file`, resolved against `root`, stays inside it.
pub fn stays_inside(root: &Path, file: &str) -> bool {
    is_path_inside(root, &resolve_from(root, Path::new(file)))
}

pub use act_platform::paths::home;

/// `~` and `~/…` expanded against the home folder; anything else as written.
pub fn expand_home(named: &str) -> PathBuf {
    if named == "~" {
        home()
    } else if let Some(rest) = named.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(named)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_dots_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn a_sibling_with_a_shared_prefix_is_not_inside() {
        assert!(is_path_inside(Path::new("/a/b"), Path::new("/a/b/c")));
        assert!(is_path_inside(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_path_inside(Path::new("/a/b"), Path::new("/a/bc")));
        assert!(!stays_inside(Path::new("/a/b"), "../c"));
    }
}
