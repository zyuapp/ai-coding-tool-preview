//! Serves the phone bridge on the loopback with an empty workspace and prints the pairing link, so the
//! phone app in a simulator can pair with it. Runs for the seconds given as its argument.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use act_host::mobile::pairing::PairingStore;
use act_host::mobile::relay::{MobileOp, MobileRelay, MobileResponse};
use act_host::mobile::server::{mobile_vocabulary, MobileServer, ServerOptions, SESSION_GRACE};
use serde_json::json;

fn empty_view() -> serde_json::Value {
    json!({
        "groups": [],
        "activity": { "priority": [], "running": [], "threads": [], "snoozed": [] },
        "theme": { "dark": "aicodingtool-dark", "light": "aicodingtool-light", "mode": "dark" },
        "engines": {},
        "upkeep": [],
        "dictation": false,
        "usage": { "engines": [], "composer": null },
        "archived": 0,
        "thread": null,
        "draft": null,
        "error": null,
    })
}

#[tokio::main]
async fn main() {
    let seconds: u64 = std::env::args().nth(1).and_then(|value| value.parse().ok()).unwrap_or(60);
    let data = std::env::temp_dir().join(format!("act-phone-bridge-{}", std::process::id()));
    let devices = Arc::new(Mutex::new(PairingStore::open(data.join("devices.json"), chrono::Utc::now().timestamp_millis())));
    let cell: Arc<OnceLock<Arc<MobileRelay>>> = Arc::new(OnceLock::new());
    let answering = cell.clone();
    let relay = MobileRelay::new(Arc::new(move |request| {
        println!("window asked: {}", serde_json::to_string(&request.op).unwrap_or_default());
        let relay = answering.get().unwrap().clone();
        let result = match request.op {
            MobileOp::Snapshot => Ok(empty_view()),
            MobileOp::Command { .. } => Ok(json!(null)),
            MobileOp::Query { .. } => Err("Nothing to read here.".to_string()),
        };
        tokio::spawn(async move { relay.answer(MobileResponse { request_id: request.request_id, result }) });
        true
    }));
    cell.set(relay.clone()).ok();
    let on_change_devices = devices.clone();
    let server = MobileServer::new(ServerOptions {
        devices: devices.clone(),
        port: 0,
        allowed_origins: Arc::new(Vec::new),
        relay,
        on_change: Arc::new(move || println!("devices: {:?}", on_change_devices.lock().unwrap().views().iter().map(|device| device.name.clone()).collect::<Vec<_>>())),
        workspace: None,
        session_grace: SESSION_GRACE,
        vocabulary: mobile_vocabulary(),
    });
    server.start("127.0.0.1").await.expect("the bridge starts");
    let port = server.port().unwrap();
    let address = act_host::mobile::addresses::loopback_address(port);
    let code = devices.lock().unwrap().mint(chrono::Utc::now().timestamp_millis());
    println!("{}", act_host::mobile::addresses::pairing_offer(&address, &code).url);
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    for session in server.session_views() {
        println!("session {} {:?} sequence {}", session.device_name, session.connection, session.sequence);
    }
    server.stop().await;
    let _ = std::fs::remove_dir_all(&data);
}
