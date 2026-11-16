//! Reads `{input, ts}` lines and prints those where the window's command check disagrees with `ts`.
use std::io::BufRead;

fn main() {
    let (mut cases, mut differ) = (0, 0);
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(case) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        cases += 1;
        let input = &case["input"];
        let rust = act_core::command_fields::view_input(input);
        if rust.is_ok() != case["ts"].as_bool().unwrap_or(false) {
            differ += 1;
            let shown = input.to_string();
            let shown: String = shown.chars().take(240).collect();
            println!("ts={} rust={:?} field={} {shown}", case["ts"], rust.err(), case["field"]);
        }
    }
    eprintln!("{cases} cases, {differ} differ");
}
