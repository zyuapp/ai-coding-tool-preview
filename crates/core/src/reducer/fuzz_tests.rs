//! Commands from agents, phones and paired computers carry whatever their sender wrote. Any command
//! that passes validation must leave the reducer standing, whatever its fields hold.

use serde_json::{json, Map, Value};

use crate::domain::conversation::ConversationMessage;
use crate::domain::engine::AgentEngine;
use crate::domain::project::Project;
use crate::domain::run::ExecutionPolicy;
use crate::domain::thread::Thread;
use crate::reducer::reduce_transition;
use crate::state::WorkspaceState;
use crate::validation::{parse_command, DECLARATIONS};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

fn message(id: &str, text: &str) -> ConversationMessage {
    serde_json::from_value(json!({ "id": id, "kind": "user", "text": text, "at": 1 })).unwrap()
}

fn seeded() -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state.projects.push(Project { id: "p1".into(), root: "/tmp/p1".into(), name: None, workspace_id: Some("w1".into()), sort_index: None, unlisted: false });
    for id in ["t1", "t2"] {
        let mut thread = Thread::new(id, id, AgentEngine::Claude, ExecutionPolicy::Confirm, 1);
        thread.project_id = Some("p1".into());
        thread.messages = vec![message("m1", "hello").into(), message("m2", "é😀 world").into()];
        state.threads.push(thread.into());
    }
    state.current_id = Some("t1".into());
    state
}

fn values() -> Vec<Value> {
    vec![
        Value::Null,
        json!(true),
        json!(false),
        json!(0),
        json!(1),
        json!(-1),
        json!(2),
        json!(0.5),
        json!(-0.0),
        json!(1e15),
        json!(-1e15),
        json!(9_007_199_254_740_993_u64),
        json!(""),
        json!(" "),
        json!("t1"),
        json!("t2"),
        json!("m1"),
        json!("p1"),
        json!("w1"),
        json!("missing"),
        json!("é😀\u{0}\u{202e}"),
        json!("/tmp/p1"),
        json!("../../etc"),
        json!("x".repeat(5000)),
        json!("claude"),
        json!("codex"),
        json!("haiku"),
        json!("high"),
        json!("autonomous"),
        json!("priority"),
        json!("2026-11-13T08:00+a€"),
        json!([]),
        json!(["t1", "t2"]),
        json!(["", "missing"]),
        json!([0, -1, 5]),
        json!({}),
        json!({ "id": "", "text": "é" }),
    ]
}

#[test]
fn validated_commands_never_panic_the_reducer() {
    let pool = values();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut parsed = 0;
    for declaration in DECLARATIONS {
        for _ in 0..400 {
            let mut fields = Map::new();
            fields.insert("type".into(), json!(declaration.kind));
            for field in declaration.fields {
                if rng.next() % 5 != 0 {
                    fields.insert((*field).into(), rng.pick(&pool).clone());
                }
            }
            let value = Value::Object(fields);
            let Ok(command) = parse_command(&value) else { continue };
            parsed += 1;
            let mut state = seeded();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reduce_transition(&mut state, command.clone());
                reduce_transition(&mut state, command);
            }));
            assert!(outcome.is_ok(), "reducer panicked on {value}");
        }
    }
    assert!(parsed > 1000, "too few commands parsed ({parsed}) to mean anything");
}
