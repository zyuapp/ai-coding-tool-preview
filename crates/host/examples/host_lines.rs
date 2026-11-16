//! The host's pure message checks and workspace patches over stdin, for comparing them with the
//! TypeScript ones (scripts/reducer-diff/spec): each line `{"name", "args"}` is answered with one line
//! holding the result. `host_lines <requests-fifo> <answers-fifo>` reads and answers through those pipes.

use std::io::{BufRead, BufReader, Write};

use act_host::computers::replica::read_remote_workspace;
use act_host::mobile::messages;
use act_host::workspace_patches::{apply_workspace_patches, workspace_patches, WorkspacePatch};
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
    for line in input.lines() {
        let Ok(line) = line else { break };
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(asked) => call(asked["name"].as_str().unwrap_or(""), asked["args"].as_array().cloned().unwrap_or_default()),
            Err(_) => json!({ "error": "unreadable line" }),
        };
        let _ = writeln!(out, "{answer}");
        let _ = out.flush();
    }
}

fn call(name: &str, args: Vec<Value>) -> Value {
    let first = args.first().cloned().unwrap_or(Value::Null);
    match name {
        "isMobileClientMessage" => json!(messages::is_mobile_client_message(&first)),
        "isMobileQuery" => json!(messages::is_mobile_query(&first)),
        "isComputerClientRequest" => json!(messages::is_computer_client_message(&first)),
        "isComputerQuery" => json!(messages::is_computer_query(&first)),
        "isComputerServerMessage" => json!(messages::is_computer_server_message(&first)),
        "workspacePatches" => json!(workspace_patches(&first, args.get(1).unwrap_or(&Value::Null))),
        "applyWorkspacePatches" => {
            let mut state = first;
            match serde_json::from_value::<Vec<WorkspacePatch>>(args.get(1).cloned().unwrap_or_default()) {
                Ok(patches) => match apply_workspace_patches(&mut state, &patches) {
                    Ok(()) => state,
                    Err(error) => json!({ "error": format!("throws: {error}") }),
                },
                Err(error) => json!({ "error": format!("patches: {error}") }),
            }
        }
        "readRemoteWorkspace" => match read_remote_workspace(&first) {
            Ok(state) => serde_json::to_value(state).unwrap_or_default(),
            Err(error) => json!({ "error": format!("throws: {error}") }),
        },
        _ => json!({ "error": format!("no Rust counterpart wired for {name}") }),
    }
}
