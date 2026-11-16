//! A pairing code over a private Unix socket, for `aic pair` beside a headless host on the same machine.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use act_core::domain::mobile::MobilePairingOffer;
use bytes::Bytes;
use futures::future::BoxFuture;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use tokio_util::sync::CancellationToken;

pub type Pair = Arc<dyn Fn() -> BoxFuture<'static, Result<MobilePairingOffer, String>> + Send + Sync>;

/// Serves pairing codes at `socket_path` until the returned token is cancelled, which also removes the socket.
pub async fn serve_pairing_socket(socket_path: &Path, pair: Pair) -> Result<CancellationToken, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let directory = socket_path.parent().ok_or("The pairing socket has no folder.")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let entry = std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if !entry.is_dir() || entry.uid() != unsafe { libc::getuid() } {
        return Err("The pairing socket directory is not owned by this user.".into());
    }
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).map_err(|error| error.to_string())?;
    // Whoever serves holds the machine's one instance, so any previous socket is stale.
    let _ = std::fs::remove_file(socket_path);
    let listener = tokio::net::UnixListener::bind(socket_path).map_err(|error| error.to_string())?;
    let stop = CancellationToken::new();
    let stopping = stop.clone();
    let path: PathBuf = socket_path.to_path_buf();
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = stopping.cancelled() => break,
            };
            let Ok((stream, _)) = accepted else { continue };
            let pair = pair.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                    let pair = pair.clone();
                    async move {
                        let mut response = if request.method() != http::Method::POST || request.uri().path() != "/pair" {
                            let mut response = Response::new(Full::new(Bytes::new()));
                            *response.status_mut() = StatusCode::NOT_FOUND;
                            response
                        } else {
                            match pair().await {
                                Ok(offer) => {
                                    let mut response = Response::new(Full::new(Bytes::from(serde_json::to_vec(&offer).unwrap_or_default())));
                                    response.headers_mut().insert("content-type", http::HeaderValue::from_static("application/json"));
                                    response
                                }
                                Err(message) => {
                                    let mut response = Response::new(Full::new(Bytes::from(message)));
                                    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                                    response
                                }
                            }
                        };
                        response.headers_mut().insert("cache-control", http::HeaderValue::from_static("no-store"));
                        Ok::<_, Infallible>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
        let _ = std::fs::remove_file(&path);
    });
    Ok(stop)
}

/// Asks the process serving on the socket for a pairing code.
pub async fn request_pairing(socket_path: &Path) -> Result<MobilePairingOffer, String> {
    let work = async {
        let stream = tokio::net::UnixStream::connect(socket_path).await.map_err(|_| "The bridge is not ready.".to_string())?;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|error| error.to_string())?;
        tokio::spawn(connection);
        let request = Request::post("/pair").header("host", "localhost").body(Full::new(Bytes::new())).map_err(|error| error.to_string())?;
        let response = sender.send_request(request).await.map_err(|error| error.to_string())?;
        let status = response.status();
        let body = response.into_body().collect().await.map_err(|error| error.to_string())?.to_bytes();
        if body.len() > 16_384 {
            return Err("Invalid pairing response.".to_string());
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        if status != StatusCode::OK {
            return Err(if text.is_empty() { "The bridge is not ready.".into() } else { text });
        }
        let offer: MobilePairingOffer = serde_json::from_str(&text).map_err(|_| "Invalid pairing response.".to_string())?;
        let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        if offer.code.is_empty() || !offer.code.chars().all(|character| alphabet.contains(character)) {
            return Err("Invalid pairing response.".into());
        }
        Ok(offer)
    };
    tokio::time::timeout(Duration::from_secs(3), work).await.map_err(|_| "The bridge did not answer.".to_string())?
}
