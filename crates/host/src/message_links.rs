//! The local images a reply links, read from its Markdown the way the window previews them, and kept
//! once the reply is written so a later read still finds them.

use std::sync::Arc;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::message_images::{is_message_image_file, MessageImageStore, MAX_MESSAGE_IMAGES};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedImage {
    pub path: String,
    pub label: String,
}

const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// A cheap test that rules out text linking no image, before any Markdown is parsed.
pub fn may_link_images(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices('.').any(|(at, _)| {
        IMAGE_EXTENSIONS.iter().any(|extension| {
            let end = at + 1 + extension.len();
            lower[at + 1..].starts_with(extension) && bytes.get(end).is_none_or(|next| !(next.is_ascii_alphanumeric() || *next == b'_'))
        })
    })
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn is_windows_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn has_url_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once(':') else { return false };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn decode_uri_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

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

/// `:\d+(?::\d+)?$`, as the index it starts at.
fn line_suffix(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let digits_before = |end: usize| {
        let mut start = end;
        while start > 0 && bytes[start - 1].is_ascii_digit() {
            start -= 1;
        }
        start
    };
    let start = digits_before(bytes.len());
    if start == bytes.len() || start == 0 || bytes[start - 1] != b':' {
        return None;
    }
    let colon = start - 1;
    let second = digits_before(colon);
    if second < colon && second > 0 && bytes[second - 1] == b':' {
        return Some(second - 1);
    }
    Some(colon)
}

/// Only a local raster image named without a line is previewed; web links keep their usual behaviour.
pub fn message_image_path(href: &str) -> Option<String> {
    if href.is_empty() || href.starts_with('#') || href.starts_with("//") {
        return None;
    }
    let decoded = decode_uri_component(strip_file_url(href).unwrap_or(href))?;
    if line_suffix(&decoded).is_some() {
        return None;
    }
    let file = decoded.as_str();
    if file.is_empty() || file.ends_with('/') || (has_url_scheme(file) && !is_windows_path(file)) {
        return None;
    }
    is_message_image_file(file).then(|| file.to_string())
}

/// Link and image destinations naming local raster images, in order and once each, reference links
/// included. A link is labelled by its own text, an image by its alt text, else "Screenshot".
pub fn message_images(text: &str) -> Vec<LinkedImage> {
    if !may_link_images(text) {
        return Vec::new();
    }
    let mut images: Vec<LinkedImage> = Vec::new();
    // The open link or image: its path, whether it is an image, its label so far, and its depth.
    let mut open: Option<(Option<String>, bool, String, usize)> = None;
    let mut depth = 0usize;
    for event in Parser::new_ext(text, Options::empty()) {
        if images.len() >= MAX_MESSAGE_IMAGES {
            break;
        }
        match event {
            Event::Start(tag @ (Tag::Link { .. } | Tag::Image { .. })) if open.is_none() => {
                let (is_image, destination) = match tag {
                    Tag::Image { dest_url, .. } => (true, dest_url),
                    Tag::Link { dest_url, .. } => (false, dest_url),
                    _ => continue,
                };
                let path = message_image_path(&destination).filter(|path| !images.iter().any(|image| &image.path == path));
                open = Some((path, is_image, String::new(), depth));
                depth += 1;
            }
            Event::End(TagEnd::Link | TagEnd::Image) if open.as_ref().is_some_and(|(_, _, _, at)| *at + 1 == depth) => {
                depth -= 1;
                if let Some((Some(path), _, label, _)) = open.take() {
                    images.push(LinkedImage { path, label: if label.is_empty() { "Screenshot".into() } else { label } });
                }
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Text(value) | Event::Code(value) => {
                if let Some((_, is_image, label, at)) = open.as_mut() {
                    if *is_image || *at + 1 == depth {
                        label.push_str(&value);
                    }
                }
            }
            _ => {}
        }
    }
    images
}

/// The `PreserveMessageImages` effect: keeps a copy of every image the reply links, so its previews
/// outlive the files they were taken from.
pub async fn preserve_reply_images(store: &Arc<MessageImageStore>, text: &str, root: &str, message_id: &str) -> Result<(), String> {
    let files: Vec<String> = message_images(text).into_iter().map(|image| image.path).collect();
    if files.is_empty() {
        return Ok(());
    }
    store.preserve_all(&files, root, message_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_images_are_listed_once_with_their_labels() {
        let text = "See [old shot](/tmp/old%20shot.png), ![Another](/tmp/b.png) and [**bold**](/tmp/c.jpg).\n\n[again](/tmp/b.png) [web](https://x.dev/a.png) [line](/tmp/e.png:4) [ref][r]\n\n[r]: /tmp/d.webp";
        let found: Vec<(String, String)> = message_images(text).into_iter().map(|image| (image.path, image.label)).collect();
        let expected = [("/tmp/old shot.png", "old shot"), ("/tmp/b.png", "Another"), ("/tmp/c.jpg", "Screenshot"), ("/tmp/d.webp", "ref")];
        assert_eq!(found, expected.map(|(path, label)| (path.to_string(), label.to_string())));
    }
}
