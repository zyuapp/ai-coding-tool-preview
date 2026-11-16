//! The local images a message links, which the app previews under it. Ported from
//! `application/message-images.ts`.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::links::{may_link_images, message_image_path, MAX_MESSAGE_IMAGES};

/// A linked image: its path and the label to show for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageImage {
    pub path: String,
    pub label: String,
}

/// Link and image destinations naming local raster images, in order, without repeats. Reference
/// links count. A link's label is its own text, an image's is its alt text, else "Screenshot".
pub fn message_images(text: &str) -> Vec<MessageImage> {
    if !may_link_images(text) {
        return Vec::new();
    }
    let mut images: Vec<MessageImage> = Vec::new();
    // The open link or image: its path, whether it is an image, its label so far, and its depth.
    let mut open: Option<(Option<String>, bool, String, usize)> = None;
    let mut depth = 0usize;
    for event in Parser::new(text) {
        if images.len() >= MAX_MESSAGE_IMAGES {
            break;
        }
        match event {
            Event::Start(tag @ (Tag::Link { .. } | Tag::Image { .. })) if open.is_none() => {
                let (is_image, dest_url) = match tag {
                    Tag::Image { dest_url, .. } => (true, dest_url),
                    Tag::Link { dest_url, .. } => (false, dest_url),
                    _ => continue,
                };
                let path = message_image_path(&dest_url)
                    .filter(|path| !images.iter().any(|image| &image.path == path));
                open = Some((path, is_image, String::new(), depth));
                depth += 1;
            }
            Event::End(TagEnd::Link | TagEnd::Image)
                if open.as_ref().is_some_and(|(_, _, _, at)| *at + 1 == depth) =>
            {
                depth -= 1;
                if let Some((Some(path), _, label, _)) = open.take() {
                    let label = if label.is_empty() {
                        "Screenshot".to_string()
                    } else {
                        label
                    };
                    images.push(MessageImage { path, label });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_images_are_listed_once_with_their_labels() {
        let text = "See [old shot](/tmp/old%20shot.png), ![Another](/tmp/b.png) and [**bold**](/tmp/c.jpg).\n\n[again](/tmp/b.png) [web](https://x.dev/a.png) [ref][r]\n\n[r]: /tmp/d.webp";
        let images = message_images(text);
        let found: Vec<(&str, &str)> = images
            .iter()
            .map(|image| (image.path.as_str(), image.label.as_str()))
            .collect();
        assert_eq!(
            found,
            vec![
                ("/tmp/old shot.png", "old shot"),
                ("/tmp/b.png", "Another"),
                ("/tmp/c.jpg", "Screenshot"),
                ("/tmp/d.webp", "ref")
            ]
        );
    }
}
