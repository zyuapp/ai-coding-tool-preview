//! The window's elements as an AccessKit tree, shared by the macOS and Linux adapters. Nodes nest
//! under the container they were painted in; text fields carry one text run per row.

use accesskit::{Action, Node, NodeId, Rect, Role, Toggled, TreeId, TreeInfo, TreeUpdate};

use crate::{AccessNode, AccessRole};

pub(crate) const ROOT: NodeId = NodeId(0);

/// Whether an action asked of a node with `role` is carried out by pressing it. Focus is asked as
/// assistive technology's cursor moves, so it presses only fields, where a press just focuses.
pub(crate) fn presses(action: Action, role: AccessRole) -> bool {
    match action {
        Action::Click => true,
        Action::Focus => matches!(role, AccessRole::TextInput | AccessRole::MultilineTextInput),
        _ => false,
    }
}

pub(crate) fn reads_only(role: AccessRole) -> bool {
    matches!(
        role,
        AccessRole::Label
            | AccessRole::Heading
            | AccessRole::Image
            | AccessRole::Group
            | AccessRole::Dialog
            | AccessRole::List
            | AccessRole::Menu
            | AccessRole::Navigation
            | AccessRole::Region
            | AccessRole::Meter
            | AccessRole::TabList
            | AccessRole::Article
            | AccessRole::Paragraph
            | AccessRole::ListEntry
            | AccessRole::ListMarker
            | AccessRole::Time
            | AccessRole::Code
            | AccessRole::Strong
            | AccessRole::Emphasis
            | AccessRole::Main
            | AccessRole::Complementary
            | AccessRole::SectionHeader
            | AccessRole::SectionFooter
            | AccessRole::Details
    )
}

fn role(role: AccessRole) -> Role {
    match role {
        AccessRole::Button => Role::Button,
        // Read as AXPopUpButton, as a browser reads a button that opens a menu.
        #[cfg(target_os = "macos")]
        AccessRole::PopupButton => Role::ComboBox,
        #[cfg(not(target_os = "macos"))]
        AccessRole::PopupButton => Role::Button,
        AccessRole::ToggleButton => Role::Button,
        AccessRole::CheckBox => Role::CheckBox,
        AccessRole::Switch => Role::Switch,
        AccessRole::TextInput => Role::TextInput,
        AccessRole::MultilineTextInput => Role::MultilineTextInput,
        AccessRole::Label => Role::Label,
        // AccessKit's macOS adapter reads Heading as a role VoiceOver does not know; this one is AXHeading.
        #[cfg(target_os = "macos")]
        AccessRole::Heading => Role::DocSubtitle,
        #[cfg(not(target_os = "macos"))]
        AccessRole::Heading => Role::Heading,
        AccessRole::Link => Role::Link,
        AccessRole::Tab => Role::Tab,
        AccessRole::MenuItem => Role::MenuItem,
        // A clickable row reads as a button, as a row with a press action is one to VoiceOver.
        AccessRole::ListItem => Role::Button,
        AccessRole::Dialog => Role::Dialog,
        AccessRole::Group => Role::Group,
        AccessRole::Image => Role::Image,
        AccessRole::List => Role::List,
        AccessRole::Menu => Role::Menu,
        AccessRole::Navigation => Role::Navigation,
        AccessRole::Region => Role::Region,
        AccessRole::Meter => Role::Meter,
        AccessRole::Splitter => Role::Splitter,
        AccessRole::TabList => Role::TabList,
        AccessRole::Slider => Role::Slider,
        AccessRole::Article => Role::Article,
        AccessRole::Paragraph => Role::Paragraph,
        AccessRole::ListEntry => Role::ListItem,
        AccessRole::ListMarker => Role::ListMarker,
        AccessRole::Time => Role::Time,
        AccessRole::Code => Role::Code,
        AccessRole::Strong => Role::Strong,
        AccessRole::Emphasis => Role::Emphasis,
        AccessRole::Main => Role::Main,
        AccessRole::Complementary => Role::Complementary,
        AccessRole::SectionHeader => Role::SectionHeader,
        AccessRole::SectionFooter => Role::SectionFooter,
        AccessRole::Details => Role::Details,
        AccessRole::Summary => Role::DisclosureTriangle,
    }
}

/// What VoiceOver calls a role AppKit has no words for, as Chromium names it.
fn role_description(role: AccessRole) -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    Some(match role {
        AccessRole::Article => "article",
        AccessRole::Main => "main",
        AccessRole::Complementary => "complementary",
        AccessRole::Navigation => "navigation",
        AccessRole::Region => "region",
        AccessRole::SectionHeader => "section header",
        AccessRole::SectionFooter => "section footer",
        AccessRole::Time => "time",
        AccessRole::Code => "code",
        AccessRole::Strong => "strong",
        AccessRole::Emphasis => "emphasis",
        AccessRole::Details => "details",
        AccessRole::Summary => "disclosure triangle",
        _ => return None,
    })
}

/// The id of a field's `index`th text run.
fn run_id(field: u64, index: usize) -> NodeId {
    use std::hash::{Hash, Hasher};
    let mut hasher = collections::FxHasher::default();
    (field, index, "run").hash(&mut hasher);
    NodeId(hasher.finish().max(1))
}

fn rect(bounds: crate::Bounds<crate::ScaledPixels>) -> Rect {
    Rect {
        x0: bounds.origin.x.0 as f64,
        y0: bounds.origin.y.0 as f64,
        x1: (bounds.origin.x.0 + bounds.size.width.0) as f64,
        y1: (bounds.origin.y.0 + bounds.size.height.0) as f64,
    }
}

/// A field's text as one AccessKit text run per row, and where its selection lies among them.
fn text_runs(
    field: u64,
    text: &crate::AccessText,
) -> (Vec<(NodeId, Node)>, Option<accesskit::TextSelection>) {
    let mut runs = Vec::with_capacity(text.runs.len());
    let mut anchor = None;
    let mut focus = None;
    let place = |offset: usize, run: &crate::AccessTextRun, id: NodeId, last: bool| -> Option<accesskit::TextPosition> {
        let inside = offset >= run.range.start && (offset < run.range.end || (last && offset == run.range.end));
        inside.then(|| accesskit::TextPosition {
            node: id,
            character_index: text.text[run.range.start..offset.min(text.text.len())].chars().count(),
        })
    };
    let count = text.runs.len();
    for (index, run) in text.runs.iter().enumerate() {
        let id = run_id(field, index);
        let value = text.text.get(run.range.clone()).unwrap_or_default();
        let mut node = Node::new(Role::TextRun);
        node.set_value(value);
        node.set_text_direction(accesskit::TextDirection::LeftToRight);
        node.set_character_lengths(value.chars().map(|ch| ch.len_utf8() as u8).collect::<Vec<_>>());
        node.set_character_positions(run.positions.clone());
        node.set_character_widths(run.widths.clone());
        let mut starts = Vec::new();
        let mut space = true;
        for (at, ch) in value.chars().enumerate() {
            if space && !ch.is_whitespace() && at > 0 {
                starts.push(at as u8);
            }
            space = ch.is_whitespace();
            if at >= 255 {
                break;
            }
        }
        node.set_word_starts(starts);
        let bounds = run.bounds;
        node.set_bounds(Rect {
            x0: bounds.origin.x.0 as f64,
            y0: bounds.origin.y.0 as f64,
            x1: (bounds.origin.x.0 + bounds.size.width.0) as f64,
            y1: (bounds.origin.y.0 + bounds.size.height.0) as f64,
        });
        let last = index + 1 == count;
        anchor = anchor.or_else(|| place(text.anchor, run, id, last));
        focus = focus.or_else(|| place(text.focus, run, id, last));
        runs.push((id, node));
    }
    let selection = anchor.zip(focus).map(|(anchor, focus)| accesskit::TextSelection { anchor, focus });
    (runs, selection)
}

pub(crate) fn tree(title: &str, elements: &[AccessNode]) -> TreeUpdate {
    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(elements.len() + 1);
    let mut root = Node::new(Role::Window);
    root.set_label(title);
    let mut focus = ROOT;
    let mut seen = collections::FxHashSet::default();
    let mut index_of = collections::FxHashMap::default();
    let mut runs = Vec::new();
    for element in elements {
        // An element painted twice in a frame is read once.
        if !seen.insert(element.id) {
            continue;
        }
        let id = NodeId(element.id);
        let info = &element.info;
        let mut node = Node::new(role(info.role));
        if !info.label.is_empty() {
            node.set_label(info.label.as_ref());
        }
        if let Some(description) = &info.description {
            node.set_description(description.as_ref());
        }
        if let Some(text) = &info.text {
            let (field_runs, selection) = text_runs(element.id, text);
            for (run_id, _) in &field_runs {
                node.push_child(*run_id);
            }
            if let Some(selection) = selection {
                node.set_text_selection(selection);
            }
            node.set_value(text.text.as_ref());
            if let Some(placeholder) = &info.value {
                if text.text.is_empty() {
                    node.set_placeholder(placeholder.as_ref());
                }
            }
            runs.extend(field_runs);
        } else if let Some(value) = &info.value {
            node.set_value(value.as_ref());
        } else if matches!(info.role, AccessRole::Label | AccessRole::ListMarker) && !info.label.is_empty() {
            // Static text is read from its value.
            node.set_value(info.label.as_ref());
        }
        if let Some(value) = info.numeric {
            node.set_numeric_value(value);
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(100.0);
        }
        if info.modal {
            node.set_modal();
        }
        if let Some(url) = &info.url {
            node.set_url(url.as_ref());
        }
        if let Some(description) = role_description(info.role) {
            node.set_role_description(description);
        }
        if let Some(toggled) = info.toggled {
            node.set_toggled(if toggled { Toggled::True } else { Toggled::False });
        }
        if let Some(expanded) = info.expanded {
            node.set_expanded(expanded);
        }
        if let Some(selected) = info.selected {
            node.set_selected(selected);
        }
        if info.role == AccessRole::PopupButton {
            node.set_has_popup(accesskit::HasPopup::Menu);
        }
        if info.disabled {
            node.set_disabled();
        } else if !reads_only(info.role) {
            node.add_action(Action::Click);
            node.add_action(Action::Focus);
        }
        node.set_bounds(rect(element.bounds));
        if element.focused {
            focus = id;
        }
        index_of.insert(element.id, nodes.len());
        nodes.push((id, node));
    }
    // Each node goes under the container it was painted in, else under the window.
    let mut placed = collections::FxHashSet::default();
    for element in elements {
        if !placed.insert(element.id) {
            continue;
        }
        let parent = element.parent.and_then(|parent| index_of.get(&parent).copied());
        match parent {
            Some(parent) if parent != index_of[&element.id] => nodes[parent].1.push_child(NodeId(element.id)),
            _ => root.push_child(NodeId(element.id)),
        }
    }
    nodes.extend(runs);
    nodes.insert(0, (ROOT, root));
    TreeUpdate { nodes, tree: Some(TreeInfo::new(ROOT)), tree_id: TreeId::ROOT, focus }
}
