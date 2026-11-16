//! The reducer over stdin, for comparing it with the TypeScript one: each line `{"now": ms, "input": {..}}`
//! is reduced and answered with one line `{"state", "effects", "result"}`, or `{"error"}` when the
//! input does not parse. `{"reset": true}` starts again from the empty state. Ids count up from zero.
//! A line carrying `"state"` reduces that state instead of the running one, with ids counted from zero.
//! `reduce_lines <requests-fifo> <answers-fifo>` reads and answers through those pipes instead.

use std::io::{BufRead, BufReader, Write};

use act_core::{reduce_transition, WorkspaceInput, WorkspaceState};
use serde_json::{json, Value};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let [requests, answers] = arguments.as_slice() {
        let input = BufReader::new(std::fs::File::open(requests).expect("requests pipe"));
        let output = std::fs::OpenOptions::new().write(true).open(answers).expect("answers pipe");
        serve(input, output);
    } else {
        let stdout = std::io::stdout();
        serve(std::io::stdin().lock(), stdout.lock());
    }
}

fn serve(input: impl BufRead, mut out: impl Write) {
    act_core::util::use_sequential_ids(true);
    let mut state = WorkspaceState::default();
    for line in input.lines() {
        let Ok(line) = line else { break };
        let answer = answer(&mut state, &line);
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
}

fn answer(state: &mut WorkspaceState, line: &str) -> Value {
    let Ok(mut asked) = serde_json::from_str::<Value>(line) else {
        return json!({ "error": "unreadable line" });
    };
    if asked["reset"] == true {
        *state = WorkspaceState::default();
        act_core::util::use_sequential_ids(true);
        return json!({ "state": state });
    }
    if let Some(given) = asked.get_mut("state").map(Value::take) {
        match serde_json::from_value::<WorkspaceState>(given.clone()) {
            Ok(given) => *state = given,
            Err(error) => return json!({ "error": format!("state: {error}{}", unreadable_field(&given)) }),
        }
        act_core::util::use_sequential_ids(true);
    }
    act_core::util::set_fixed_now(asked["now"].as_i64());
    match serde_json::from_value::<WorkspaceInput>(asked["input"].clone()) {
        Ok(input) => {
            let transition = reduce_transition(state, input);
            json!({ "state": state, "effects": transition.effects, "result": transition.result, "ids": act_core::util::ids_handed_out() })
        }
        Err(error) => {
            let event = serde_json::from_value::<act_core::WorkspaceEvent>(asked["input"].clone()).err().map(|error| error.to_string());
            let command = serde_json::from_value::<act_core::AppCommand>(asked["input"].clone()).err().map(|error| error.to_string());
            json!({ "error": format!("{error} (as event: {event:?}; as command: {command:?})") })
        }
    }
}

/// The first top-level field whose value alone stops the state from reading.
fn unreadable_field(given: &Value) -> String {
    let Value::Object(fields) = given else { return String::new() };
    let empty = serde_json::to_value(WorkspaceState::default()).unwrap_or_default();
    for (key, value) in fields {
        let mut probe = empty.clone();
        probe[key] = value.clone();
        if let Err(error) = serde_json::from_value::<WorkspaceState>(probe) {
            return format!(" (at {key}: {error})");
        }
    }
    String::new()
}
