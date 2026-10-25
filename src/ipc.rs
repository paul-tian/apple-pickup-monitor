//! Private, newline-delimited commands between the native app and its child.
//!
//! No socket or discoverable session is involved. The existing request handlers
//! are dispatched in memory, keeping their validation and transaction semantics.
//! stdin EOF owns the engine lifetime, including while Keychain startup waits.

use std::{future::Future, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, Uri},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};
use tower::ServiceExt;

use crate::service::{self, App};

const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONCURRENT_REQUESTS: usize = 8;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    id: u64,
    method: String,
    path: String,
    #[serde(default)]
    body: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Reply {
    id: u64,
    status: u16,
    body: Value,
}

impl Reply {
    fn error(id: u64, status: u16, message: &str) -> Self {
        Self {
            id,
            status,
            body: json!({"error": message}),
        }
    }
}

struct Endpoint {
    router: Router,
    host: String,
    token: String,
    quit: watch::Receiver<bool>,
}

struct Transport {
    requests: mpsc::Receiver<Command>,
    responses: mpsc::Sender<Reply>,
    input_closed: watch::Receiver<bool>,
    reader: JoinHandle<()>,
    writer: JoinHandle<Result<(), String>>,
}

impl Transport {
    fn new<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (requests_tx, requests) = mpsc::channel(MAX_CONCURRENT_REQUESTS);
        let (responses, responses_rx) = mpsc::channel(MAX_CONCURRENT_REQUESTS * 2);
        let (closed_tx, input_closed) = watch::channel(false);
        let errors = responses.clone();
        let reader = tokio::spawn(async move {
            read_requests(reader, requests_tx, errors).await;
            let _ = closed_tx.send(true);
        });
        let writer = tokio::spawn(write_replies(writer, responses_rx));
        Self {
            requests,
            responses,
            input_closed,
            reader,
            writer,
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

/// Start reading the parent pipe before initializing the app, so parent death
/// can cancel a pending Keychain prompt rather than orphaning the helper.
pub async fn serve<R, W, F, S>(
    create_app: F,
    input: R,
    output: W,
    shutdown: S,
) -> Result<(), String>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    F: Future<Output = Result<Arc<App>, String>>,
    S: Future<Output = ()>,
{
    let mut transport = Transport::new(input, output);
    tokio::pin!(shutdown);
    let app = tokio::select! {
        biased;
        _ = transport.input_closed.changed() => return Ok(()),
        result = &mut transport.writer => return writer_result(result),
        _ = &mut shutdown => return Ok(()),
        app = create_app => app?,
    };
    let endpoint = Endpoint {
        router: service::router(app.clone()),
        host: app.origin.trim_start_matches("http://").to_string(),
        token: app.token.clone(),
        quit: app.quit.subscribe(),
    };
    let result = drive(endpoint, &mut transport, &mut shutdown).await;
    // This does not wait for a credential-save blocking thread. main exits the
    // native process after this bounded shutdown, terminating such OS prompts.
    let _ = app.quit.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), app.watcher.stop()).await;
    result
}

async fn drive<S>(
    mut endpoint: Endpoint,
    transport: &mut Transport,
    shutdown: S,
) -> Result<(), String>
where
    S: Future<Output = ()>,
{
    let mut requests = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        if *transport.input_closed.borrow() || *endpoint.quit.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = transport.input_closed.changed() => break,
            result = &mut transport.writer => return writer_result(result),
            _ = endpoint.quit.changed() => break,
            _ = &mut shutdown => break,
            Some(_) = requests.join_next(), if !requests.is_empty() => {},
            command = transport.requests.recv(), if requests.len() < MAX_CONCURRENT_REQUESTS => {
                let Some(command) = command else { break };
                let router = endpoint.router.clone();
                let host = endpoint.host.clone();
                let token = endpoint.token.clone();
                let replies = transport.responses.clone();
                requests.spawn(async move {
                    let reply = dispatch(router, &host, &token, command).await;
                    let _ = replies.send(reply).await;
                });
            }
        }
    }
    requests.abort_all();
    Ok(())
}

fn writer_result(result: Result<Result<(), String>, tokio::task::JoinError>) -> Result<(), String> {
    result.map_err(|_| "The app's response pipe stopped unexpectedly.".to_string())?
}

enum Frame {
    Line(Vec<u8>),
    TooLarge,
    End,
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> std::io::Result<Frame> {
    let mut line = Vec::new();
    let mut too_large = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // Commands require a newline. A partial frame on a closed parent
            // pipe is discarded, never executed as a last-minute mutation.
            return Ok(Frame::End);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if !too_large {
            if line.len() + count > MAX_REQUEST_BYTES {
                too_large = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..count]);
            }
        }
        reader.consume(count);
        if newline.is_some() {
            return Ok(if too_large {
                Frame::TooLarge
            } else {
                Frame::Line(line)
            });
        }
    }
}

async fn read_requests<R: AsyncRead + Unpin>(
    input: R,
    requests: mpsc::Sender<Command>,
    replies: mpsc::Sender<Reply>,
) {
    let mut reader = BufReader::with_capacity(4096, input);
    loop {
        let reply = match read_frame(&mut reader).await {
            Ok(Frame::End) | Err(_) => break,
            Ok(Frame::TooLarge) => {
                Some(Reply::error(0, 413, "The monitoring request is too large."))
            }
            Ok(Frame::Line(line)) => match serde_json::from_slice::<Command>(&line) {
                Ok(command) => match requests.try_send(command) {
                    Ok(()) => None,
                    Err(mpsc::error::TrySendError::Full(command)) => Some(Reply::error(
                        command.id,
                        429,
                        "Too many monitoring requests are pending.",
                    )),
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                },
                Err(_) => Some(Reply::error(
                    0,
                    400,
                    "The monitoring request is not valid JSON.",
                )),
            },
        };
        if let Some(reply) = reply {
            // Never block input EOF detection behind a caller that is flooding
            // invalid commands without consuming its responses.
            if replies.try_send(reply).is_err() {
                break;
            }
        }
    }
}

async fn dispatch(router: Router, host: &str, token: &str, command: Command) -> Reply {
    let method = match command.method.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        _ => {
            return Reply::error(
                command.id,
                405,
                "This monitoring command method is not supported.",
            );
        }
    };
    let uri: Uri = match command.path.parse() {
        Ok(uri) => uri,
        Err(_) => return Reply::error(command.id, 400, "The monitoring command path is invalid."),
    };
    if !uri.path().starts_with("/api/") || uri.scheme().is_some() || uri.authority().is_some() {
        return Reply::error(
            command.id,
            403,
            "Only monitoring commands are available on this pipe.",
        );
    }
    let body = command.body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(&value).unwrap_or_default()
    });
    let request = match Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("x-session-token", token)
        .header("content-type", "application/json")
        .body(Body::from(body))
    {
        Ok(request) => request,
        Err(_) => return Reply::error(command.id, 400, "The monitoring command is invalid."),
    };
    let response = match router.oneshot(request).await {
        Ok(response) => response,
        Err(error) => match error {},
    };
    let status = response.status().as_u16();
    let data = match to_bytes(response.into_body(), MAX_RESPONSE_BYTES).await {
        Ok(data) => data,
        Err(_) => return Reply::error(command.id, 502, "The monitoring response is too large."),
    };
    let body = match serde_json::from_slice(&data) {
        Ok(value) => value,
        Err(_) => json!({"error": "The monitoring request could not be completed."}),
    };
    Reply {
        id: command.id,
        status,
        body,
    }
}

async fn write_replies<W: AsyncWrite + Unpin>(
    mut output: W,
    mut responses: mpsc::Receiver<Reply>,
) -> Result<(), String> {
    while let Some(response) = responses.recv().await {
        let mut bytes = serde_json::to_vec(&response)
            .map_err(|_| "Could not encode the monitoring response.".to_string())?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bytes = serde_json::to_vec(&Reply::error(
                response.id,
                502,
                "The monitoring response is too large.",
            ))
            .unwrap();
        }
        bytes.push(b'\n');
        output
            .write_all(&bytes)
            .await
            .map_err(|_| "The app closed its response pipe.".to_string())?;
        output
            .flush()
            .await
            .map_err(|_| "The app closed its response pipe.".to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json,
        extract::Query,
        routing::{get, post},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, DuplexStream};

    fn test_router() -> Router {
        Router::new()
            .route(
                "/api/state",
                get(|| async { Json(json!({"running": false, "targets": []})) }),
            )
            .route(
                "/api/catalog",
                get(
                    |Query(query): Query<std::collections::HashMap<String, String>>| async move {
                        Json(json!({"locale":query.get("locale"),"stores":[],"products":[]}))
                    },
                ),
            )
            .route(
                "/api/slow",
                post(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Json(json!({"ok":true}))
                }),
            )
    }
    type TestSession = (
        DuplexStream,
        BufReader<DuplexStream>,
        JoinHandle<Result<(), String>>,
        watch::Sender<bool>,
    );
    fn session() -> TestSession {
        let (client_input, server_input) = tokio::io::duplex(128 * 1024);
        let (server_output, client_output) = tokio::io::duplex(128 * 1024);
        let (quit, quit_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut transport = Transport::new(server_input, server_output);
            drive(
                Endpoint {
                    router: test_router(),
                    host: "native.invalid".into(),
                    token: "private".into(),
                    quit: quit_rx,
                },
                &mut transport,
                std::future::pending(),
            )
            .await
        });
        (client_input, BufReader::new(client_output), task, quit)
    }
    async fn response(reader: &mut BufReader<DuplexStream>) -> Reply {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
    #[tokio::test]
    async fn dispatches_state_and_regional_catalog_over_private_pipe() {
        let (mut input, mut output, task, _quit) = session();
        input
            .write_all(b"{\"id\":1,\"method\":\"GET\",\"path\":\"/api/state\"}\n")
            .await
            .unwrap();
        let first = response(&mut output).await;
        assert_eq!(first.id, 1);
        assert_eq!(first.status, 200);
        assert_eq!(first.body["running"], false);
        input
            .write_all(b"{\"id\":2,\"method\":\"GET\",\"path\":\"/api/catalog?locale=zh_CN\"}\n")
            .await
            .unwrap();
        let second = response(&mut output).await;
        assert_eq!(second.body["locale"], "zh_CN");
        drop(input);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn invalid_input_is_bounded_and_never_echoes_credentials() {
        let (mut input, mut output, task, _quit) = session();
        input.write_all(b"{PRIVATE-CREDENTIAL}\n").await.unwrap();
        let reply = response(&mut output).await;
        assert_eq!(reply.status, 400);
        assert!(!reply.body.to_string().contains("PRIVATE"));
        input
            .write_all(&vec![b'a'; MAX_REQUEST_BYTES + 10])
            .await
            .unwrap();
        input.write_all(b"\n").await.unwrap();
        assert_eq!(response(&mut output).await.status, 413);
        for (id, method, path, status) in [
            (1, "GET", "https://example.com/api/state", 403),
            (2, "DELETE", "/api/state", 405),
            (3, "GET", "/", 403),
        ] {
            let line = json!({"id":id,"method":method,"path":path}).to_string() + "\n";
            input.write_all(line.as_bytes()).await.unwrap();
            assert_eq!(response(&mut output).await.status, status);
        }
        drop(input);
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn eof_cancels_pending_operations_without_waiting_for_their_timeout() {
        let (mut input, mut output, task, _quit) = session();
        input.write_all(b"{\"id\":1,\"method\":\"POST\",\"path\":\"/api/slow\",\"body\":{}}\n{\"id\":2,\"method\":\"GET\",\"path\":\"/api/state\"}\n").await.unwrap();
        assert_eq!(response(&mut output).await.id, 2);
        drop(input);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn eof_cancels_startup_even_when_initialization_is_blocked() {
        let (input, server_input) = tokio::io::duplex(128);
        let (server_output, _output) = tokio::io::duplex(128);
        let task = tokio::spawn(serve(
            std::future::pending::<Result<Arc<App>, String>>(),
            server_input,
            server_output,
            std::future::pending(),
        ));
        drop(input);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn a_closed_output_pipe_terminates_the_transport() {
        let (mut input, output, task, _quit) = session();
        drop(output);
        input
            .write_all(b"{\"id\":1,\"method\":\"GET\",\"path\":\"/api/state\"}\n")
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
