//! What a link in a message names, ported from `domain/markdown-links.ts` and
//! `domain/message-artifacts.ts`.

/// The file a link names, and the line to open it at when the link carried one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FileHref {
    pub file: String,
    pub line: Option<u64>,
}

/// `aicodingtool://thread/<id>`, case-insensitive.
pub fn parse_thread_href(href: &str) -> Option<String> {
    const PREFIX: &str = "aicodingtool://thread/";
    if href.len() < PREFIX.len() || !href[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let id = &href[PREFIX.len()..];
    if id.is_empty() || id.contains(['/', '?', '#']) {
        return None;
    }
    Some(id.to_string())
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.is_char_boundary(prefix.len())
        && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// `^[A-Za-z]:[\\/]`
fn is_windows_path(file: &str) -> bool {
    let bytes = file.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// `^[A-Za-z][A-Za-z\d+.-]*:`
fn has_url_scheme(file: &str) -> bool {
    let bytes = file.as_bytes();
    if bytes.first().is_none_or(|byte| !byte.is_ascii_alphabetic()) {
        return false;
    }
    for &byte in &bytes[1..] {
        if byte == b':' {
            return true;
        }
        if !(byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'.' || byte == b'-') {
            return false;
        }
    }
    false
}

/// `decodeURIComponent`: `None` where it would throw.
pub fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let text = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Strips `file://` or `file://localhost` before an absolute path, and the slash before a drive letter.
fn strip_file_url(href: &str) -> Option<&str> {
    if !starts_with_ignore_case(href, "file://") {
        return None;
    }
    let mut rest = &href["file://".len()..];
    if starts_with_ignore_case(rest, "localhost") && rest["localhost".len()..].starts_with('/') {
        rest = &rest["localhost".len()..];
    }
    if !rest.starts_with('/') {
        return None;
    }
    let after = &rest[1..];
    if is_windows_path(after) && after.as_bytes()[2] == b'/' {
        return Some(after);
    }
    Some(rest)
}

/// `:\d+(?::\d+)?$`, as the index it starts at and the line it names.
fn line_suffix(text: &str) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    let digits_before = |end: usize| {
        let mut start = end;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        start
    };
    let end = bytes.len();
    let start = digits_before(end);
    if start == end || start == 0 || bytes[start - 1] != b':' {
        return None;
    }
    let colon = start - 1;
    let second_start = digits_before(colon);
    if second_start < colon && second_start > 0 && bytes[second_start - 1] == b':' {
        return Some((second_start - 1, &text[second_start..colon]));
    }
    Some((colon, &text[start..end]))
}

/// The file a link names, and the line to open it at when the link carried one.
pub fn parse_file_href(href: &str) -> Option<FileHref> {
    if href.is_empty() || href.starts_with('#') || href.starts_with("//") {
        return None;
    }
    let decoded = decode_uri_component(strip_file_url(href).unwrap_or(href))?;
    let (file, line) = match line_suffix(&decoded) {
        Some((at, digits)) => {
            let line = digits.parse::<u64>().unwrap_or(u64::MAX);
            (&decoded[..at], line)
        }
        None => (decoded.as_str(), 0),
    };
    if file.is_empty() || file.ends_with('/') || (has_url_scheme(file) && !is_windows_path(file)) {
        return None;
    }
    Some(FileHref {
        file: file.to_string(),
        line: (line > 0).then_some(line),
    })
}

/// Whether a written path names a place on its own, so no checkout is needed to look for it.
pub fn is_absolute_file_path(file: &str) -> bool {
    file.starts_with('/') || file.starts_with("~/") || is_windows_path(file)
}

/// How many images one message previews.
pub const MAX_MESSAGE_IMAGES: usize = 20;

const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

pub fn is_message_image_file(file: &str) -> bool {
    file.rsplit_once('.').is_some_and(|(_, extension)| {
        IMAGE_EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// A cheap test that rules out text linking no image, before any Markdown is parsed.
pub fn may_link_images(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices('.').any(|(at, _)| {
        IMAGE_EXTENSIONS.iter().any(|extension| {
            let end = at + 1 + extension.len();
            lower[at + 1..].starts_with(extension)
                && bytes
                    .get(end)
                    .is_none_or(|next| !(next.is_ascii_alphanumeric() || *next == b'_'))
        })
    })
}

/// Only local raster images are previewed automatically. Web links keep their usual behaviour.
pub fn message_image_path(href: &str) -> Option<String> {
    let file = parse_file_href(href)?;
    (file.line.is_none() && is_message_image_file(&file.file)).then_some(file.file)
}

pub fn is_commit_hash(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_app_href(url: &str) -> bool {
    starts_with_ignore_case(url, "aicodingtool:")
}

fn is_web_href(url: &str) -> bool {
    starts_with_ignore_case(url, "http:") || starts_with_ignore_case(url, "https:")
}

/// micromark's `normalizeUri`: what a link destination becomes as an `href`.
pub fn normalize_uri(value: &str) -> String {
    fn allowed(byte: u8) -> bool {
        matches!(byte, b'!' | b'#' | b'$' | b'&'..=b';' | b'=' | b'?'..=b'Z' | b'_' | b'a'..=b'z' | b'~')
    }
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_alphanumeric)
            && bytes.get(index + 2).is_some_and(u8::is_ascii_alphanumeric)
        {
            out.push_str(&value[index..index + 3]);
            index += 3;
        } else if byte < 128 {
            if allowed(byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
            index += 1;
        } else {
            let ch = value[index..].chars().next().unwrap_or('\u{FFFD}');
            let mut buffer = [0u8; 4];
            for encoded in ch.encode_utf8(&mut buffer).bytes() {
                out.push_str(&format!("%{encoded:02X}"));
            }
            index += ch.len_utf8();
        }
    }
    out
}

/// react-markdown's `defaultUrlTransform`, widened as the app widens it: links into the app stay,
/// and a `file:` link opens like a bare path. An image never loads straight from the disk.
pub fn url_transform(url: &str, is_image: bool) -> String {
    if is_app_href(url) || (!is_image && starts_with_ignore_case(url, "file:")) {
        return url.to_string();
    }
    let colon = url.find(':');
    let before = |needle: char, colon: usize| url.find(needle).is_some_and(|at| colon > at);
    match colon {
        None => url.to_string(),
        Some(colon) if before('/', colon) || before('?', colon) || before('#', colon) => {
            url.to_string()
        }
        Some(colon) => {
            let protocol = &url[..colon];
            let safe = ["http", "https", "irc", "ircs", "mailto", "xmpp"];
            if safe
                .iter()
                .any(|known| protocol.eq_ignore_ascii_case(known))
            {
                url.to_string()
            } else {
                String::new()
            }
        }
    }
}

/// What the host can open. A capability left off turns that kind of link into plain text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LinkCapabilities {
    pub select_thread: bool,
    pub open_file: bool,
    pub open_url_in_app: bool,
    pub open_image: bool,
    pub open_commit: bool,
}

/// What a rendered link reaches.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LinkTarget {
    /// Selects that thread in place. Drawn as words with a thread glyph.
    Thread(String),
    /// A local image the message linked. The host opens it in its viewer.
    Image(String),
    /// Opens the file, at the line when the link named one. Drawn with a file glyph.
    File(FileHref),
    /// Opens in the browser, or in the app's browser panel with the command key held.
    Web(String),
    /// Any other link: `mailto:`, a relative anchor, or an href the transform emptied.
    Other(String),
    /// An inline code hash the thread's checkout knows.
    Commit(String),
}

impl LinkTarget {
    /// The hint shown over the link: a file names where it goes.
    pub fn tooltip(&self) -> Option<String> {
        match self {
            LinkTarget::File(file) => Some(match file.line {
                Some(line) => format!("{}:{line}", file.file),
                None => file.file.clone(),
            }),
            LinkTarget::Commit(_) => Some("Open commit".into()),
            _ => None,
        }
    }

    /// The text the host's "Copy" entry in a link menu writes.
    pub fn copy_text(&self) -> String {
        match self {
            LinkTarget::Thread(id) => format!("aicodingtool://thread/{id}"),
            LinkTarget::Image(path) => path.clone(),
            LinkTarget::File(file) => file.file.clone(),
            LinkTarget::Web(url) | LinkTarget::Other(url) | LinkTarget::Commit(url) => url.clone(),
        }
    }
}

/// Decides what an href (already normalized and transformed) becomes, in `MarkdownLink`'s order.
/// `None` means the link's words render as plain text.
pub fn classify_href(
    href: &str,
    capabilities: LinkCapabilities,
    has_message_id: bool,
) -> Option<LinkTarget> {
    if let Some(id) = parse_thread_href(href).filter(|_| capabilities.select_thread) {
        return Some(LinkTarget::Thread(id));
    }
    if is_app_href(href) {
        return None;
    }
    if let Some(path) =
        message_image_path(href).filter(|_| has_message_id && capabilities.open_image)
    {
        return Some(LinkTarget::Image(path));
    }
    if let Some(file) = parse_file_href(href) {
        return capabilities.open_file.then_some(LinkTarget::File(file));
    }
    if is_web_href(href) {
        return Some(LinkTarget::Web(href.to_string()));
    }
    Some(LinkTarget::Other(href.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, line: Option<u64>) -> Option<FileHref> {
        Some(FileHref {
            file: path.into(),
            line,
        })
    }

    #[test]
    fn file_urls_name_the_same_file_as_their_bare_path() {
        assert_eq!(
            parse_file_href("file:///tmp/mock%20up.html"),
            file("/tmp/mock up.html", None)
        );
        assert_eq!(
            parse_file_href("file://localhost/tmp/app.ts:12"),
            file("/tmp/app.ts", Some(12))
        );
        assert_eq!(
            parse_file_href("file:///C:/work/page.html"),
            file("C:/work/page.html", None)
        );
        assert_eq!(
            parse_file_href("docs/My%20Notes.md:7:3"),
            file("docs/My Notes.md", Some(7))
        );
        assert_eq!(
            parse_file_href("C:\\work\\a.rs"),
            file("C:\\work\\a.rs", None)
        );
    }

    #[test]
    fn web_links_and_bad_escapes_are_not_files() {
        assert_eq!(parse_file_href("https://example.com/page.html"), None);
        assert_eq!(parse_file_href("file://server/share/page.html"), None);
        assert_eq!(parse_file_href("a%zz"), None);
        assert_eq!(parse_file_href("dir/"), None);
        assert_eq!(parse_file_href("#top"), None);
    }

    #[test]
    fn links_classify_in_the_order_the_app_checks_them() {
        let all = LinkCapabilities {
            select_thread: true,
            open_file: true,
            open_url_in_app: true,
            open_image: true,
            open_commit: true,
        };
        assert_eq!(
            classify_href("aicodingtool://thread/task-9", all, true),
            Some(LinkTarget::Thread("task-9".into()))
        );
        assert_eq!(
            classify_href("aicodingtool://archive/task-9", all, true),
            None
        );
        assert_eq!(
            classify_href(
                "aicodingtool://thread/task-9",
                LinkCapabilities::default(),
                true
            ),
            None
        );
        assert_eq!(
            classify_href("/tmp/shot.png", all, true),
            Some(LinkTarget::Image("/tmp/shot.png".into()))
        );
        assert!(matches!(
            classify_href("/tmp/shot.png", all, false),
            Some(LinkTarget::File(_))
        ));
        assert_eq!(
            classify_href("README.md", LinkCapabilities::default(), true),
            None
        );
        assert_eq!(
            classify_href("https://x.dev", all, true),
            Some(LinkTarget::Web("https://x.dev".into()))
        );
        assert_eq!(
            classify_href("mailto:a@b.c", all, true),
            Some(LinkTarget::Other("mailto:a@b.c".into()))
        );
    }

    #[test]
    fn unsafe_protocols_are_emptied_and_unsafe_bytes_encoded() {
        assert_eq!(url_transform("javascript:alert(1)", false), "");
        assert_eq!(url_transform("file:///a", true), "");
        assert_eq!(url_transform("file:///a", false), "file:///a");
        assert_eq!(url_transform("a/b:c", false), "a/b:c");
        assert_eq!(normalize_uri("docs/My Notes.md"), "docs/My%20Notes.md");
        assert_eq!(normalize_uri("a%20b%"), "a%20b%25");
        assert_eq!(normalize_uri("naïve"), "na%C3%AFve");
    }

    #[test]
    fn image_mentions_need_a_whole_extension() {
        assert!(may_link_images("see shot.PNG here"));
        assert!(!may_link_images("a.pngx"));
        assert!(is_commit_hash("60cceb8"));
        assert!(!is_commit_hash("60cceb"));
        assert!(!is_commit_hash("60cceg8"));
    }
}
