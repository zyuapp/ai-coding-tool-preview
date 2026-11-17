//! The review: what the checkout changed, file by file, side by side or in one column, coloured by
//! its grammar, with lines picked from the gutter and commented on, and each file ticked off as it
//! is read (DiffPanel.tsx, DiffToolbar.tsx, DiffFileRow.tsx, DiffRows.tsx, DiffCommentEditor.tsx,
//! renderer/diff/*).

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;

use act_core::domain::conversation::{Annotation, AnnotationAnchor, DiffSide};
use act_core::domain::diff::{comment_quote, diff_rows, DiffHunk, file_fingerprint, folded_for_size, hunk_text, hunk_text_index, line_on, parse_file_patch, range_key, split_rows, DiffFile, DiffFileStatus, DiffFileSummary, DiffLineKind, DiffMode, DiffRange, DiffRow, SplitRow};
use act_core::domain::find::{match_offsets, review_hits, FindResults, FindTarget, ReviewFile, ReviewHit};
use act_core::events::{ChangedFilesResult, DiffSummaryResult, WorkspaceEvent, WorkspaceInput};
use act_core::state::{diff_for, dock_subject, DiffState, DiffStateMode, WorkspaceState, DIFF_MODE_MENU};
use act_core::view::find::find_view;
use act_core::workspace::location::thread_workspace_id;
use act_core::AppCommand;
use act_markdown::highlight::{highlight_lines, language_for_path, SyntaxRole};
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, ToggleTone};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::field::Checkbox;
use act_ui::components::menu::{Menu, MenuEntry, MenuItem};
use act_ui::components::meter::{Meter, MeterSize};
use act_ui::components::notice::{Notice, NoticeTone};
use act_ui::components::spinner::Spinner;
use act_ui::input::{Autosize, InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, Align, BoundsCell, Placement};
use act_ui::style::{PaneFill, Raised};
use act_ui::theme::{ActiveTheme, Tokens, Tone};
use act_ui::tooltip::{shortcut_tip, Tippable};
use act_ui::IconName;
use gpui::{div, list, prelude::*, px, relative, AnyElement, App, Context, Entity, FontWeight, Hsla, ListAlignment, ListOffset, ListState, MouseButton, Rgba, SharedString, StyledText, Subscription, TextRun, WeakEntity, Window};

use super::common::{icon, ink_icon, rimmed, with_alpha, Ellipsis};
use super::root::Root;
use crate::effects::window_desktop::{self, Branches, DiffPatchResult};
use crate::workspace::dispatch;

/// Two columns need room for two; below this each side is a few words a line.
const SPLIT_MIN_WIDTH: f32 = 520.0;
/// Where the toolbar fits on one row.
const WIDE_TOOLBAR: f32 = 560.0;
const COMPARE_MENU: &str = "diff:compare";
const BASE_MENU: &str = "diff:base";
/// Rows coloured before a file first draws, about two screens of a tall window.
const OPENING_ROWS: usize = 200;
/// What an unwrapped row is counted at until the list measures it.
const ROW_HEIGHT: f32 = 20.0;
const FILE_ROW_HEIGHT: f32 = 39.0;
const HUNK_ROW_HEIGHT: f32 = 24.0;
/// The padding either side of a hunk header.
const HUNK_INSET: f32 = 20.0;
/// The room a unified line's numbers, marker and padding take beside its code.
const UNIFIED_CODE_INSET: f32 = 122.0;
/// The same for one side of a split row.
const SPLIT_CODE_INSET: f32 = 62.0;
const NO_PROJECT: &str = "Open a project to review changes";

/// A file's patch as the host last read it.
#[derive(Clone)]
enum Patch {
    Reading,
    Ready(Rc<DrawnFile>),
    TooLarge(usize),
    Failed(String),
}

/// One file's patch as everything drawing it needs, coloured a hunk at a time as rows are drawn.
pub struct DrawnFile {
    file: DiffFile,
    rows: Vec<DiffRow>,
    pairs: Vec<SplitRow>,
    index_by_key: HashMap<String, usize>,
    hunk_of: HashMap<String, usize>,
    lang: Option<&'static str>,
    tokens: RefCell<HashMap<String, Vec<(Range<usize>, SyntaxRole)>>>,
    coloured: RefCell<HashSet<usize>>,
}

impl DrawnFile {
    fn new(file: DiffFile) -> DrawnFile {
        let rows = diff_rows(&file);
        let pairs = split_rows(&file);
        let index_by_key = rows.iter().enumerate().map(|(index, row)| (row.key.clone(), index)).collect();
        let mut hunk_of = HashMap::new();
        for (index, hunk) in file.hunks.iter().enumerate() {
            for row in &hunk.rows {
                hunk_of.insert(row.key.clone(), index);
            }
        }
        let lang = language_for_path(&file.path);
        DrawnFile { file, rows, pairs, index_by_key, hunk_of, lang, tokens: RefCell::default(), coloured: RefCell::default() }
    }

    /// Colours the hunks holding the first `rows` rows, before the file is first drawn.
    fn colour_opening(&self, rows: usize) {
        let Some(lang) = self.lang else { return };
        let mut seen = 0;
        let mut coloured = self.coloured.borrow_mut();
        let mut tokens = self.tokens.borrow_mut();
        for (index, hunk) in self.file.hunks.iter().enumerate() {
            if seen >= rows {
                break;
            }
            seen += hunk.rows.len();
            coloured.insert(index);
            tokens.extend(hunk_tokens(hunk, lang));
        }
    }

    /// Reads the hunk `key` belongs to, once, off the main thread, and redraws the review when its
    /// colours are in. Until then its rows draw plain.
    fn colour(self: &Rc<Self>, key: &str, root: &WeakEntity<Root>, cx: &mut App) {
        let Some(lang) = self.lang else { return };
        let Some(&index) = self.hunk_of.get(key) else { return };
        if !self.coloured.borrow_mut().insert(index) {
            return;
        }
        let Some(hunk) = self.file.hunks.get(index).cloned() else { return };
        let read = cx.background_executor().spawn(async move { hunk_tokens(&hunk, lang) });
        let drawing = Rc::downgrade(self);
        let root = root.clone();
        cx.spawn(async move |cx| {
            let read = read.await;
            let Some(drawing) = drawing.upgrade() else { return };
            if read.is_empty() {
                return;
            }
            drawing.tokens.borrow_mut().extend(read);
            let _ = root.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
}

/// One hunk's colours by row key. A side is read only for the lines that are its own: context
/// reads the same either way.
fn hunk_tokens(hunk: &DiffHunk, lang: &str) -> Vec<(String, Vec<(Range<usize>, SyntaxRole)>)> {
    let mut sides = Vec::new();
    if hunk.rows.iter().any(|row| row.kind == DiffLineKind::Delete) {
        sides.push(DiffSide::Old);
    }
    if hunk.rows.iter().any(|row| row.kind == DiffLineKind::Add) || hunk.rows.iter().all(|row| row.kind == DiffLineKind::Context) {
        sides.push(DiffSide::New);
    }
    let mut out = Vec::new();
    for side in sides {
        let Some(lines) = highlight_lines(&hunk_text(hunk, side), lang) else { continue };
        for (key, line) in hunk_text_index(hunk, side) {
            if let Some(drawn) = lines.get(line) {
                out.push((key, drawn.clone()));
            }
        }
    }
    out
}

/// Where a comment is being taken: a file and the two rows its run is bounded by, named by key.
#[derive(Clone, Debug, PartialEq)]
struct Selection {
    path: String,
    anchor: String,
    head: String,
}

/// The rows a selection covers now. A hunk header between its ends ends it.
fn span_of(selection: &Selection, drawn: &HashMap<String, Rc<DrawnFile>>) -> Option<(usize, usize)> {
    let drawing = drawn.get(&selection.path)?;
    let from = *drawing.index_by_key.get(&selection.anchor)?;
    let to = *drawing.index_by_key.get(&selection.head)?;
    let (from, to) = (from.min(to), from.max(to));
    if drawing.rows[from..=to].iter().any(|row| row.kind == DiffLineKind::Hunk) {
        return None;
    }
    Some((from, to))
}

/// The old side only when every line taken was taken away.
fn selection_side(rows: &[DiffRow]) -> DiffSide {
    let lines: Vec<&DiffRow> = rows.iter().filter(|row| row.kind != DiffLineKind::Hunk).collect();
    if !lines.is_empty() && lines.iter().all(|row| row.kind == DiffLineKind::Delete) {
        DiffSide::Old
    } else {
        DiffSide::New
    }
}

/// A draft comment pinned to the exact comparison and rows it was made on.
#[derive(Clone)]
struct DiffComment {
    annotation: Annotation,
    number: usize,
    path: String,
    from: usize,
    to: usize,
    marker: usize,
    side: DiffSide,
}

fn anchored_comments(annotations: &[Annotation], comparison: &str, drawn: &HashMap<String, Rc<DrawnFile>>) -> Vec<DiffComment> {
    let mut comments = Vec::new();
    for (index, annotation) in annotations.iter().enumerate() {
        let Some(AnnotationAnchor::Diff { comparison: made_on, path, start, end, side }) = &annotation.anchor else { continue };
        if made_on != comparison {
            continue;
        }
        let Some(drawing) = drawn.get(path) else { continue };
        let (Some(&start), Some(&end)) = (drawing.index_by_key.get(start), drawing.index_by_key.get(end)) else { continue };
        let (from, to) = (start.min(end), start.max(end));
        let selected = &drawing.rows[from..=to];
        if selected.iter().any(|row| row.kind == DiffLineKind::Hunk) || comment_quote(path, selected, *side) != annotation.quote {
            continue;
        }
        let marker = selected.iter().rev().find(|row| line_on(row, *side).is_some()).and_then(|row| drawing.index_by_key.get(&row.key).copied()).unwrap_or(to);
        comments.push(DiffComment { annotation: annotation.clone(), number: index + 1, path: path.clone(), from, to, marker, side: *side });
    }
    comments
}

#[derive(Default)]
struct CommentIndex {
    highlighted: HashSet<(String, usize)>,
    markers: HashMap<(String, usize), Vec<DiffComment>>,
}

fn index_comments(comments: Vec<DiffComment>) -> CommentIndex {
    let mut index = CommentIndex::default();
    for comment in comments {
        for row in comment.from..=comment.to {
            index.highlighted.insert((comment.path.clone(), row));
        }
        index.markers.entry((comment.path.clone(), comment.marker)).or_default().push(comment);
    }
    index
}

/// Every drawn line of the review, flat, so one list covers the whole review.
#[derive(Clone)]
enum PanelRow {
    File(String),
    Note(String, String),
    Line(String, usize),
    Pair(String, usize),
    Composer(String),
}

/// A place the keyboard stops in the review: a file's row, or a line's gutter, on one side in two
/// columns. `line` is the file, the row's key, its side and whether it is picked.
#[derive(Clone)]
struct Stop {
    key: String,
    row: usize,
    line: Option<(String, String, Option<DiffSide>, bool)>,
}

fn file_stop(path: &str) -> String {
    format!("f\n{path}")
}

fn gutter_stop(path: &str, key: &str, side: Option<DiffSide>) -> String {
    format!("g\n{path}\n{key}\n{side:?}")
}

impl PanelRow {
    fn path(&self) -> &str {
        match self {
            PanelRow::File(path) | PanelRow::Note(path, _) | PanelRow::Line(path, _) | PanelRow::Pair(path, _) | PanelRow::Composer(path) => path,
        }
    }
}

fn empty_patch_note(file: &DiffFileSummary) -> String {
    if file.additions == 0 && file.deletions == 0 {
        "No changes to the file's contents".into()
    } else {
        "Nothing to show for this comparison".into()
    }
}

fn patch_note(patch: Option<&Patch>) -> Option<String> {
    match patch {
        None | Some(Patch::Reading) => Some("Reading the patch…".into()),
        Some(Patch::Failed(message)) => Some(message.clone()),
        Some(Patch::TooLarge(limit)) => Some(format!("Patch is larger than {} MB. Open it in your editor.", (*limit as f64 / 1_000_000.0).round())),
        Some(Patch::Ready(_)) => None,
    }
}

/// What a comment on a selection is being written with.
struct NoteEditor {
    input: Entity<TextInput>,
    _subscription: Subscription,
}

pub struct DiffCache {
    patches: HashMap<String, Patch>,
    list: ListState,
    /// The scrollbar's room at the list's right, which the rows were last measured without.
    gutter: f32,
    signatures: Vec<u64>,
    /// The list's width in code columns when its rows were last counted.
    estimated_columns: i32,
    selection: Option<Selection>,
    editing: Option<String>,
    note: Option<NoteEditor>,
    /// The file a gutter drag is picking lines in.
    dragging: Option<String>,
    /// Lines picked from the keyboard keep the caret in the gutter.
    keep_gutter: bool,
    note_focus_pending: bool,
    review: String,
    read_all: bool,
    read_all_timer: Option<gpui::Task<()>>,
    scanned: HashMap<String, Vec<ReviewHit>>,
    scanned_for: String,
    held: (String, Option<ReviewHit>, i64),
    reported: Option<(i64, Option<i64>, bool)>,
    stepped_to: Option<ReviewHit>,
    want_file: Option<String>,
    mode_probe: BoundsCell,
    compare_probe: BoundsCell,
    base_probe: BoundsCell,
    panel_probe: BoundsCell,
    pub toolbar_probe: BoundsCell,
    copied: Option<(bool, std::time::Instant)>,
    focus: Option<gpui::FocusHandle>,
    /// Each file row's and gutter's own keyboard stop, made as the row is first drawn.
    stops: Rc<RefCell<HashMap<String, gpui::FocusHandle>>>,
    /// The stops in the order the review draws them.
    stop_order: Vec<Stop>,
}

impl Default for DiffCache {
    fn default() -> Self {
        DiffCache {
            patches: HashMap::new(),
            list: ListState::new(0, ListAlignment::Top, px(600.0)),
            gutter: 0.0,
            signatures: Vec::new(),
            estimated_columns: 0,
            selection: None,
            editing: None,
            note: None,
            dragging: None,
            keep_gutter: false,
            note_focus_pending: false,
            review: String::new(),
            read_all: false,
            read_all_timer: None,
            scanned: HashMap::new(),
            scanned_for: String::new(),
            held: (String::new(), None, 0),
            reported: None,
            stepped_to: None,
            want_file: None,
            mode_probe: bounds_cell(),
            compare_probe: bounds_cell(),
            base_probe: bounds_cell(),
            panel_probe: bounds_cell(),
            toolbar_probe: bounds_cell(),
            copied: None,
            focus: None,
            stops: Rc::default(),
            stop_order: Vec::new(),
        }
    }
}

impl DiffCache {
    /// The review opens anew for another thread: at the top, with nothing picked or half-written.
    pub fn remount(&mut self, cx: &mut App) {
        // An empty list has no top to hold, and one set there would ride below rows spliced in later.
        if self.list.item_count() > 0 {
            self.list.scroll_to(ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        }
        self.selection = None;
        self.editing = None;
        if let Some(note) = &self.note {
            note.input.update(cx, |input, cx| input.reset("", cx));
        }
    }
}

fn version_key(workspace_id: &str, range: &DiffRange, ignore_whitespace: bool, file: &DiffFileSummary) -> String {
    format!("{workspace_id}|{}|{ignore_whitespace}|{}|{}", range_key(range), file.path, file_fingerprint(file))
}

fn event(event: WorkspaceEvent, cx: &mut App) {
    let workspace = crate::workspace::workspace(cx);
    workspace.update(cx, |workspace, cx| workspace.dispatch(WorkspaceInput::Event(event), cx));
}

/// What the review is about: the thread tab a coordinator is reviewing, else the dock's own thread.
struct Subject {
    task_id: Option<String>,
    workspace_id: Option<String>,
    branch: Option<String>,
    annotations: Vec<Annotation>,
}

fn subject(state: &WorkspaceState, owner: &str, diff: &DiffState) -> Subject {
    let reviewed = dock_subject(state, owner, act_core::state::DIFF_PANEL);
    let thread = match &reviewed {
        Some(id) => state.thread(id),
        None => state.current_thread(),
    };
    let own_workspace = match thread {
        Some(thread) => thread_workspace_id(state, Some(thread)),
        None => state.draft_project_id.as_ref().and_then(|id| state.project(id)).and_then(|project| project.workspace_id.clone()),
    };
    let workspace_id = diff.workspace_id.clone().or(own_workspace.clone());
    let environment = own_workspace.as_ref().and_then(|id| state.environments.get(id));
    let branch = match environment {
        Some(ChangedFilesResult::Available { branch, .. }) if diff.workspace_id.is_none() || diff.workspace_id == own_workspace => branch.clone(),
        _ => None,
    };
    // A new thread's notes are kept under the draft's own key until it is sent.
    let key = reviewed.clone().or_else(|| state.current_id.clone()).unwrap_or_else(|| act_core::state::NEW_THREAD_DRAFT.to_string());
    let annotations = state.annotations.get(&key).cloned().unwrap_or_default();
    Subject { task_id: reviewed, workspace_id, branch, annotations }
}

impl Root {
    fn read_patch(&mut self, key: String, workspace_id: String, range: DiffRange, ignore_whitespace: bool, file: DiffFileSummary, cx: &mut Context<Root>) {
        self.dock.diff.patches.insert(key.clone(), Patch::Reading);
        let read = window_desktop::diff_patch(workspace_id, &range, file.path.clone(), file.previous_path.clone(), ignore_whitespace, cx);
        cx.spawn(async move |root, cx| {
            let patch = match read.await {
                DiffPatchResult::Available { patch } => {
                    let parsed = cx.background_executor().spawn({
                        let path = file.path.clone();
                        async move {
                            let drawing = DrawnFile::new(parse_file_patch(&patch, &path));
                            drawing.colour_opening(OPENING_ROWS);
                            drawing
                        }
                    });
                    Patch::Ready(Rc::new(parsed.await))
                }
                DiffPatchResult::TooLarge { limit } => Patch::TooLarge(limit),
                DiffPatchResult::Error { message } => Patch::Failed(message),
            };
            let _ = root.update(cx, |root, cx| {
                if root.dock.diff.patches.contains_key(&key) {
                    root.dock.diff.patches.insert(key, patch);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn clear_comment(&mut self, cx: &mut Context<Root>) {
        let diff = &mut self.dock.diff;
        diff.selection = None;
        diff.editing = None;
        if let Some(note) = &diff.note {
            note.input.update(cx, |input, cx| input.reset("", cx));
        }
        cx.notify();
    }

    fn select_line(&mut self, path: &str, key: &str, extend: bool, from_keys: bool, cx: &mut Context<Root>) {
        let diff = &mut self.dock.diff;
        diff.keep_gutter = from_keys;
        if diff.editing.take().is_some() {
            if let Some(note) = &diff.note {
                note.input.update(cx, |input, cx| input.reset("", cx));
            }
        }
        let next = match &diff.selection {
            Some(current) if extend && current.path == path => Selection { head: key.to_string(), ..current.clone() },
            _ => Selection { path: path.to_string(), anchor: key.to_string(), head: key.to_string() },
        };
        let moved = diff.selection.as_ref().is_none_or(|current| current.path != next.path || current.anchor != next.anchor);
        diff.selection = Some(next);
        if moved && diff.dragging.is_none() && !from_keys {
            diff.note_focus_pending = true;
        }
        cx.notify();
    }

    fn note_input(&mut self, cx: &mut Context<Root>) -> Entity<TextInput> {
        if let Some(note) = &self.dock.diff.note {
            return note.input.clone();
        }
        let input = cx.new(|cx| TextInput::new(cx).multiline(Autosize { min_rows: 1, max_height: Some(px(180.0)) }).submit_on_enter(true).label("Note on the selected lines").placeholder("What should change here?"));
        let subscription = cx.subscribe(&input, |root, _, event: &InputEvent, cx| match event {
            InputEvent::Submit => root.submit_comment(cx),
            InputEvent::Changed => cx.notify(),
            _ => {}
        });
        self.dock.diff.note = Some(NoteEditor { input: input.clone(), _subscription: subscription });
        input
    }

    fn submit_comment(&mut self, cx: &mut Context<Root>) {
        let Some(input) = self.dock.diff.note.as_ref().map(|note| note.input.clone()) else { return };
        let note = input.read(cx).text().trim().to_string();
        if note.is_empty() {
            return;
        }
        let state = self.state(cx);
        let (owner, _) = act_core::state::front_dock(&state);
        let diff = diff_for(&state, &owner).clone();
        let subject = subject(&state, &owner, &diff);
        let Some(selection) = self.dock.diff.selection.clone() else { return };
        let drawn = self.drawn_files(&diff, subject.workspace_id.as_deref());
        let Some((from, to)) = span_of(&selection, &drawn) else { return };
        let Some(drawing) = drawn.get(&selection.path) else { return };
        let rows = &drawing.rows[from..=to];
        let side = selection_side(rows);
        let task_id = subject.task_id.clone();
        if let Some(editing) = self.dock.diff.editing.clone() {
            dispatch(AppCommand::AnnotationNote { task_id, annotation_id: editing, note }, cx);
        } else {
            let quote = comment_quote(&selection.path, rows, side);
            let anchor = AnnotationAnchor::Diff { comparison: range_key(&diff.range), path: selection.path.clone(), start: selection.anchor.clone(), end: selection.head.clone(), side };
            dispatch(AppCommand::AnnotationAdd { task_id, quote, note: Some(note), anchor: Some(anchor) }, cx);
        }
        self.clear_comment(cx);
    }

    fn drawn_files(&self, diff: &DiffState, workspace_id: Option<&str>) -> HashMap<String, Rc<DrawnFile>> {
        let mut drawn = HashMap::new();
        let (Some(workspace_id), Some(DiffSummaryResult::Available { files, range, ignore_whitespace, .. })) = (workspace_id, &diff.result) else { return drawn };
        for file in files {
            if let Some(Patch::Ready(drawing)) = self.dock.diff.patches.get(&version_key(workspace_id, range, *ignore_whitespace, file)) {
                drawn.insert(file.path.clone(), drawing.clone());
            }
        }
        drawn
    }

    /// The first file still to read after the one at the top, wrapping round.
    fn next_unviewed(&mut self, cx: &mut Context<Root>) {
        let state = self.state(cx);
        let (owner, _) = act_core::state::front_dock(&state);
        let diff = diff_for(&state, &owner);
        let Some(DiffSummaryResult::Available { files, .. }) = &diff.result else { return };
        let pinned = self.dock.diff.pinned_path();
        let from = pinned.and_then(|path| files.iter().position(|file| file.path == path)).map(|at| at as i64).unwrap_or(-1);
        let start = (from + 1) as usize;
        let order = files[start.min(files.len())..].iter().chain(files[..start.min(files.len())].iter());
        let Some(next) = order.into_iter().find(|file| !diff.viewed.contains_key(&file.path)) else { return };
        if diff.collapsed.contains(&next.path) {
            dispatch(AppCommand::DiffSetCollapsed { path: next.path.clone(), collapsed: false }, cx);
        }
        self.dock.diff.want_file = Some(next.path.clone());
        cx.notify();
    }

    /// Up and Down walk the file rows and gutters, keeping to a two-column gutter's side; with Shift they
    /// pick lines. Enter on a picked line goes to its note; Enter or Space on a gutter picks it.
    fn review_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Root>) {
        let keys = &event.keystroke;
        if keys.modifiers.platform || keys.modifiers.control || keys.modifiers.alt {
            return;
        }
        let order = self.dock.diff.stop_order.clone();
        let at = {
            let stops = self.dock.diff.stops.borrow();
            order.iter().position(|stop| stops.get(&stop.key).is_some_and(|handle| handle.is_focused(window)))
        };
        let Some(at) = at else { return };
        let here = order[at].clone();
        match keys.key.as_str() {
            "up" | "down" => {
                let column = here.line.as_ref().and_then(|line| line.2);
                let fits = |stop: &&Stop| column.is_none() || stop.line.as_ref().is_none_or(|line| line.2.is_none() || line.2 == column);
                let next = if keys.key == "down" { order[at + 1..].iter().find(fits) } else { order[..at].iter().rev().find(fits) };
                let Some(next) = next.cloned() else { return };
                cx.stop_propagation();
                if keys.modifiers.shift {
                    if let (Some((path, key, _, picked)), Some((next_path, next_key, _, _))) = (&here.line, &next.line) {
                        if path == next_path {
                            if !picked {
                                self.select_line(path, key, false, true, cx);
                            }
                            self.select_line(next_path, next_key, true, true, cx);
                        }
                    }
                }
                let handle = self.dock.diff.stops.borrow_mut().entry(next.key.clone()).or_insert_with(|| cx.focus_handle()).clone();
                window.focus(&handle);
                self.dock.diff.list.scroll_to_reveal_item(next.row);
                cx.notify();
            }
            "enter" | "space" if here.line.is_some() => {
                cx.stop_propagation();
                match &here.line {
                    Some((_, _, _, true)) if keys.key == "enter" => {
                        self.dock.diff.keep_gutter = false;
                        let note = self.note_input(cx);
                        note.update(cx, |input, _| input.focus(window));
                    }
                    Some((path, key, _, _)) => self.select_line(path, key, keys.modifiers.shift, false, cx),
                    // A file row is a button: its click answers the key.
                    None => {}
                }
            }
            _ => {}
        }
    }
}

impl DiffCache {
    fn pinned_path(&self) -> Option<String> {
        PINNED.with(|pinned| pinned.borrow().clone())
    }
}

thread_local! {
    /// The file under the review's top edge, as the last frame drew it.
    static PINNED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// A long name split so its end, which tells branches apart, stays in view as the chip narrows.
fn split_name(name: &str, suffix: &str) -> (String, String) {
    let chars: Vec<char> = name.chars().collect();
    let keep = 10;
    if chars.len() <= keep + 4 {
        return (format!("{name}{suffix}"), String::new());
    }
    let cut = chars.len() - keep;
    (chars[..cut].iter().collect(), format!("{}{suffix}", chars[cut..].iter().collect::<String>()))
}

fn mode_label(mode: DiffStateMode) -> &'static str {
    match mode {
        DiffStateMode::Uncommitted => "Uncommitted",
        DiffStateMode::Branch => "Branch",
        DiffStateMode::Commit => "Commit",
    }
}

/// The note above the list: a wait, a fact, a failure to try again, or spacing hidden away.
enum PanelNote {
    Wait(String),
    Fact(String),
    Error(String),
    Spacing(String),
}

fn summary_note(result: Option<&DiffSummaryResult>, loading: bool, workspace: bool, ignore_whitespace: bool) -> Option<PanelNote> {
    if !workspace {
        return Some(PanelNote::Fact(NO_PROJECT.into()));
    }
    let reading = || PanelNote::Wait("Reading the comparison…".into());
    match result {
        None => loading.then(reading),
        Some(DiffSummaryResult::Error { message }) => Some(if loading { reading() } else { PanelNote::Error(message.clone()) }),
        Some(DiffSummaryResult::Unknown { .. }) => Some(PanelNote::Fact("Workspace is no longer registered".into())),
        Some(DiffSummaryResult::Unavailable { reason }) => Some(PanelNote::Fact(format!("Workspace is {}", serde_json::to_value(reason).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default()))),
        Some(DiffSummaryResult::Available { files, .. }) if files.is_empty() => {
            let text = "Nothing has changed in this comparison".to_string();
            Some(if ignore_whitespace { PanelNote::Spacing(text) } else { PanelNote::Fact(text) })
        }
        Some(DiffSummaryResult::Available { .. }) => None,
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, owner: &str, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let mono = theme.typography.mono_family.clone();
    let diff = diff_for(state, owner).clone();
    let subject = subject(state, owner, &diff);
    let workspace_id = subject.workspace_id.clone();
    let weak = cx.entity().downgrade();

    // A comparison that changes is a different set of lines, so a picked range is gone.
    let review = format!("{owner}|{:?}|{}|{:?}", workspace_id, range_key(&diff.range), diff.mode);
    if root.dock.diff.review != review {
        root.dock.diff.review = review;
        root.dock.diff.selection = None;
        root.dock.diff.editing = None;
        root.dock.diff.read_all = false;
        root.dock.diff.read_all_timer = None;
        if let Some(note) = &root.dock.diff.note {
            note.input.update(cx, |input, cx| input.reset("", cx));
        }
    }

    let panel_width = root.dock.diff.panel_probe.get().map(|bounds| f32::from(bounds.size.width)).unwrap_or(620.0);
    let room_for_two = panel_width >= SPLIT_MIN_WIDTH;
    let split = diff.split && room_for_two;

    // Find over the review.
    let finding = find_view(state).filter(|view| matches!(&view.find.target, FindTarget::Review { owner: of } if of == owner));
    let needle = finding.as_ref().map(|view| view.find.query.trim().to_lowercase()).unwrap_or_default();
    if !needle.is_empty() && !root.dock.diff.read_all && root.dock.diff.read_all_timer.is_none() {
        root.dock.diff.read_all_timer = Some(cx.spawn(async |root, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
            let _ = root.update(cx, |root, cx| {
                root.dock.diff.read_all = true;
                cx.notify();
            });
        }));
    }

    let files: Vec<DiffFileSummary> = match &diff.result {
        Some(DiffSummaryResult::Available { files, .. }) => files.clone(),
        _ => Vec::new(),
    };
    let collapsed: HashSet<String> = diff.collapsed.iter().cloned().collect();

    // Reads every open file's patch, or every file's while a search wants them.
    let mut requested = 0;
    let mut settled_any = false;
    let mut drawn: HashMap<String, Rc<DrawnFile>> = HashMap::new();
    let mut versions: HashMap<String, String> = HashMap::new();
    if let (Some(workspace_id), Some(DiffSummaryResult::Available { range, ignore_whitespace, .. })) = (&workspace_id, &diff.result) {
        let live: HashSet<String> = files.iter().map(|file| version_key(workspace_id, range, *ignore_whitespace, file)).collect();
        root.dock.diff.patches.retain(|key, _| live.contains(key));
        for file in &files {
            let key = version_key(workspace_id, range, *ignore_whitespace, file);
            versions.insert(file.path.clone(), key.clone());
            if file.binary {
                continue;
            }
            let open = !collapsed.contains(&file.path);
            if open || root.dock.diff.read_all {
                if !root.dock.diff.patches.contains_key(&key) {
                    root.read_patch(key.clone(), workspace_id.clone(), range.clone(), *ignore_whitespace, file.clone(), cx);
                }
            }
            if open {
                requested += 1;
                match root.dock.diff.patches.get(&key) {
                    Some(Patch::Reading) | None => {}
                    Some(Patch::Ready(drawing)) => {
                        settled_any = true;
                        drawn.insert(file.path.clone(), drawing.clone());
                    }
                    Some(_) => settled_any = true,
                }
            }
        }
    }
    let settling = requested > 0 && !settled_any;

    // The selection and the comments on the drawn lines.
    let span = root.dock.diff.selection.as_ref().and_then(|selection| span_of(selection, &drawn).map(|span| (selection.path.clone(), span)));
    if root.dock.diff.selection.is_some() && span.is_none() && !drawn.is_empty() {
        let path = root.dock.diff.selection.as_ref().map(|selection| selection.path.clone()).unwrap_or_default();
        if drawn.contains_key(&path) {
            root.dock.diff.selection = None;
        }
    }
    let comparison = range_key(&diff.range);
    let comments = index_comments(anchored_comments(&subject.annotations, &comparison, &drawn));
    let dragging = root.dock.diff.dragging.is_some();

    // Rows.
    let mut rows: Vec<PanelRow> = Vec::new();
    if !settling {
        for file in &files {
            rows.push(PanelRow::File(file.path.clone()));
            if collapsed.contains(&file.path) {
                continue;
            }
            if file.binary {
                rows.push(PanelRow::Note(file.path.clone(), "Binary file".into()));
                continue;
            }
            let Some(drawing) = drawn.get(&file.path) else {
                let patch = versions.get(&file.path).and_then(|key| root.dock.diff.patches.get(key));
                rows.push(PanelRow::Note(file.path.clone(), patch_note(patch).unwrap_or_else(|| "Reading the patch…".into())));
                continue;
            };
            if drawing.rows.is_empty() {
                rows.push(PanelRow::Note(file.path.clone(), empty_patch_note(file)));
                continue;
            }
            let commenting = span.as_ref().filter(|(path, _)| *path == file.path && !dragging).map(|(_, (_, to))| *to);
            if split {
                for (index, pair) in drawing.pairs.iter().enumerate() {
                    rows.push(PanelRow::Pair(file.path.clone(), index));
                    if let (Some(to), SplitRow::Pair { left, right, .. }) = (commenting, pair) {
                        let reached = [left, right].iter().any(|side| side.as_ref().and_then(|row| drawing.index_by_key.get(&row.key)) == Some(&to));
                        if reached {
                            rows.push(PanelRow::Composer(file.path.clone()));
                        }
                    }
                }
            } else {
                for index in 0..drawing.rows.len() {
                    rows.push(PanelRow::Line(file.path.clone(), index));
                    if commenting == Some(index) {
                        rows.push(PanelRow::Composer(file.path.clone()));
                    }
                }
            }
        }
    }

    // Find: count over the data, hold the match being read, and bring it into view.
    let mut active_hit: Option<ReviewHit> = None;
    if let Some(view) = &finding {
        if root.dock.diff.scanned_for != needle {
            root.dock.diff.scanned = HashMap::new();
            root.dock.diff.scanned_for = needle.clone();
        }
        let all_patches: Vec<(DiffFileSummary, Option<Patch>)> = files.iter().map(|file| (file.clone(), versions.get(&file.path).and_then(|key| root.dock.diff.patches.get(key)).cloned())).collect();
        let seen: Vec<ReviewFile> = all_patches
            .iter()
            .map(|(file, patch)| ReviewFile {
                path: file.path.clone(),
                version: versions.get(&file.path).cloned().unwrap_or_default(),
                file: match patch {
                    Some(Patch::Ready(drawing)) if !file.binary => Some(&drawing.file),
                    _ => None,
                },
                coming: !file.binary && matches!(patch, None | Some(Patch::Reading)),
            })
            .collect();
        let count = review_hits(&seen, &needle, &mut root.dock.diff.scanned);
        let index = view.find.index;
        let held = &root.dock.diff.held;
        let anchor = (held.0 == needle && held.2 == index).then(|| held.1.clone()).flatten();
        let moved = anchor.as_ref().and_then(|anchor| count.hits.iter().position(|hit| hit == anchor));
        let at = moved.map(|at| at as i64).unwrap_or_else(|| index.min((count.hits.len() as i64 - 1).max(0)));
        root.dock.diff.held = (needle.clone(), count.hits.get(at as usize).cloned(), at);
        let report = (count.hits.len() as i64, (at != index).then_some(at), count.counting);
        if root.dock.diff.reported != Some(report) {
            root.dock.diff.reported = Some(report);
            let results = FindResults { matches: report.0, index: report.1, counting: report.2.then_some(true) };
            let target = FindTarget::Review { owner: owner.to_string() };
            cx.defer(move |cx| event(WorkspaceEvent::FindResults { target, results }, cx));
        }
        active_hit = (!needle.is_empty()).then(|| count.hits.get(at as usize).cloned()).flatten();
        if let Some(hit) = &active_hit {
            if root.dock.diff.stepped_to.as_ref() != Some(hit) {
                if hit.key.is_some() && collapsed.contains(&hit.path) {
                    let path = hit.path.clone();
                    cx.defer(move |cx| dispatch(AppCommand::DiffSetCollapsed { path, collapsed: false }, cx));
                }
                let target = rows.iter().position(|row| match (row, &hit.key) {
                    (PanelRow::File(path), None) => *path == hit.path,
                    (PanelRow::Line(path, index), Some(key)) => *path == hit.path && drawn.get(path).is_some_and(|drawing| drawing.rows[*index].key == *key),
                    (PanelRow::Pair(path, index), Some(key)) => {
                        *path == hit.path
                            && drawn.get(path).is_some_and(|drawing| match &drawing.pairs[*index] {
                                SplitRow::Pair { left, right, .. } => [left, right].iter().any(|side| side.as_ref().is_some_and(|row| row.key == *key)),
                                SplitRow::Hunk { key: hunk, .. } => hunk == key,
                            })
                    }
                    _ => false,
                });
                if let Some(target) = target {
                    root.dock.diff.stepped_to = Some(hit.clone());
                    root.dock.diff.list.scroll_to_reveal_item(target);
                }
            }
        }
    } else {
        root.dock.diff.reported = None;
        root.dock.diff.held = (String::new(), None, 0);
        root.dock.diff.stepped_to = None;
    }

    // Keep the list's measurements for every row that did not change.
    let viewed: HashSet<String> = diff.viewed.keys().cloned().collect();
    let selected_range = span.as_ref().map(|(path, (from, to))| (path.clone(), *from, *to));
    let signatures: Vec<u64> = rows
        .iter()
        .map(|row| {
            let mut hasher = DefaultHasher::new();
            match row {
                PanelRow::File(path) => {
                    ("f", path, collapsed.contains(path), viewed.contains(path)).hash(&mut hasher);
                }
                PanelRow::Note(path, text) => ("n", path, text).hash(&mut hasher),
                PanelRow::Line(path, index) | PanelRow::Pair(path, index) => {
                    (matches!(row, PanelRow::Pair(..)), path, index, versions.get(path)).hash(&mut hasher);
                    let rows_of: Vec<usize> = match row {
                        PanelRow::Line(_, index) => vec![*index],
                        _ => drawn
                            .get(path)
                            .and_then(|drawing| match drawing.pairs.get(*index) {
                                Some(SplitRow::Pair { left, right, .. }) => Some([left, right].iter().filter_map(|side| side.as_ref().and_then(|row| drawing.index_by_key.get(&row.key).copied())).collect()),
                                _ => None,
                            })
                            .unwrap_or_default(),
                    };
                    for at in rows_of {
                        let picked = selected_range.as_ref().is_some_and(|(of, from, to)| of == path && at >= *from && at <= *to);
                        (picked, comments.highlighted.contains(&(path.clone(), at)), comments.markers.get(&(path.clone(), at)).map(|list| list.iter().map(|comment| comment.annotation.note.clone()).collect::<Vec<_>>())).hash(&mut hasher);
                    }
                    if !needle.is_empty() {
                        (&needle, active_hit.as_ref().map(|hit| (&hit.path, &hit.key, hit.occurrence))).hash(&mut hasher);
                    }
                }
                PanelRow::Composer(path) => ("c", path, root.dock.diff.editing.is_some()).hash(&mut hasher),
            }
            hasher.finish()
        })
        .collect();
    let text_system = window.text_system();
    let ch = text_system.ch_advance(text_system.resolve_font(&gpui::font(mono.clone())), px(m.text_meta)).map(f32::from).unwrap_or(m.text_meta * 0.6);
    let list_width = panel_width - root.dock.diff.gutter;
    sync_list(&mut root.dock.diff, signatures, (list_width / ch).floor() as i32, |at| row_estimate(&rows[at], &drawn, list_width, ch));
    let mut stop_order = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let picked = |path: &str, key: &str| -> bool {
            let index = drawn.get(path).and_then(|drawing| drawing.index_by_key.get(key).copied());
            selected_range.as_ref().zip(index).is_some_and(|((of, from, to), index)| of == path && index >= *from && index <= *to)
        };
        match row {
            PanelRow::File(path) => stop_order.push(Stop { key: file_stop(path), row: at, line: None }),
            PanelRow::Line(path, index) => {
                if let Some(line) = drawn.get(path).map(|drawing| &drawing.rows[*index]).filter(|line| line.kind != DiffLineKind::Hunk) {
                    stop_order.push(Stop { key: gutter_stop(path, &line.key, None), row: at, line: Some((path.clone(), line.key.clone(), None, picked(path, &line.key))) });
                }
            }
            PanelRow::Pair(path, index) => {
                if let Some(SplitRow::Pair { left, right, .. }) = drawn.get(path).map(|drawing| &drawing.pairs[*index]) {
                    for (side, cell) in [(DiffSide::Old, left), (DiffSide::New, right)] {
                        if let Some(line) = cell.as_ref().filter(|line| line.kind != DiffLineKind::Hunk) {
                            stop_order.push(Stop { key: gutter_stop(path, &line.key, Some(side)), row: at, line: Some((path.clone(), line.key.clone(), Some(side), picked(path, &line.key))) });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let live: HashSet<&str> = stop_order.iter().map(|stop| stop.key.as_str()).collect();
    root.dock.diff.stops.borrow_mut().retain(|key, _| live.contains(key.as_str()));
    root.dock.diff.stop_order = stop_order;
    if let Some(path) = root.dock.diff.want_file.clone() {
        if let Some(index) = rows.iter().position(|row| matches!(row, PanelRow::File(of) if *of == path)) {
            root.dock.diff.want_file = None;
            root.dock.diff.list.scroll_to(ListOffset { item_ix: index, offset_in_item: px(0.0) });
        }
    }

    // The note being written goes to its field as soon as a run is first picked.
    let note_input = root.note_input(cx);
    if root.dock.diff.note_focus_pending && span.is_some() && !dragging {
        root.dock.diff.note_focus_pending = false;
        note_input.update(cx, |input, _| input.focus(window));
    }
    let quote = span.as_ref().and_then(|(path, (from, to))| drawn.get(path).map(|drawing| comment_quote(path, &drawing.rows[*from..=*to], selection_side(&drawing.rows[*from..=*to]))));

    // The pinned file row follows the list's top edge.
    let top = root.dock.diff.list.logical_scroll_top().item_ix;
    let pinned = rows.get(top).map(|row| row.path().to_string());
    let over_own_row = matches!(rows.get(top), Some(PanelRow::File(_)));
    PINNED.with(|cell| *cell.borrow_mut() = pinned.clone());

    let model = Rc::new(DiffModel {
        ch,
        rows,
        drawn,
        files: files.iter().map(|file| (file.path.clone(), file.clone())).collect(),
        collapsed,
        viewed,
        selected: selected_range,
        comments,
        tokens: t.clone(),
        text_meta: m.text_meta,
        text_control: m.text_control,
        syntax: super::markdown::style(&theme, super::markdown::Variant::Answer).syntax,
        root: weak.clone(),
        note: note_input.clone(),
        quote,
        editing: root.dock.diff.editing.is_some(),
        needle: needle.clone(),
        active: active_hit,
        mono: mono.clone(),
        searching: !needle.is_empty(),
        stops: root.dock.diff.stops.clone(),
        ring: act_ui::focus::keyboard_focus_visible(cx),
    });

    let toolbar = toolbar(root, &diff, workspace_id.is_some(), subject.branch.clone(), workspace_id.clone(), split, room_for_two, panel_width >= WIDE_TOOLBAR, subject.task_id.clone(), cx);

    let progress = match &diff.result {
        Some(DiffSummaryResult::Available { files, additions, deletions, .. }) if !files.is_empty() => {
            let done = files.iter().filter(|file| diff.viewed.contains_key(&file.path)).count();
            let lead = if done < files.len() {
                Button::new("diff-next-unviewed", format!("{done} of {} viewed", files.len()))
                    .variant(ButtonVariant::Soft)
                    .size(ButtonSize::Sm)
                    .child(ink_icon(IconName::ArrowDown, 12.0))
                    .tip(shortcut_tip("Next unviewed file", "N"))
                    .on_click(cx.listener(|root, _, _, cx| root.next_unviewed(cx)))
                    .into_any_element()
            } else {
                div()
                    .px(px(5.0))
                    .py(px(3.0))
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .text_color(t.success)
                    .font_weight(FontWeight(560.0))
                    .child(icon(IconName::Check, 12.0, t.success))
                    .child(if files.len() == 1 { "Viewed".to_string() } else { format!("All {} viewed", files.len()) })
                    .into_any_element()
            };
            Some(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .pl(px(7.0))
                    .pr(px(12.0))
                    .pb(px(7.0))
                    .text_color(t.quiet_strong)
                    .text_size(px(m.text_meta))
                    .child(lead)
                    .child(change_counts(*additions, *deletions, &t))
                    .child(div().absolute().left(px(12.0)).right(px(12.0)).bottom_0().child(Meter::new(done as f32 / files.len() as f32).size(MeterSize::Hair).tone(Tone::Success)))
                    .into_any_element(),
            )
        }
        _ => None,
    };

    let over_budget = folded_for_size(&files).iter().any(|path| diff.collapsed.contains(path));
    let note = summary_note(diff.result.as_ref(), diff.loading, workspace_id.is_some(), diff.ignore_whitespace).or_else(|| {
        if settling {
            Some(PanelNote::Wait("Reading the changes…".into()))
        } else if over_budget {
            Some(PanelNote::Fact("This review is large, so its biggest files start folded.".into()))
        } else {
            None
        }
    });
    let note = note.map(|note| match note {
        PanelNote::Error(message) => div()
            .mx(px(10.0))
            .my(px(6.0))
            .child(Notice::new("diff-error", message).tone(NoticeTone::Danger).action(Button::new("diff-retry", "Try again").size(ButtonSize::Sm).variant(ButtonVariant::Soft).on_click(|_, _, cx| dispatch(AppCommand::DiffRefresh, cx))))
            .into_any_element(),
        PanelNote::Wait(text) | PanelNote::Fact(text) | PanelNote::Spacing(text) => {
            let wait = text.starts_with("Reading");
            let spacing = text == "Nothing has changed in this comparison" && diff.ignore_whitespace;
            div()
                .mx(px(12.0))
                .my(px(8.0))
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(px(8.0))
                .gap_y(px(6.0))
                .text_color(t.quiet_strong)
                .text_size(px(m.text_meta))
                .line_height(relative(1.4))
                .when(wait, |row| row.child(Spinner::new("diff-wait").size(px(12.0)).color(t.quiet_strong)))
                .child(text)
                .when(spacing, |row| row.child(Button::new("diff-show-spacing", "Show spacing changes").size(ButtonSize::Sm).variant(ButtonVariant::Soft).on_click(|_, _, cx| dispatch(AppCommand::DiffSetIgnoreWhitespace { ignore: false }, cx))))
                .into_any_element()
        }
    });

    let item_model = model.clone();
    let count = model.rows.len();
    let gutter = f32::from(act_ui::components::scrollbar::list_gutter(&root.dock.diff.list));
    if gutter != root.dock.diff.gutter {
        root.dock.diff.gutter = gutter;
        root.dock.diff.list.remeasure();
    }
    // The list learns whether it overflows as it lays out, after this render.
    let laid_out = root.dock.diff.list.clone();
    let probe = root.dock.diff.panel_probe.clone();
    cx.on_next_frame(window, move |_, _, cx| {
        let resized = probe.get().is_some_and(|bounds| f32::from(bounds.size.width) != panel_width);
        if resized || f32::from(act_ui::components::scrollbar::list_gutter(&laid_out)) != gutter {
            cx.notify();
        }
    });
    let files_list = div()
        .size_full()
        .pr(px(gutter))
        .child(list(root.dock.diff.list.clone(), move |index, window, cx| render_row(&item_model, index, window, cx)).size_full());
    let pinned_row = pinned.as_ref().and_then(|path| model.files.get(path)).map(|file| {
        let t = &model.tokens;
        // On the glass sheet the echo frosts its own row under it but only dims code, as Chromium draws it.
        let strip = if t.page.a == 0.0 && t.glass_blur && !over_own_row {
            div().bg(t.glass_strip).shadow(vec![gpui::BoxShadow { color: t.line_glass.into(), offset: gpui::point(px(0.0), px(1.0)), blur_radius: px(0.0), spread_radius: px(0.0) }])
        } else {
            act_ui::style::glass(div(), t, act_ui::style::GlassKind::Strip, None, None)
        };
        div().absolute().top_0().left_0().right_0().occlude().child(strip.child(file_header(&model, file, true, window, cx)))
    });
    let drag_weak = weak.clone();
    let up_weak = weak.clone();
    let scroll = div()
        .id("diff-scroll")
        .relative()
        .flex_1()
        .min_h_0()
        .bg(t.code_surface)
        .font_family(mono)
        .text_size(px(m.text_meta))
        .line_height(px(20.0))
        .when(count > 0, |scroll| {
            // The pinned row is drawn over the scrollbar too, which shows through it.
            scroll.child(files_list).child(act_ui::components::scrollbar::Scrollbar::list("diff-scrollbar", &root.dock.diff.list)).children(pinned_row)
        })
        .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, window, cx| root.review_key(event, window, cx)))
        .on_mouse_up(MouseButton::Left, move |_, _, cx| {
            let _ = drag_weak.update(cx, |root, cx| {
                if root.dock.diff.dragging.take().is_some() {
                    root.dock.diff.note_focus_pending = true;
                    cx.notify();
                }
            });
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
            let _ = up_weak.update(cx, |root, cx| {
                if root.dock.diff.dragging.take().is_some() {
                    root.dock.diff.note_focus_pending = true;
                    cx.notify();
                }
            });
        });

    let focus = root.dock.diff.focus.get_or_insert_with(|| cx.focus_handle()).clone();
    let note_for_keys = note_input.clone();
    div()
        .id("diff-panel")
        .size_full()
        .flex()
        .flex_col()
        .track_focus(&focus)
        .child(bounds_probe(&root.dock.diff.panel_probe))
        .on_key_down(cx.listener(move |root, event: &gpui::KeyDownEvent, window, cx| {
            let keys = &event.keystroke;
            if keys.key == "n" && !keys.modifiers.platform && !keys.modifiers.control && !keys.modifiers.alt && !note_for_keys.read(cx).is_focused(window) {
                cx.stop_propagation();
                root.next_unviewed(cx);
            }
        }))
        .child(toolbar.relative().child(bounds_probe(&root.dock.diff.toolbar_probe)).when(progress.is_none(), |toolbar| toolbar.border_b_1().border_color(t.line_soft)).when(progress.is_some(), |toolbar| toolbar.pb(px(4.0))))
        .children(progress)
        .children(note)
        .child(scroll)
        .into_any_element()
}

fn sync_list(cache: &mut DiffCache, signatures: Vec<u64>, columns: i32, estimate: impl Fn(usize) -> f32) {
    let old = &cache.signatures;
    // Rows spliced into an empty list would push its top below them, so it opens at the top.
    if old.is_empty() && !signatures.is_empty() {
        cache.list.reset_estimated((0..signatures.len()).map(|at| px(estimate(at))));
        cache.signatures = signatures;
    } else if old.len() != signatures.len() || old != &signatures {
        let first = old.iter().zip(&signatures).position(|(a, b)| a != b).unwrap_or(old.len().min(signatures.len()));
        let last_old = old.len();
        let last_new = signatures.len();
        let tail = old.iter().rev().zip(signatures.iter().rev()).take_while(|(a, b)| a == b).count().min(last_old - first).min(last_new - first);
        cache.list.splice_estimated(first..last_old - tail, (first..last_new - tail).map(|at| px(estimate(at))));
        cache.signatures = signatures;
    }
    // Rows wrap again at a new width, so every row is counted afresh but the one the scroll holds by.
    if columns != cache.estimated_columns {
        cache.estimated_columns = columns;
        let count = cache.signatures.len();
        if count > 0 {
            let top = cache.list.logical_scroll_top().item_ix.min(count - 1);
            cache.list.splice_estimated(top + 1..count, (top + 1..count).map(|at| px(estimate(at))));
            cache.list.splice_estimated(0..top, (0..top).map(|at| px(estimate(at))));
        }
    }
}

/// What a row is counted at until the list measures it: its code wrapped at the width it will
/// have, so the scrollbar reckons with the whole review before it is drawn.
fn row_estimate(row: &PanelRow, drawn: &HashMap<String, Rc<DrawnFile>>, width: f32, ch: f32) -> f32 {
    // Code lines hang their wrapped lines by two columns; a hunk header does not.
    let code = |text: &str, room: f32| wrapped_lines(text, room, room - 2.0 * ch, ch) as f32 * ROW_HEIGHT;
    let hunk = |text: &str| HUNK_ROW_HEIGHT - ROW_HEIGHT + wrapped_lines(text, width - HUNK_INSET, width - HUNK_INSET, ch) as f32 * ROW_HEIGHT;
    match row {
        PanelRow::File(_) => FILE_ROW_HEIGHT,
        PanelRow::Line(path, index) => match drawn.get(path).and_then(|drawing| drawing.rows.get(*index)) {
            Some(row) if row.kind == DiffLineKind::Hunk => hunk(&row.text),
            Some(row) => code(&row.text, width - UNIFIED_CODE_INSET),
            None => ROW_HEIGHT,
        },
        PanelRow::Pair(path, index) => match drawn.get(path).and_then(|drawing| drawing.pairs.get(*index)) {
            Some(SplitRow::Hunk { text, .. }) => hunk(text),
            Some(SplitRow::Pair { left, right, .. }) => {
                [left, right].into_iter().flatten().map(|row| code(&row.text, width / 2.0 - SPLIT_CODE_INSET)).fold(ROW_HEIGHT, f32::max)
            }
            None => ROW_HEIGHT,
        },
        PanelRow::Note(..) | PanelRow::Composer(_) => ROW_HEIGHT,
    }
}

/// How many lines monospace `text` wraps to, `first` wide on its first line and `rest` after, breaking
/// where the text layout breaks it.
fn wrapped_lines(text: &str, first: f32, rest: f32, ch: f32) -> usize {
    let expanded;
    let text = if text.contains('\t') {
        expanded = expand_tabs(text).0;
        expanded.as_str()
    } else {
        text
    };
    let fits = |columns: usize, room: f32| columns as f32 * ch <= room + 0.001;
    if fits(text.chars().count(), first) {
        return 1;
    }
    let opportunities = gpui::break_opportunities(text);
    let (mut lines, mut start, mut candidate, mut started) = (1, 0, None, false);
    for (column, (index, c)) in text.char_indices().enumerate() {
        if c != ' ' && started && opportunities.binary_search(&index).is_ok() {
            candidate = Some(column);
        }
        started |= c != ' ';
        if !fits(column + 1 - start, if lines == 1 { first } else { rest }) && column > start && c != ' ' {
            start = candidate.take().unwrap_or(column);
            lines += 1;
        }
    }
    lines
}

fn change_counts(additions: i64, deletions: i64, t: &Tokens) -> impl IntoElement {
    div().flex().flex_none().gap(px(8.0)).font_weight(FontWeight(600.0)).child(div().text_color(t.success).child(format!("+{additions}"))).child(div().text_color(t.danger).child(format!("−{deletions}")))
}

/// A trigger in the toolbar: the word it leads with, then what it is set to.
fn trigger(id: &'static str, lead: &'static str, value: impl IntoElement, open: bool, disabled: bool, size: f32, t: &Tokens) -> gpui::Stateful<gpui::Div> {
    super::common::rimmed(div(), t, 7.0)
        .id(id)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, "").expanded(open).disabled(disabled))
        .relative()
        .flex()
        .items_center()
        .gap(px(5.0))
        .min_h(px(28.0))
        .rounded(px(7.0))
        .text_size(px(size))
        .map(|button| {
            if disabled {
                button.text_color(t.quiet).opacity(t.disabled_opacity)
            } else {
                button.cursor_pointer().bg(if open { t.surface_hover } else { t.control_soft }).text_color(if open { t.ink } else { t.ink_dim }).hover(|style| style.bg(t.surface_hover).text_color(t.ink))
            }
        })
        .child(div().flex_none().text_color(t.muted).text_size(px(12.0)).font_weight(FontWeight(560.0)).child(lead))
        .child(div().flex_shrink().min_w_0().overflow_hidden().child(value))
        .child(div().flex_none().child(icon(IconName::ChevronDown, 12.0, t.quiet_strong)))
}

#[allow(clippy::too_many_arguments)]
fn toolbar(root: &mut Root, diff: &DiffState, has_workspace: bool, current_branch: Option<String>, workspace_id: Option<String>, split: bool, room_for_two: bool, wide: bool, task_id: Option<String>, cx: &mut Context<Root>) -> gpui::Div {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let mode_probe = root.dock.diff.mode_probe.clone();
    let mode = diff.mode;
    let mode_open = root.menus.is_open(DIFF_MODE_MENU);
    let mut mode_trigger = trigger("diff-mode", "Review", div().child(mode_label(mode)), mode_open, !has_workspace, m.text_menu, &t)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("Review mode: {}", mode_label(mode))).expanded(mode_open).disabled(!has_workspace))
        .pl(px(9.0)).pr(px(7.0)).py(px(3.0)).child(bounds_probe(&mode_probe));
    if !has_workspace {
        mode_trigger = mode_trigger.tip(NO_PROJECT);
    }
    let mode_trigger = mode_trigger.on_click(cx.listener(move |root, _, window, cx| {
        if !has_workspace {
            return;
        }
        if root.menus.is_open(DIFF_MODE_MENU) {
            root.menus.close(cx);
            return;
        }
        let anchor = mode_probe.get().unwrap_or_default();
        let entries: Vec<MenuEntry> = [(DiffMode::Uncommitted, "Uncommitted", "What changed since the last commit"), (DiffMode::Branch, "Branch", "What one branch adds over another")]
            .into_iter()
            .map(|(value, label, description)| MenuItem::new(label).description(description).checked(DiffStateMode::from(value) == mode).on_select(move |_, cx| dispatch(AppCommand::DiffSetMode { mode: value }, cx)).into())
            .collect();
        let menu = Menu::build(entries, Placement::Anchor { anchor, align: Align::Start }, window, cx, |menu| menu.top_level().checkable(true).width(px(230.0).max(anchor.size.width)).exempt(&mode_probe));
        root.menus.show(DIFF_MODE_MENU, menu, cx);
    }));
    let mut lead = div().flex().items_center().gap(px(6.0)).min_w_0().child(mode_trigger);
    if let DiffRange::Commit { commit } = &diff.range {
        lead = lead.child(linked_commit(root, commit, cx));
    }

    let ignore = diff.ignore_whitespace;
    let split_value = diff.split;
    let loading = diff.loading;
    let actions = div()
        .flex_none()
        .flex()
        .gap(px(2.0))
        .child(
            Button::bare("diff-split")
                .icon(IconName::Columns2)
                .icon_size(16.0)
                .variant(ButtonVariant::Ghost)
                .toggle_tone(ToggleTone::Accent)
                .pressed(split)
                .name("Two columns")
                .disabled(!room_for_two)
                .tip(if room_for_two { "Two columns" } else { "Two columns need a wider panel" })
                .on_click(move |_, _, cx| {
                    if room_for_two {
                        dispatch(AppCommand::DiffSetSplit { split: !split_value }, cx)
                    }
                }),
        )
        .child(Button::bare("diff-whitespace").name("Hide spacing changes").icon(IconName::Pilcrow).icon_size(16.0).variant(ButtonVariant::Ghost).toggle_tone(ToggleTone::Accent).pressed(ignore).tip(if ignore { "Spacing changes hidden" } else { "Hide spacing changes" }).on_click(move |_, _, cx| dispatch(AppCommand::DiffSetIgnoreWhitespace { ignore: !ignore }, cx)))
        .child(
            div()
                .relative()
                .child(IconButton::new("diff-refresh", if loading { IconName::LoaderCircle } else { IconName::RefreshCw }, "Refresh changes").tip(if loading { "Refreshing…" } else { "Refresh" }).on_click(|_, _, cx| dispatch(AppCommand::DiffRefresh, cx)))
                .when(loading, |wrap| wrap.child(div().absolute().inset_0().flex().items_center().justify_center().bg(t.sunken).rounded(px(8.0)).child(Spinner::new("diff-refresh-spin").size(px(14.0)).color(t.muted)))),
        );

    let compare = (diff.mode == DiffStateMode::Branch).then(|| {
        let (base, head) = match &diff.range {
            DiffRange::Branches { base, compare } => (base.clone(), compare.clone().unwrap_or_default()),
            _ => match &diff.branch_range {
                Some(DiffRange::Branches { base, compare }) => (base.clone(), compare.clone().unwrap_or_default()),
                _ => ("HEAD".to_string(), String::new()),
            },
        };
        let mut from = side_picker(root, COMPARE_MENU, "Compare", "From", head.clone(), ("Working tree", ""), current_branch.clone(), workspace_id.clone(), base.clone(), head.clone(), true, cx);
        let into = side_picker(root, BASE_MENU, "Target branch", "Into", base.clone(), ("HEAD", "HEAD"), None, workspace_id.clone(), base.clone(), head.clone(), false, cx);
        from.style().flex_shrink = Some(4.0);
        div().flex().items_center().gap(px(5.0)).min_w_0().child(from.min_w_0()).child(div().flex_none().child(icon(IconName::ArrowRight, 14.0, t.muted))).child(into.flex_shrink().min_w_0())
    });
    let _ = task_id;
    let header = div().flex().flex_none().flex_col().gap(px(5.0)).px(px(10.0)).pt(px(7.0)).pb(px(8.0));
    match compare {
        Some(compare) if wide => header.pb(px(8.0)).child(div().flex().items_center().gap(px(8.0)).child(lead).child(div().flex_1().min_w_0().child(compare)).child(actions)),
        Some(compare) => header.child(div().flex().items_center().justify_between().gap(px(8.0)).min_h(px(28.0)).child(lead).child(actions)).child(compare),
        None => header.child(div().flex().items_center().justify_between().gap(px(8.0)).min_h(px(28.0)).child(lead).child(actions)),
    }
}

/// The reviewed commit's short name; a click copies the full hash and says so in place.
fn linked_commit(root: &mut Root, commit: &str, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let copied = root.dock.diff.copied.filter(|(_, at)| at.elapsed() < std::time::Duration::from_millis(1400)).map(|(ok, _)| ok);
    let tip = match copied {
        Some(true) => "Copied".to_string(),
        Some(false) => "Copy failed".to_string(),
        None => format!("Copy {commit}"),
    };
    let name = if copied.is_none() { format!("Copy commit {commit}") } else { tip.clone() };
    let full = commit.to_string();
    div()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, name))
        .id("diff-linked-commit")
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(5.0))
        .min_h(px(28.0))
        .pl(px(7.0))
        .pr(px(8.0))
        .rounded(px(7.0))
        .bg(t.control_soft)
        .text_color(t.ink_dim)
        .cursor_pointer()
        .hover(|style| style.bg(t.surface_hover))
        .child(icon(if copied == Some(true) { IconName::Check } else { IconName::GitCommitHorizontal }, 14.0, if copied == Some(true) { t.success } else { t.quiet_strong }))
        .child(div().font_family(mono).text_size(px(12.0)).child(commit.chars().take(7).collect::<String>()))
        .tip(tip)
        .on_click(cx.listener(move |root, _, _, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(full.clone()));
            root.dock.diff.copied = Some((true, std::time::Instant::now()));
            cx.notify();
            cx.spawn(async |root, cx| {
                cx.background_executor().timer(std::time::Duration::from_millis(1450)).await;
                let _ = root.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }))
        .into_any_element()
}

/// One end of a branch comparison and the list it is picked from.
#[allow(clippy::too_many_arguments)]
fn side_picker(root: &mut Root, id: &'static str, label: &'static str, role: &'static str, value: String, extra: (&'static str, &'static str), current_branch: Option<String>, workspace_id: Option<String>, base: String, head: String, compare_side: bool, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let probe = if compare_side { root.dock.diff.compare_probe.clone() } else { root.dock.diff.base_probe.clone() };
    let open = root.menus.is_open(id);
    let (head_part, tail_part) = if value.is_empty() {
        match &current_branch {
            Some(branch) => split_name(branch, " · working"),
            None => split_name("Working tree", ""),
        }
    } else if value == extra.1 {
        split_name(extra.0, "")
    } else {
        split_name(&value, "")
    };
    let description = if value.is_empty() { format!("Working tree{}", current_branch.as_ref().map(|branch| format!(" on {branch}")).unwrap_or_default()) } else { format!("{head_part}{tail_part}") };
    let disabled = workspace_id.is_none();
    let shown = div().flex().min_w_0().overflow_hidden().child(div().flex_shrink().min_w_0().ellipsis().child(head_part)).when(!tail_part.is_empty(), |shown| shown.child(div().flex_none().whitespace_nowrap().child(tail_part)));
    let button = trigger(id, role, shown, open, disabled, m.text_meta, &t)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::PopupButton, format!("{label}: {description}")).expanded(open).disabled(disabled))
        .min_w_0().px(px(8.0)).py(px(4.0)).child(bounds_probe(&probe)).tip(if disabled { NO_PROJECT.to_string() } else { format!("{label}: {description}") });
    let selected = value.clone();
    button.on_click(cx.listener(move |root, _, window, cx| {
        let Some(workspace_id) = workspace_id.clone() else { return };
        if root.menus.is_open(id) {
            root.menus.close(cx);
            return;
        }
        let anchor = probe.get().unwrap_or_default();
        let branches: Rc<RefCell<Option<Result<Branches, String>>>> = Rc::default();
        let pick = {
            let (base, head) = (base.clone(), head.clone());
            Rc::new(move |picked: String, cx: &mut App| {
                let range = if compare_side { DiffRange::Branches { base: base.clone(), compare: (!picked.is_empty()).then_some(picked) } } else { DiffRange::Branches { base: picked, compare: (!head.is_empty()).then(|| head.clone()) } };
                dispatch(AppCommand::DiffSetRange { range }, cx);
            })
        };
        let entries = {
            let branches = branches.clone();
            let selected = selected.clone();
            let pick = pick.clone();
            Rc::new(move |query: &str| branch_entries(branches.borrow().as_ref(), query, extra, &selected, pick.clone()))
        };
        let filter = entries.clone();
        let menu = Menu::build(entries(""), Placement::Anchor { anchor, align: Align::Start }, window, cx, |menu| menu.top_level().checkable(true).min_width(px(260.0)).exempt(&probe).title(if compare_side { "Compare" } else { "Target branch" }).searchable("Search branches", move |query, _| filter(query)));
        root.menus.show(id, menu.clone(), cx);
        let read = window_desktop::branches(workspace_id, cx);
        let weak_menu = menu.downgrade();
        cx.spawn(async move |_, cx| {
            *branches.borrow_mut() = Some(read.await);
            let _ = cx.update(|cx| {
                if let Some(menu) = weak_menu.upgrade() {
                    menu.update(cx, |menu, cx| {
                        let query = menu.query(cx);
                        menu.set_entries(entries(&query), cx);
                    });
                }
            });
        })
        .detach();
    }))
}

/// Matches a fragment anywhere in a name, with the exact name first.
fn match_branches(names: &[String], query: &str) -> Vec<String> {
    let typed = query.trim();
    let needle = typed.to_lowercase();
    if needle.is_empty() {
        return names.to_vec();
    }
    let mut matched: Vec<String> = names.iter().filter(|name| name.to_lowercase().contains(&needle)).cloned().collect();
    if let Some(exact) = matched.iter().position(|name| name == typed) {
        let name = matched.remove(exact);
        matched.insert(0, name);
    }
    matched
}

fn branch_entries(branches: Option<&Result<Branches, String>>, query: &str, extra: (&'static str, &'static str), selected: &str, pick: Rc<dyn Fn(String, &mut App)>) -> Vec<MenuEntry> {
    let mut entries = Vec::new();
    let option = |label: String, value: String| {
        let pick = pick.clone();
        let chosen = value == selected;
        MenuItem::new(label).checked(chosen).on_select(move |_, cx| pick(value.clone(), cx))
    };
    if !match_branches(&[extra.0.to_string()], query).is_empty() {
        entries.push(MenuEntry::Group { label: "Not a branch".into(), items: vec![option(extra.0.to_string(), extra.1.to_string())] });
    }
    match branches {
        None => entries.push(MenuEntry::Note { text: "Reading branches…".into(), danger: false }),
        Some(Err(message)) => entries.push(MenuEntry::Note { text: message.clone().into(), danger: true }),
        Some(Ok(branches)) => {
            let local = match_branches(&branches.branches, query);
            let remote = match_branches(&branches.remotes, query);
            if !local.is_empty() {
                entries.push(MenuEntry::Group { label: "Local".into(), items: local.into_iter().map(|name| option(name.clone(), name)).collect() });
            }
            if !remote.is_empty() {
                entries.push(MenuEntry::Group { label: "Remote".into(), items: remote.into_iter().map(|name| option(name.clone(), name)).collect() });
            }
            if entries.is_empty() {
                entries.push(MenuEntry::Note { text: "No branch matches".into(), danger: false });
            }
        }
    }
    entries
}

/// What every row of the review is drawn from, held once per frame.
struct DiffModel {
    rows: Vec<PanelRow>,
    drawn: HashMap<String, Rc<DrawnFile>>,
    files: HashMap<String, DiffFileSummary>,
    collapsed: HashSet<String>,
    viewed: HashSet<String>,
    selected: Option<(String, usize, usize)>,
    comments: CommentIndex,
    tokens: Tokens,
    text_meta: f32,
    /// The code font's `ch`, which wrapped code lines hang by twice.
    ch: f32,
    text_control: f32,
    syntax: act_markdown::SyntaxColors,
    root: WeakEntity<Root>,
    note: Entity<TextInput>,
    quote: Option<String>,
    editing: bool,
    needle: String,
    active: Option<ReviewHit>,
    mono: SharedString,
    searching: bool,
    stops: Rc<RefCell<HashMap<String, gpui::FocusHandle>>>,
    /// Whether focus came from the keyboard, so the focused stop draws its ring.
    ring: bool,
}

impl DiffModel {
    /// A row's keyboard stop, reached by Tab only when `tab` says so; the arrows reach the rest.
    fn stop(&self, key: String, tab: bool, cx: &mut App) -> gpui::FocusHandle {
        self.stops.borrow_mut().entry(key).or_insert_with(|| cx.focus_handle()).clone().tab_stop(tab)
    }

    fn picked(&self, path: &str, index: usize) -> bool {
        self.selected.as_ref().is_some_and(|(of, from, to)| of == path && index >= *from && index <= *to)
    }
}

fn status_glyph(status: DiffFileStatus) -> (IconName, &'static str) {
    match status {
        DiffFileStatus::Added => (IconName::FilePlus2, "New file"),
        DiffFileStatus::Untracked => (IconName::FilePlus2, "New, untracked"),
        DiffFileStatus::Deleted => (IconName::FileMinus2, "Deleted file"),
        DiffFileStatus::Renamed => (IconName::FileSymlink, "Moved file"),
        DiffFileStatus::Modified => (IconName::FilePen, "Changed file"),
    }
}

fn render_row(model: &Rc<DiffModel>, index: usize, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(row) = model.rows.get(index) else { return div().into_any_element() };
    let t = &model.tokens;
    match row {
        PanelRow::File(path) => match model.files.get(path) {
            Some(file) => file_header(model, file, false, window, cx).into_any_element(),
            None => div().into_any_element(),
        },
        PanelRow::Note(_, text) => div().pl(px(30.0)).pr(px(12.0)).py(px(6.0)).text_color(t.quiet).text_size(px(model.text_meta)).child(text.clone()).into_any_element(),
        PanelRow::Line(path, index) => {
            let Some(drawing) = model.drawn.get(path) else { return div().into_any_element() };
            line_row(model, path, drawing, *index, window, cx)
        }
        PanelRow::Pair(path, index) => {
            let Some(drawing) = model.drawn.get(path) else { return div().into_any_element() };
            match &drawing.pairs[*index] {
                SplitRow::Hunk { text, .. } => hunk_row(text, t),
                SplitRow::Pair { left, right, .. } => {
                    // Only a file's first line is reached by Tab: its old side, else its new one.
                    let first = first_line(drawing);
                    let index_of = |cell: &Option<DiffRow>| cell.as_ref().and_then(|row| drawing.index_by_key.get(&row.key).copied());
                    let left_first = first.is_some() && index_of(left) == first;
                    let right_first = first.is_some() && index_of(right) == first && !left_first;
                    div()
                        .flex()
                        .w_full()
                        .child(split_cell(model, path, drawing, left.as_ref(), DiffSide::Old, left_first, window, cx).border_r_1().border_color(t.line_soft))
                        .child(split_cell(model, path, drawing, right.as_ref(), DiffSide::New, right_first, window, cx))
                        .into_any_element()
                }
            }
        }
        PanelRow::Composer(_) => comment_editor(model, cx),
    }
}

/// The index of a file's first line that can be commented on.
fn first_line(drawing: &DrawnFile) -> Option<usize> {
    drawing.rows.iter().position(|row| row.kind != DiffLineKind::Hunk)
}

fn hunk_row(text: &str, t: &Tokens) -> AnyElement {
    div().w_full().px(px(10.0)).py(px(2.0)).bg(t.diff_hunk_surface).text_color(t.diff_hunk_ink).child(text.to_string()).into_any_element()
}

/// The row a file is headed by: what happened to it, what it cost, and whether it has been read.
fn file_header(model: &Rc<DiffModel>, file: &DiffFileSummary, echo: bool, window: &mut Window, cx: &mut App) -> gpui::Stateful<gpui::Div> {
    let t = &model.tokens;
    let open = !model.collapsed.contains(&file.path);
    let viewed = model.viewed.contains(&file.path);
    let (folder, name) = match file.path.rfind('/') {
        Some(cut) => (file.path[..=cut].to_string(), file.path[cut + 1..].to_string()),
        None => (String::new(), file.path.clone()),
    };
    let (glyph, tip) = status_glyph(file.status);
    let id_base = format!("{}{}", if echo { "pin-" } else { "" }, file.path);
    let editor_focus = window.use_keyed_state(SharedString::from(format!("file-editor-focus-{id_base}")), cx, |_, cx| cx.focus_handle().tab_stop(true)).read(cx).clone();
    let group: SharedString = format!("diff-file-{id_base}").into();
    let toggle = {
        let root = model.root.clone();
        let path = file.path.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = root.update(cx, |_, cx| dispatch(AppCommand::DiffSetCollapsed { path: path.clone(), collapsed: open }, cx));
        }
    };
    let open_path = file.path.clone();
    let tick = {
        let root = model.root.clone();
        let path = file.path.clone();
        let searching = model.searching;
        move |_: &bool, _: &mut Window, cx: &mut App| {
            let path = path.clone();
            let _ = root.update(cx, |root, cx| {
                if !viewed && !searching {
                    let state = root.state(cx);
                    let (owner, _) = act_core::state::front_dock(&state);
                    let diff = diff_for(&state, &owner);
                    if let Some(DiffSummaryResult::Available { files, .. }) = &diff.result {
                        let from = files.iter().position(|file| file.path == path).unwrap_or(0);
                        root.dock.diff.want_file = files[from + 1..].iter().find(|file| !diff.viewed.contains_key(&file.path)).map(|file| file.path.clone());
                    }
                }
                dispatch(AppCommand::DiffSetViewed { path, viewed: !viewed }, cx);
            });
        }
    };
    let name_ink = if viewed { t.quiet } else { t.ink_dim };
    // The pinned echo is for the pointer only; the list's own row holds the keyboard.
    let stop = (!echo).then(|| model.stop(file_stop(&file.path), true, cx));
    // The pinned echo covers the list's own row, so it rings for it.
    let ring = model.ring && model.stops.borrow().get(&file_stop(&file.path)).is_some_and(|stop| stop.is_focused(window));
    div()
        .id(SharedString::from(format!("file-{id_base}")))
        .group(group.clone())
        .w_full()
        .flex()
        .items_center()
        .gap(px(2.0))
        .pl(px(2.0))
        .pr(px(8.0))
        .py(px(4.0))
        .border_t_1()
        .border_color(t.line)
        .when(!echo, |row| row.bg(t.dock_sunken))
        // A see-through echo keeps its fill under the pointer.
        .when(!(echo && t.page.a == 0.0 && t.glass_blur), |row| row.hover(|style| style.bg(t.surface_hover)))
        .child(
            div()
                .id(SharedString::from(format!("file-open-{id_base}")))
                .when_some(stop, |row, stop| row.track_focus(&stop))
                .relative()
                .when(ring, |row| row.child(div().absolute().inset_0().rounded(px(6.0)).border_2().border_color(t.focus_ring)))
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(4.0))
                .py(px(5.0))
                .cursor_pointer()
                .text_color(name_ink)
                .text_size(px(model.text_meta))
                // The pinned echo sits outside the list's 20px lines, so its name sets no taller than its icon.
                .when(echo, |row| row.line_height(px(16.0)))
                .child(div().id(SharedString::from(format!("file-caret-{id_base}"))).flex_none().child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 14.0, t.quiet_strong)).tip(if open { "Fold" } else { "Unfold" }))
                .child(div().id(SharedString::from(format!("file-icon-{id_base}"))).flex_none().child(icon(glyph, 16.0, t.quiet_strong)).tip(tip))
                .child(
                    div()
                        .id(SharedString::from(format!("file-name-{id_base}")))
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .font_family(model.mono.clone())
                        .child(div().flex_shrink().min_w_0().ellipsis().text_color(t.quiet).child(highlighted(&model.needle, &folder, t)))
                        .child(div().flex_none().whitespace_nowrap().child(highlighted(&model.needle, &name, t)))
                        .tip(file.path.clone()),
                )
                .when_some(file.previous_path.clone(), |row, previous| row.child(div().id(SharedString::from(format!("file-renamed-{id_base}"))).flex_none().px(px(6.0)).rounded(px(5.0)).bg(t.chip).text_size(px(11.0)).text_color(t.muted_strong).child("renamed").tip(previous)))
                // The echo stands outside the code font, so its counts set in the interface's.
                .when(!file.binary && !echo, |row| row.child(change_counts(file.additions, file.deletions, t)))
                .when(!file.binary && echo, |row| row.child(act_ui::style::tabular_nums(div().flex_none().font_family(cx.theme().typography.ui_family.clone())).child(change_counts(file.additions, file.deletions, t))))
                .on_click(toggle),
        )
        .child(
            // Drawn clear rather than left out, so the keyboard still reaches it.
            div()
                .id(SharedString::from(format!("file-editor-slot-{id_base}")))
                .w(px(28.0))
                .flex_none()
                .transition(gpui::Transition::ease_out(110).only(&[gpui::TransitionProperty::Opacity]))
                .when(!editor_focus.is_focused(window), |slot| slot.opacity(0.0).group_hover(group, |style| style.opacity(1.0)))
                .child(
                IconButton::new(SharedString::from(format!("file-editor-{id_base}")), IconName::SquareArrowOutUpRight, format!("Open {} in your editor", file.path))
                    .track_focus(&editor_focus)
                    .tip("Open in editor")
                    .icon_size(16.0)
                    .on_click(move |_, _, cx| dispatch(AppCommand::FileOpen { task_id: None, path: open_path.clone(), line: None }, cx)),
            ),
        )
        .child(
            div().flex_none().child(
                Checkbox::new(SharedString::from(format!("file-viewed-{id_base}")), viewed)
                    .name(format!("Mark {} viewed", file.path))
                    .tip(if viewed { "Mark unviewed" } else { "Mark viewed" })
                    .on_change(tick),
            ),
        )
}

/// Expands tabs to the next stop of eight, keeping a map from source bytes to drawn bytes.
fn expand_tabs(text: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    let mut column = 0;
    for ch in text.chars() {
        for _ in 0..ch.len_utf8() {
            map.push(out.len());
        }
        if ch == '\t' {
            let spaces = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            out.push(ch);
            column += 1;
        }
    }
    map.push(out.len());
    (out, map)
}

/// A line's text, coloured by its tokens, with the find matches painted over it.
fn code_text(model: &DiffModel, drawing: &Rc<DrawnFile>, path: &str, row: &DiffRow, cx: &mut App) -> StyledText {
    drawing.colour(&row.key, &model.root, cx);
    let t = &model.tokens;
    let (text, map) = expand_tabs(&row.text);
    let plain: Hsla = t.code_ink.into();
    let font = gpui::Font { family: model.mono.clone(), ..gpui::font(model.mono.clone()) };
    let mut colours: Vec<(Range<usize>, Hsla)> = Vec::new();
    if let Some(tokens) = drawing.tokens.borrow().get(&row.key) {
        for (range, role) in tokens {
            let start = map.get(range.start).copied().unwrap_or(text.len());
            let end = map.get(range.end).copied().unwrap_or(text.len());
            if start < end {
                colours.push((start..end, model.syntax.color(*role, plain)));
            }
        }
    }
    let mut marks: Vec<(Range<usize>, Rgba)> = Vec::new();
    if !model.needle.is_empty() {
        let active = model.active.as_ref().filter(|hit| hit.path == path && hit.key.as_deref() == Some(row.key.as_str())).map(|hit| hit.occurrence);
        for (occurrence, start) in match_offsets(&row.text, &model.needle, 500).into_iter().enumerate() {
            let end = (start + model.needle.len()).min(row.text.len());
            let (Some(&from), Some(&to)) = (map.get(start), map.get(end)) else { continue };
            marks.push((from..to, if active == Some(occurrence) { t.find_active } else { t.find_match }));
        }
    }
    let display = if text.is_empty() { " ".to_string() } else { text };
    let runs = runs_for(&display, &colours, &marks, plain, &font);
    StyledText::new(display).with_runs(runs)
}

/// Text runs over `text`: syntax colour first, then find marks on top.
fn runs_for(text: &str, colours: &[(Range<usize>, Hsla)], marks: &[(Range<usize>, Rgba)], plain: Hsla, font: &gpui::Font) -> Vec<TextRun> {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (range, _) in colours {
        cuts.push(range.start.min(text.len()));
        cuts.push(range.end.min(text.len()));
    }
    for (range, _) in marks {
        cuts.push(range.start.min(text.len()));
        cuts.push(range.end.min(text.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut runs = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let color = colours.iter().rev().find(|(range, _)| range.start <= start && range.end >= end).map(|(_, color)| *color).unwrap_or(plain);
        let background = marks.iter().find(|(range, _)| range.start <= start && range.end >= end).map(|(_, fill)| Hsla::from(*fill));
        runs.push(TextRun { len: end - start, font: font.clone(), color, background_color: background, underline: None, strikethrough: None });
    }
    if runs.is_empty() {
        runs.push(TextRun { len: text.len(), font: font.clone(), color: plain, background_color: None, underline: None, strikethrough: None });
    }
    runs
}

/// Plain text with the find matches painted, for a file's name.
fn highlighted(needle: &str, text: &str, t: &Tokens) -> AnyElement {
    if needle.is_empty() || text.is_empty() {
        return div().child(text.to_string()).into_any_element();
    }
    let lower = text.to_lowercase();
    if !lower.contains(needle) {
        return div().child(text.to_string()).into_any_element();
    }
    let mut row = div().flex();
    let mut at = 0;
    for start in match_offsets(text, needle, 50) {
        if start < at || !text.is_char_boundary(start) {
            continue;
        }
        let end = (start + needle.len()).min(text.len());
        if !text.is_char_boundary(end) {
            continue;
        }
        if start > at {
            row = row.child(text[at..start].to_string());
        }
        row = row.child(div().bg(t.find_match).child(text[start..end].to_string()));
        at = end;
    }
    if at < text.len() {
        row = row.child(text[at..].to_string());
    }
    row.into_any_element()
}

/// The numbered marks a commented line carries, each of which opens its note again.
fn comment_markers(model: &DiffModel, path: &str, index: usize, side: Option<DiffSide>, row: &DiffRow) -> Option<AnyElement> {
    let comments: Vec<DiffComment> = model.comments.markers.get(&(path.to_string(), index))?.iter().filter(|comment| side.is_none_or(|side| comment.side == side)).cloned().collect();
    if comments.is_empty() {
        return None;
    }
    let t = &model.tokens;
    let mut markers = div().absolute().top(px(1.0)).left(px(1.0)).flex().gap(px(2.0));
    for comment in comments {
        let tip = {
            let text = comment.annotation.note.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                "Edit comment".to_string()
            } else if text.chars().count() > 140 {
                format!("{}…", text.chars().take(139).collect::<String>())
            } else {
                text
            }
        };
        let root = model.root.clone();
        let anchor = comment.annotation.anchor.clone();
        let id = comment.annotation.id.clone();
        let note = comment.annotation.note.clone();
        markers = markers.child(
            div()
                .id(SharedString::from(format!("diff-marker-{}-{}", comment.annotation.id, row.key)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Edit comment {} on {}", comment.number, row_label(row).to_lowercase())))
                .min_w(px(18.0))
                .h(px(18.0))
                .px(px(5.0))
                .rounded_full()
                .bg(t.info_badge)
                .text_color(t.badge_ink)
                .text_size(px(11.0))
                .font_weight(FontWeight(650.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.shadow(vec![gpui::BoxShadow { color: t.info_line.into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(2.0) }]))
                .child(comment.number.to_string())
                .tip(tip)
                .on_click(move |_, _, cx| {
                    let Some(AnnotationAnchor::Diff { path, start, end, .. }) = anchor.clone() else { return };
                    let (id, note) = (id.clone(), note.clone());
                    let _ = root.update(cx, |root, cx| {
                        root.dock.diff.selection = Some(Selection { path, anchor: start, head: end });
                        root.dock.diff.editing = Some(id);
                        root.dock.diff.note_focus_pending = true;
                        let input = root.note_input(cx);
                        input.update(cx, |input, cx| input.reset(&note, cx));
                        cx.notify();
                    });
                }),
        );
    }
    Some(markers.into_any_element())
}

/// How a line is named to assistive technology.
fn row_label(row: &DiffRow) -> String {
    let number = |line: Option<i64>| line.map(|line| line.to_string()).unwrap_or_default();
    match row.kind {
        DiffLineKind::Add => format!("Added line {}", number(row.new_line)),
        DiffLineKind::Delete => format!("Removed line {}", number(row.old_line)),
        _ => format!("Unchanged line {}", number(row.new_line)),
    }
}

/// The gutter: the comment affordance and the line numbers. Pressing it picks lines; dragging picks more.
fn gutter(model: &DiffModel, path: &str, row: &DiffRow, numbers: Vec<Option<i64>>, side: Option<DiffSide>, first: bool, picked: bool, commented: bool, id: String, window: &Window, cx: &mut App) -> AnyElement {
    let t = &model.tokens;
    let split = side.is_some();
    let stop = model.stop(gutter_stop(path, &row.key, side), first, cx);
    let ring = model.ring && stop.is_focused(window);
    let changed = matches!(row.kind, DiffLineKind::Add | DiffLineKind::Delete);
    let fill = if picked {
        Some(t.diff_selected_line)
    } else if commented {
        Some(t.info_surface_soft)
    } else {
        match row.kind {
            DiffLineKind::Add => Some(t.diff_add_gutter),
            DiffLineKind::Delete => Some(t.diff_del_gutter),
            _ => None,
        }
    };
    let ink = if picked { t.ink } else if commented { t.muted } else if changed { t.quiet_strong } else { t.diff_gutter_ink };
    let hover_group: SharedString = format!("line-gutter-{id}").into();
    let (width, pad) = if split { (36.0, 6.0) } else { (38.0, 8.0) };
    let press_root = model.root.clone();
    let move_root = model.root.clone();
    let (press_path, press_key) = (path.to_string(), row.key.clone());
    let (move_path, move_key) = (path.to_string(), row.key.clone());
    div()
        .id(SharedString::from(id))
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, format!("Add comment on {}", row_label(row).to_lowercase())))
        .track_focus(&stop)
        .relative()
        .flex_none()
        .flex()
        .items_start()
        .pl(px(if split { 18.0 } else { 20.0 }))
        .when(ring, |gutter| gutter.child(div().absolute().inset_0().border_2().border_color(t.focus_ring)))
        .when_some(fill, |gutter, fill| gutter.bg(fill))
        .when(commented && !picked, |gutter| gutter.child(div().absolute().left_0().top_0().bottom_0().w(px(2.0)).bg(t.info_line)))
        .text_color(ink)
        .cursor_pointer()
        .when(!picked && !commented && !changed, |gutter| gutter.hover(|style| style.text_color(t.ink_dim)))
        .children(numbers.into_iter().map(|number| div().w(px(width)).pr(px(pad)).flex().justify_end().child(number.map(|number| number.to_string()).unwrap_or_default())))
        .when(!commented, |gutter| {
            gutter.child(
                div()
                    .absolute()
                    .top(px(2.0))
                    .left(px(2.0))
                    .size(px(16.0))
                    .rounded_full()
                    .border_1()
                    .border_color(t.info_line)
                    .bg(t.info_surface)
                    .flex()
                    .items_center()
                    .justify_center()
                    .invisible()
                    .group_hover(hover_group, |style| style.visible())
                    .child(icon(IconName::MessageSquarePlus, 12.0, t.info_soft)),
            )
        })
        .tip("Add comment")
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let (path, key) = (press_path.clone(), press_key.clone());
            let shift = event.modifiers.shift;
            let _ = press_root.update(cx, |root, cx| {
                root.select_line(&path, &key, shift, false, cx);
                if !shift {
                    root.dock.diff.dragging = Some(path);
                }
            });
        })
        .on_mouse_move(move |event, _, cx| {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let (path, key) = (move_path.clone(), move_key.clone());
            let _ = move_root.update(cx, |root, cx| {
                if root.dock.diff.dragging.as_deref() == Some(path.as_str()) && root.dock.diff.selection.as_ref().is_some_and(|selection| selection.head != key) {
                    root.select_line(&path, &key, true, false, cx);
                }
            });
        })
        .into_any_element()
}

fn line_row(model: &DiffModel, path: &str, drawing: &Rc<DrawnFile>, index: usize, window: &Window, cx: &mut App) -> AnyElement {
    let t = &model.tokens;
    let row = &drawing.rows[index];
    if row.kind == DiffLineKind::Hunk {
        return hunk_row(&row.text, t);
    }
    let picked = model.picked(path, index);
    let commented = model.comments.highlighted.contains(&(path.to_string(), index));
    let fill = if picked {
        Some(t.diff_selected)
    } else {
        match row.kind {
            DiffLineKind::Add => Some(t.diff_add_surface),
            DiffLineKind::Delete => Some(t.diff_del_surface),
            _ => None,
        }
    };
    let changed = matches!(row.kind, DiffLineKind::Add | DiffLineKind::Delete);
    let marker = match row.kind {
        DiffLineKind::Add => "+",
        DiffLineKind::Delete => "−",
        _ => " ",
    };
    let id = format!("{path}:{}", row.key);
    let line_group: SharedString = format!("line-gutter-{id}").into();
    let drag_root = model.root.clone();
    let (drag_path, drag_key) = (path.to_string(), row.key.clone());
    div()
        .id(SharedString::from(format!("line-{id}")))
        .group(line_group)
        .relative()
        .w_full()
        .flex()
        .min_h(px(20.0))
        .when_some(fill, |line, fill| line.bg(fill))
        .child(gutter(model, path, row, vec![row.old_line, row.new_line], None, first_line(drawing) == Some(index), picked, commented, id.clone(), window, cx))
        .child(div().flex_none().w(px(14.0)).text_center().text_color(if changed { t.muted } else { t.diff_gutter_ink }).child(marker))
        .child(div().flex_1().min_w_0().pr(px(12.0)).pl(px(2.0 * model.ch)).text_indent(px(-2.0 * model.ch)).child(code_text(model, drawing, path, row, cx)))
        .children(comment_markers(model, path, index, None, row))
        .on_mouse_move(move |event, _, cx| {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let (path, key) = (drag_path.clone(), drag_key.clone());
            let _ = drag_root.update(cx, |root, cx| {
                if root.dock.diff.dragging.as_deref() == Some(path.as_str()) && root.dock.diff.selection.as_ref().is_some_and(|selection| selection.head != key) {
                    root.select_line(&path, &key, true, false, cx);
                }
            });
        })
        .into_any_element()
}

fn split_cell(model: &DiffModel, path: &str, drawing: &Rc<DrawnFile>, row: Option<&DiffRow>, side: DiffSide, first: bool, window: &Window, cx: &mut App) -> gpui::Stateful<gpui::Div> {
    let t = &model.tokens;
    let side_name = if side == DiffSide::Old { "old" } else { "new" };
    let Some(row) = row.filter(|row| row.kind != DiffLineKind::Hunk) else {
        return div()
            .id(SharedString::from(format!("cell-empty-{path}-{side_name}-{:p}", drawing)))
            .flex_1()
            .min_w_0()
            
            .bg(t.diff_absent)
            .child(stripes(t));
    };
    let index = drawing.index_by_key.get(&row.key).copied().unwrap_or(0);
    let picked = model.picked(path, index);
    let commented = model.comments.highlighted.contains(&(path.to_string(), index));
    let fill = if picked {
        Some(t.diff_selected)
    } else {
        match row.kind {
            DiffLineKind::Add => Some(t.diff_add_surface),
            DiffLineKind::Delete => Some(t.diff_del_surface),
            _ => None,
        }
    };
    let number = if side == DiffSide::Old { row.old_line } else { row.new_line };
    let id = format!("{path}:{}:{side_name}", row.key);
    let group: SharedString = format!("line-gutter-{id}").into();
    let drag_root = model.root.clone();
    let (drag_path, drag_key) = (path.to_string(), row.key.clone());
    div()
        .id(SharedString::from(format!("cell-{id}")))
        .group(group)
        .relative()
        .flex_1()
        .min_w_0()
        
        .flex()
        .min_h(px(20.0))
        .when_some(fill, |cell, fill: Rgba| cell.bg(fill))
        .child(gutter(model, path, row, vec![number], Some(side), first, picked, commented, id.clone(), window, cx))
        .child(div().flex_1().min_w_0().pl(px(2.0 * model.ch)).pr(px(8.0)).text_indent(px(-2.0 * model.ch)).child(code_text(model, drawing, path, row, cx)))
        .children(comment_markers(model, path, index, Some(side), row))
        .on_mouse_move(move |event, _, cx| {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let (path, key) = (drag_path.clone(), drag_key.clone());
            let _ = drag_root.update(cx, |root, cx| {
                if root.dock.diff.dragging.as_deref() == Some(path.as_str()) && root.dock.diff.selection.as_ref().is_some_and(|selection| selection.head != key) {
                    root.select_line(&path, &key, true, false, cx);
                }
            });
        })
}

/// The faint diagonal hatching an absent side of a two-column row is drawn with.
fn stripes(t: &Tokens) -> impl IntoElement {
    let ink = with_alpha(t.ink, 0.03);
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let mut x = bounds.origin.x - bounds.size.height;
            let end = bounds.origin.x + bounds.size.width;
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                while x < end {
                    let mut path = gpui::PathBuilder::stroke(px(1.0));
                    path.move_to(gpui::point(x, bounds.origin.y + bounds.size.height));
                    path.line_to(gpui::point(x + bounds.size.height, bounds.origin.y));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, ink);
                    }
                    x += px(7.0);
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

/// Where the note on a picked run is written: the lines it names, the note, one way to send.
fn comment_editor(model: &DiffModel, cx: &mut App) -> AnyElement {
    let t = &model.tokens;
    let note_empty = model.note.read(cx).text().trim().is_empty();
    let editing = model.editing;
    let range = model.quote.as_ref().and_then(|quote| quote.lines().next().map(String::from)).unwrap_or_default();
    let clear_root = model.root.clone();
    let remove_root = model.root.clone();
    let submit_root = model.root.clone();
    let escape_root = model.root.clone();
    let card = rimmed(div().pane(t.glass_control), t, 14.0)
        .elevation_glint(&t.shadow_composer, t, act_ui::style::Corners::Radius(14.0))
        .mt(px(8.0))
        .mr(px(12.0))
        .mb(px(10.0))
        .ml(px(44.0))
        .rounded(px(14.0))
        .elevation(&t.shadow_composer)
        .font_family(".SystemUIFont")
        .on_key_down(move |event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
                let _ = escape_root.update(cx, |root, cx| root.clear_comment(cx));
            }
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .pt(px(8.0))
                .pr(px(8.0))
                .pl(px(12.0))
                .text_color(t.quiet_strong)
                .child(icon(IconName::MessageSquarePlus, 14.0, t.quiet_strong))
                .child(div().flex_1().min_w_0().ellipsis().text_color(t.muted).font_family(model.mono.clone()).text_size(px(model.text_meta)).child(range))
                .when(editing, |head| {
                    head.child(ConfirmAction::new("diff-comment-remove", "", "Remove", move |_, cx| {
                        let _ = remove_root.update(cx, |root, cx| {
                            if let Some(id) = root.dock.diff.editing.clone() {
                                let state = root.state(cx);
                                let (owner, _) = act_core::state::front_dock(&state);
                                let task_id = dock_subject(&state, &owner, act_core::state::DIFF_PANEL);
                                dispatch(AppCommand::AnnotationRemove { task_id, annotation_id: id }, cx);
                            }
                            root.clear_comment(cx);
                        });
                    })
                    .icon(IconName::Trash2)
                    .variant(ButtonVariant::Secondary)
                    .size(ButtonSize::Sm)
                    .tip("Remove comment"))
                })
                .child(IconButton::new("diff-comment-clear", IconName::X, "Clear the selection").size(IconButtonSize::Xs).icon_size(14.0).tip(super::chrome::key_tip("Cancel", "Escape")).on_click(move |_, _, cx| {
                    let _ = clear_root.update(cx, |root, cx| root.clear_comment(cx));
                })),
        )
        .child(div().min_h(px(34.0)).px(px(12.0)).pt(px(6.0)).pb(px(2.0)).text_color(t.ink).text_size(px(model.text_control)).line_height(relative(1.5)).child(model.note.clone()))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .pt(px(4.0))
                .pr(px(8.0))
                .pb(px(8.0))
                .pl(px(12.0))
                .child(div().min_w_0().ellipsis().text_color(t.quiet_strong).text_size(px(model.text_meta)).child(format!("Enter to {}, Shift + Enter for a new line", if editing { "save" } else { "add" })))
                .child(
                    div()
                        .id("diff-comment-send")
                        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, if editing { "Save comment" } else { "Comment on the selected lines" }).disabled(note_empty))
                        .flex_none()
                        .size(px(28.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .map(|button| if note_empty { button.bg(t.surface_inverse_off) } else { button.bg(t.surface_inverse).cursor_pointer().hover(|style| style.bg(t.surface_inverse_soft)) })
                        .child(icon(if editing { IconName::Check } else { IconName::ArrowUp }, if editing { 16.0 } else { 18.0 }, if note_empty { t.muted } else { t.ink_inverse }))
                        .tip(super::chrome::key_tip(if editing { "Save" } else { "Comment" }, "Enter"))
                        .on_click(move |_, _, cx| {
                            if !note_empty {
                                let _ = submit_root.update(cx, |root, cx| root.submit_comment(cx));
                            }
                        }),
                ),
        );
    div().w_full().py(px(0.0)).child(card).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{sync_list, wrapped_lines, DiffCache};
    use gpui::{px, ListOffset};

    #[test]
    fn rows_loaded_into_an_emptied_review_open_at_the_top() {
        let mut cache = DiffCache::default();
        sync_list(&mut cache, (0..50).collect(), 40, |_| 20.0);
        cache.list.scroll_to(ListOffset { item_ix: 30, offset_in_item: px(4.0) });
        sync_list(&mut cache, Vec::new(), 40, |_| 20.0);
        sync_list(&mut cache, (100..140).collect(), 40, |_| 20.0);
        assert_eq!(cache.list.logical_scroll_top().item_ix, 0);
    }

    #[test]
    fn wrapped_lines_break_between_words_and_hang() {
        // A word that would straddle the edge moves to the next line whole.
        assert_eq!(wrapped_lines("aaa bbbbbb ccc", 7.0, 7.0, 1.0), 3);
        // With nowhere to break, lines fill to the edge, and the hung lines are narrower.
        assert_eq!(wrapped_lines("aaaaaaaaaaaa", 6.0, 4.0, 1.0), 3);
        assert_eq!(wrapped_lines("short", 6.0, 4.0, 1.0), 1);
    }
}
