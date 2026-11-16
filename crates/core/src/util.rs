//! Shared helpers: the ordered map every keyed record uses, serde predicates, the clock and ids.

use std::cell::Cell;

/// Records keyed by id keep insertion order, as the JavaScript objects they mirror do.
pub type Map<V> = indexmap::IndexMap<String, V>;

pub fn is_false(value: &bool) -> bool {
    !*value
}

pub fn is_true(value: &bool) -> bool {
    *value
}

pub fn yes() -> bool {
    true
}

pub fn is_zero(value: &i64) -> bool {
    *value == 0
}

thread_local! {
    static FIXED_NOW: Cell<Option<i64>> = const { Cell::new(None) };
    static ID_SEQUENCE: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Milliseconds since the epoch, as `Date.now()` reads.
pub fn now() -> i64 {
    if let Some(fixed) = FIXED_NOW.with(|cell| cell.get()) {
        return fixed;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// Pins `now()` on this thread, or releases it with `None`. Tests read a fixed clock through this.
pub fn set_fixed_now(at: Option<i64>) {
    FIXED_NOW.with(|cell| cell.set(at));
}

/// A fresh random id, as `crypto.randomUUID()` gives.
pub fn random_id() -> String {
    if let Some(next) = ID_SEQUENCE.with(|cell| cell.get()) {
        ID_SEQUENCE.with(|cell| cell.set(Some(next + 1)));
        return format!("00000000-0000-4000-8000-{next:012}");
    }
    uuid::Uuid::new_v4().to_string()
}

/// How many ids `random_id()` has handed out since `use_sequential_ids`, when it counts.
pub fn ids_handed_out() -> Option<u64> {
    ID_SEQUENCE.with(|cell| cell.get())
}

/// Makes `random_id()` count up from zero on this thread, so tests can name what it hands out.
pub fn use_sequential_ids(enabled: bool) {
    ID_SEQUENCE.with(|cell| cell.set(if enabled { Some(0) } else { None }));
}

/// JavaScript's `String.length`: UTF-16 code units, which every length limit is written in.
pub fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `units` UTF-16 code units of `text`, never splitting a character.
pub fn js_slice_to(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, ch) in text.char_indices() {
        let width = ch.len_utf16();
        if count + width > units {
            return &text[..index];
        }
        count += width;
    }
    text
}

/// Removes every item equal to `value`, keeping the order of the rest.
pub fn without<T: PartialEq + Clone>(items: &[T], value: &T) -> Vec<T> {
    items.iter().filter(|item| *item != value).cloned().collect()
}

/// Orders text the way a person reads a list: letters before case, then the text as written.
pub fn locale_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase().cmp(&right.to_lowercase()).then_with(|| right.cmp(left))
}

/// The last non-empty segment of a slash-separated path.
pub fn last_segment(path: &str) -> Option<&str> {
    path.split('/').filter(|part| !part.is_empty()).last()
}

/// A whole number as the window's locale writes it, with thousands grouped.
pub fn locale_number(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Distinct names `intern` keeps; past this a name reads as `"unknown"`.
const INTERNED_LIMIT: usize = 256;

/// A name only a newer build knows, held for the life of the process so the value types naming it stay `Copy`.
pub fn intern(text: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(held) = names.iter().find(|held| **held == text) {
        return held;
    }
    if names.len() >= INTERNED_LIMIT {
        return "unknown";
    }
    let held: &'static str = Box::leak(js_slice_to(text, 64).to_owned().into_boxed_str());
    names.push(held);
    held
}

thread_local! {
    static AS_SENT: Cell<bool> = const { Cell::new(false) };
}

/// Runs `read` with engines, efforts and modes this build does not know kept as sent rather than
/// refused, for reading the state another computer's build sent.
pub fn read_as_sent<T>(read: impl FnOnce() -> T) -> T {
    let held = AS_SENT.with(|cell| cell.replace(true));
    let result = read();
    AS_SENT.with(|cell| cell.set(held));
    result
}

pub fn reading_as_sent() -> bool {
    AS_SENT.with(|cell| cell.get())
}

/// String serde for a value type that can hold a name only a newer build knows: written as its name,
/// read strictly except inside `read_as_sent`.
#[macro_export]
macro_rules! as_sent_serde {
    ($name:ident, $what:literal) => {
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                if $crate::util::reading_as_sent() {
                    return Ok($name::read(&text));
                }
                $name::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("unknown {} `{text}`", $what)))
            }
        }
    };
}
pub use as_sent_serde;
