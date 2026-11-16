//! How a transcript is cut into rows: user words, turns of agent work ending in an answer, and a
//! coordinator's folded updates (src/renderer/timeline/grouping.ts).

use act_core::shared::Shared;
use act_core::domain::conversation::{ConversationMessage, MessageKind};
use act_core::domain::message_origin::{sender_label, MessageOrigin};

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// `from` names what a folded message's row says it is: its threads' news, or the thread that sent it.
    Message { id: String, message: Shared<ConversationMessage>, from: Option<String> },
    Turn { id: String, steps: Vec<Shared<ConversationMessage>>, final_message: Option<Shared<ConversationMessage>>, ends_at: Option<i64>, live: bool },
    Threads { id: String, thread_ids: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Group {
    Entry(Entry),
    /// A coordinator's thread updates, and its replies to them, that a newer update has replaced.
    Updates { id: String, entries: Vec<Entry>, count: usize },
}

impl Entry {
    pub fn id(&self) -> &str {
        match self {
            Entry::Message { id, .. } | Entry::Turn { id, .. } | Entry::Threads { id, .. } => id,
        }
    }

    fn start(&self) -> Option<i64> {
        match self {
            Entry::Threads { .. } => None,
            Entry::Message { message, .. } => Some(message.at),
            Entry::Turn { steps, final_message, .. } => steps.first().or(final_message.as_ref()).map(|message| message.at),
        }
    }
}

impl Group {
    pub fn id(&self) -> &str {
        match self {
            Group::Entry(entry) => entry.id(),
            Group::Updates { id, .. } => id,
        }
    }

    /// The height a row is counted at until it is drawn (the timeline virtualizer's estimateSize).
    pub fn estimate(&self) -> f32 {
        match self {
            Group::Entry(Entry::Turn { final_message, .. }) => if final_message.is_some() { 140.0 } else { 64.0 },
            Group::Entry(Entry::Threads { .. }) => 120.0,
            Group::Updates { .. } | Group::Entry(Entry::Message { from: Some(_), .. }) => 40.0,
            Group::Entry(Entry::Message { message, .. }) => if message.kind == MessageKind::User { 88.0 } else { 64.0 },
        }
    }
}

/// A step runs until the next one starts; the newest step of a live turn has not ended yet.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedStep {
    pub message: Shared<ConversationMessage>,
    pub ends_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Note { id: String, message: Shared<ConversationMessage> },
    Tools { id: String, steps: Vec<TimedStep> },
}

pub struct Options<'a> {
    pub running: bool,
    pub tail_message_id: Option<&'a str>,
    pub run_ended_at: Option<i64>,
    pub coordinator: bool,
    /// The threads a coordinator started, oldest first, as (thread id, when).
    pub starts: &'a [(String, i64)],
}

fn thread_sender(message: &ConversationMessage) -> Option<String> {
    (message.kind == MessageKind::User).then(|| sender_label(message.origin.as_ref())).flatten()
}

/// Assistant text and the tool calls it drives belong to one turn. A run's answer closes its turn.
pub fn group_timeline(messages: &[Shared<ConversationMessage>], options: &Options) -> Vec<Group> {
    enum Raw {
        Entry(Entry),
        Open(Vec<Shared<ConversationMessage>>),
    }
    let mut raw: Vec<Raw> = Vec::new();
    let mut open: Option<usize> = None;
    for message in messages {
        if matches!(message.kind, MessageKind::User | MessageKind::System) || message.artifact {
            let from = if options.coordinator { thread_sender(message) } else { None };
            raw.push(Raw::Entry(Entry::Message { id: message.id.clone(), message: message.clone(), from }));
            open = None;
            continue;
        }
        let index = match open {
            Some(index) => index,
            None => {
                raw.push(Raw::Open(Vec::new()));
                raw.len() - 1
            }
        };
        if let Raw::Open(steps) = &mut raw[index] {
            steps.push(message.clone());
        }
        open = if message.answer { None } else { Some(index) };
    }
    let live_index = if options.running { open } else { None };
    let mut timeline: Vec<Entry> = raw
        .into_iter()
        .enumerate()
        .map(|(index, item)| match item {
            Raw::Entry(entry) => entry,
            Raw::Open(mut group) => {
                let live = live_index == Some(index);
                let settled = !live && group.last().is_some_and(|message| message.kind == MessageKind::Assistant);
                let final_message = if settled { group.pop() } else { None };
                let id = group.first().or(final_message.as_ref()).map(|message| message.id.clone()).unwrap_or_default();
                Entry::Turn { id, steps: group, final_message, ends_at: None, live }
            }
        })
        .collect();
    if let Some(tail) = options.tail_message_id {
        if options.running && live_index.is_none() && !messages.iter().any(|message| message.id == tail) {
            timeline.push(Entry::Turn { id: tail.to_string(), steps: Vec::new(), final_message: None, ends_at: None, live: true });
        }
    }
    let starts: Vec<Option<i64>> = timeline.iter().map(Entry::start).collect();
    for index in 0..timeline.len() {
        let next_start = starts.get(index + 1).copied().flatten();
        if let Entry::Turn { steps, final_message, ends_at, live, .. } = &mut timeline[index] {
            *ends_at = match final_message {
                Some(message) => Some(message.at),
                None => next_start.or(if *live { None } else { options.run_ended_at.or(steps.last().map(|step| step.at)) }),
            };
        }
    }
    let placed = if options.starts.is_empty() { timeline } else { with_starts(timeline, options.starts) };
    fold_updates(placed)
}

/// Each thread a coordinator started goes after the last entry that began by the time it did.
fn with_starts(entries: Vec<Entry>, starts: &[(String, i64)]) -> Vec<Entry> {
    let mut after: std::collections::BTreeMap<i64, Vec<String>> = Default::default();
    let mut index: i64 = -1;
    for (id, at) in starts {
        while ((index + 1) as usize) < entries.len() && entries[(index + 1) as usize].start().unwrap_or(i64::MAX) <= *at {
            index += 1;
        }
        after.entry(index).or_default().push(id.clone());
    }
    let mut placed = Vec::new();
    let mut cards = |index: i64, placed: &mut Vec<Entry>| {
        if let Some(thread_ids) = after.remove(&index) {
            placed.push(Entry::Threads { id: format!("threads-{}", thread_ids[0]), thread_ids });
        }
    };
    cards(-1, &mut placed);
    for (index, entry) in entries.into_iter().enumerate() {
        placed.push(entry);
        cards(index as i64, &mut placed);
    }
    placed
}

fn is_update(entry: &Entry) -> bool {
    matches!(entry, Entry::Message { message, .. } if message.kind == MessageKind::User && matches!(message.origin, Some(MessageOrigin::Coordination)))
}

/// Of updates with no word from the user between them, only the newest and its answer stay open.
fn fold_updates(entries: Vec<Entry>) -> Vec<Group> {
    if !entries.iter().any(is_update) {
        return entries.into_iter().map(Group::Entry).collect();
    }
    let mut folded = Vec::new();
    let mut run: Vec<Entry> = Vec::new();
    let mut cycle: Vec<Entry> = Vec::new();
    let mut count = 0;
    let flush = |folded: &mut Vec<Group>, run: &mut Vec<Entry>, cycle: &mut Vec<Entry>, count: &mut usize| {
        if *count > 0 {
            folded.push(Group::Updates { id: format!("updates-{}", run[0].id()), entries: std::mem::take(run), count: *count });
        }
        folded.extend(cycle.drain(..).map(Group::Entry));
        run.clear();
        *count = 0;
    };
    for entry in entries {
        if is_update(&entry) {
            if !cycle.is_empty() {
                run.append(&mut cycle);
                count += 1;
            }
            cycle = vec![entry];
        } else if matches!(&entry, Entry::Message { message, .. } if message.kind == MessageKind::User) {
            flush(&mut folded, &mut run, &mut cycle, &mut count);
            folded.push(Group::Entry(entry));
        } else if !cycle.is_empty() {
            cycle.push(entry);
        } else {
            folded.push(Group::Entry(entry));
        }
    }
    flush(&mut folded, &mut run, &mut cycle, &mut count);
    folded
}

pub fn time_steps(steps: &[Shared<ConversationMessage>], turn_ends_at: Option<i64>) -> Vec<TimedStep> {
    steps.iter().enumerate().map(|(index, message)| TimedStep { message: message.clone(), ends_at: steps.get(index + 1).map(|next| next.at).or(turn_ends_at) }).collect()
}

pub fn to_segments(steps: Vec<TimedStep>) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for step in steps {
        if step.message.kind != MessageKind::Tool {
            segments.push(Segment::Note { id: step.message.id.clone(), message: step.message });
            continue;
        }
        if let Some(Segment::Tools { steps, .. }) = segments.last_mut() {
            steps.push(step);
        } else {
            segments.push(Segment::Tools { id: step.message.id.clone(), steps: vec![step] });
        }
    }
    segments
}

/// Every message a group holds, which a match in any of them opens it for.
pub fn group_message_ids(group: &Group) -> Vec<String> {
    match group {
        Group::Entry(entry) => entry_message_ids(entry),
        Group::Updates { entries, .. } => entries.iter().flat_map(entry_message_ids).collect(),
    }
}

fn entry_message_ids(entry: &Entry) -> Vec<String> {
    match entry {
        Entry::Threads { .. } => Vec::new(),
        Entry::Message { message, .. } => vec![message.id.clone()],
        Entry::Turn { steps, final_message, .. } => steps.iter().chain(final_message.iter()).map(|message| message.id.clone()).collect(),
    }
}

/// Which row a message is in, so a match can be scrolled to whether or not its row is drawn.
pub fn message_rows(groups: &[Group]) -> std::collections::HashMap<String, usize> {
    let mut rows = std::collections::HashMap::new();
    for (index, group) in groups.iter().enumerate() {
        rows.insert(group.id().to_string(), index);
        for id in group_message_ids(group) {
            rows.insert(id, index);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use act_core::domain::conversation::create_conversation_message;

    fn message(kind: MessageKind, text: &str, at: i64, answer: bool) -> ConversationMessage {
        let mut message = create_conversation_message(kind, text, None);
        message.at = at;
        message.answer = answer;
        message
    }

    #[test]
    fn an_answer_closes_its_turn_and_later_work_folds_apart() {
        let messages = vec![
            message(MessageKind::User, "do it", 1, false),
            message(MessageKind::Tool, "Bash", 2, false),
            message(MessageKind::Assistant, "done", 3, true),
            message(MessageKind::Tool, "Read", 4, false),
        ]
        .into_iter()
        .map(Shared::new)
        .collect::<Vec<_>>();
        let groups = group_timeline(&messages, &Options { running: true, tail_message_id: None, run_ended_at: None, coordinator: false, starts: &[] });
        assert_eq!(groups.len(), 3);
        assert!(matches!(&groups[1], Group::Entry(Entry::Turn { steps, final_message: Some(_), ends_at: Some(3), live: false, .. }) if steps.len() == 1));
        assert!(matches!(&groups[2], Group::Entry(Entry::Turn { live: true, ends_at: None, .. })));
    }
}
