//! What assistive technology reads off an element: its role, its name and its state. Elements that
//! declare it are reported to the platform's accessibility API each frame, in paint order.

use crate::{Bounds, ScaledPixels, SharedString};

impl AccessNode {
    /// Whether a blocking hitbox painted after this node's own covers its centre.
    pub(crate) fn hidden_by(&self, blockers: &[(crate::HitboxId, Bounds<ScaledPixels>)]) -> bool {
        let Some(own) = self.hitbox else { return false };
        let centre = crate::point(
            self.bounds.origin.x + self.bounds.size.width * 0.5,
            self.bounds.origin.y + self.bounds.size.height * 0.5,
        );
        blockers.iter().any(|(id, bounds)| *id > own && bounds.contains(&centre))
    }
}

/// The kind of control an element is to assistive technology.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessRole {
    /// A control that acts when pressed.
    Button,
    /// A button that stays on or off.
    ToggleButton,
    /// A box that is checked or not.
    CheckBox,
    /// An on/off switch.
    Switch,
    /// An editable text field.
    TextInput,
    /// A multi-line editable text field.
    MultilineTextInput,
    /// Text that names something.
    Label,
    /// A section title.
    Heading,
    /// A link to somewhere else.
    Link,
    /// A tab in a tab strip.
    Tab,
    /// A choice in a menu.
    MenuItem,
    /// A row in a list.
    ListItem,
    /// A control that opens a list of choices.
    PopupButton,
    /// A dialog over the window.
    Dialog,
    /// A group of related controls.
    Group,
    /// A picture.
    Image,
    /// A list of rows.
    List,
    /// A menu of choices.
    Menu,
    /// A set of links that move around the app.
    Navigation,
    /// A named part of the window.
    Region,
    /// A level shown as a gauge, such as how full something is.
    Meter,
    /// A handle that resizes the panes beside it.
    Splitter,
    /// A strip of tabs.
    TabList,
    /// A value chosen along a track.
    Slider,
    /// A self-contained piece of content, such as a message.
    Article,
    /// A paragraph of text.
    Paragraph,
    /// An entry in a list of content, read with its marker.
    ListEntry,
    /// The bullet or number before a list entry.
    ListMarker,
    /// A date or time.
    Time,
    /// Code within a run of text.
    Code,
    /// Text marked as important.
    Strong,
    /// Text marked for emphasis.
    Emphasis,
    /// The window's main content.
    Main,
    /// A part beside the main content, such as a sidebar.
    Complementary,
    /// The header of a section.
    SectionHeader,
    /// The footer of a section.
    SectionFooter,
    /// A block that shows more when its summary is pressed.
    Details,
    /// The summary of a details block, pressed to show or hide the rest.
    Summary,
}

impl AccessRole {
    /// Whether elements painted inside it are read as its children.
    pub fn contains(self) -> bool {
        matches!(
            self,
            AccessRole::Group
                | AccessRole::Dialog
                | AccessRole::List
                | AccessRole::Menu
                | AccessRole::Navigation
                | AccessRole::Region
                | AccessRole::Heading
                | AccessRole::TabList
                | AccessRole::Link
                | AccessRole::Article
                | AccessRole::Paragraph
                | AccessRole::ListEntry
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
}

impl AccessRole {
    /// Whether an unnamed element of this role is named by the text inside it.
    pub fn named_by_content(self) -> bool {
        matches!(
            self,
            AccessRole::Button
                | AccessRole::ToggleButton
                | AccessRole::PopupButton
                | AccessRole::Link
                | AccessRole::Tab
                | AccessRole::MenuItem
                | AccessRole::ListItem
                | AccessRole::Heading
                | AccessRole::CheckBox
                | AccessRole::Switch
                | AccessRole::Summary
        )
    }
}

/// A run of a field's text on one visual row.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessTextRun {
    /// Byte range of the row in the field's text.
    pub range: std::ops::Range<usize>,
    /// Where the row sits, in window pixels (device pixels once reported).
    pub bounds: Bounds<crate::Pixels>,
    /// Each character's left edge from the row's left, in window pixels (device pixels once reported).
    pub positions: Vec<f32>,
    /// Each character's advance, in window pixels (device pixels once reported).
    pub widths: Vec<f32>,
}

/// An editable field's text as laid out, so assistive technology can read it by character and
/// follow the caret.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AccessText {
    /// The text.
    pub text: SharedString,
    /// Its visual rows, in order.
    pub runs: Vec<AccessTextRun>,
    /// Where the selection starts, as a byte offset.
    pub anchor: usize,
    /// Where the selection ends and the caret is, as a byte offset.
    pub focus: usize,
}

/// What an element tells assistive technology about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessInfo {
    /// The element's kind.
    pub role: AccessRole,
    /// Its name, read first.
    pub label: SharedString,
    /// Its current value, such as a field's text.
    pub value: Option<SharedString>,
    /// Whether it is on, for toggles and check boxes.
    pub toggled: Option<bool>,
    /// Whether what it opens is open.
    pub expanded: Option<bool>,
    /// Whether it is selected, for rows and tabs.
    pub selected: Option<bool>,
    /// Whether it ignores input.
    pub disabled: bool,
    /// A longer account read after its name, such as what a tooltip says.
    pub description: Option<SharedString>,
    /// For a meter: its value from 0 to 100.
    pub numeric: Option<f64>,
    /// For a dialog: whether the rest of the window is out of reach while it is open.
    pub modal: bool,
    /// For an editable field: its text as laid out.
    pub text: Option<Box<AccessText>>,
    /// Whether it is the one the keyboard is on inside a focused list or menu that keeps focus itself.
    pub active: bool,
    /// For a link: where it goes.
    pub url: Option<SharedString>,
}

impl AccessInfo {
    /// An element of `role` named `label`.
    pub fn new(role: AccessRole, label: impl Into<SharedString>) -> Self {
        Self {
            role,
            label: label.into(),
            value: None,
            toggled: None,
            expanded: None,
            selected: None,
            disabled: false,
            description: None,
            numeric: None,
            modal: false,
            text: None,
            active: false,
            url: None,
        }
    }

    /// Sets the current value.
    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Sets whether it is on.
    pub fn toggled(mut self, toggled: bool) -> Self {
        self.toggled = Some(toggled);
        self
    }

    /// Sets whether what it opens is open.
    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    /// Sets whether it is selected.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    /// Sets whether it ignores input.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Sets the longer account read after its name.
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets a meter's value, from 0 to 100.
    pub fn numeric(mut self, value: f64) -> Self {
        self.numeric = Some(value);
        self
    }

    /// Marks a dialog as modal.
    pub fn modal(mut self) -> Self {
        self.modal = true;
        self
    }

    /// Marks it as where the keyboard is, inside a container holding focus for it.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Sets where a link goes.
    pub fn url(mut self, url: impl Into<SharedString>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Sets a field's laid-out text.
    pub fn text(mut self, text: AccessText) -> Self {
        self.text = Some(Box::new(text));
        self
    }
}

/// An element as painted this frame, for the platform's accessibility tree.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessNode {
    /// Stable across frames for the same element.
    pub id: u64,
    /// What it says about itself.
    pub info: AccessInfo,
    /// Where it is, in device pixels from the window's top left.
    pub bounds: Bounds<ScaledPixels>,
    /// Whether it holds keyboard focus.
    pub focused: bool,
    /// The container it was painted inside, read as its parent.
    pub parent: Option<u64>,
    /// The element's own hitbox, by which a float painted over it hides it.
    pub(crate) hitbox: Option<crate::HitboxId>,
}
