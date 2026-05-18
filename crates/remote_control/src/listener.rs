//! Network listener for Remote Control — TCP accept → TLS 1.3 →
//! WebSocket upgrade → HMAC handshake → JSON-RPC request loop. ADR-0003
//! is the load-bearing reference for every layer.
//!
//! Concurrency shape: one `accept_loop` task drives `TcpListener::accept`
//! in a `tokio::select!` against the shutdown oneshot; per-connection
//! work spawns onto a new tokio task whose lifetime is bounded by the
//! shutdown receiver (cloned via `tokio::sync::broadcast`-equivalent: a
//! `watch` channel where `borrow().clone()` cheaply propagates shutdown
//! intent to every alive connection).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use futures::{SinkExt as _, StreamExt as _};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::allow_list;
use crate::auth;
use crate::cert::ServerCert;
use crate::dispatch::{
    ConnectionDispatcher, JsonRpcResponse, RemoteDispatcher, parse_request,
};
use crate::model::{AuthorizedClient, BAN_DURATION_SECS};

/// Max time (seconds) the client has to reply to the challenge frame
/// before we drop the connection.
const HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// Idle-read timeout once authenticated. A connection that doesn't send
/// anything for this long is dropped; clients are expected to ping
/// (`remote.editor.ping`) well below this bound to stay alive.
const IDLE_READ_TIMEOUT_SECS: u64 = 60;

/// Configuration for a listener. Owned by the caller across the
/// `start_listener` call boundary; consumed into the spawned task.
pub struct ListenerConfig {
    pub bind_addr: SocketAddr,
    pub cert: ServerCert,
    /// Receiver of the live authorised-client list. The listener reads
    /// `borrow().clone()` at handshake time, so revoking a client
    /// (`clients_tx.send(new_list)`) takes effect on the NEXT connection.
    /// Open connections from a revoked client are NOT kicked — that's a
    /// future improvement.
    pub clients_rx: watch::Receiver<Vec<AuthorizedClient>>,
    /// Live cap on concurrent authorized client sockets. Updated via
    /// `tx.send(n)` whenever the user changes the value in the modal —
    /// the accept loop reads it on every accept so changes take effect
    /// immediately for new connections (already-open ones aren't
    /// retroactively kicked when the cap shrinks).
    pub max_connections_rx: watch::Receiver<u32>,
    pub dispatcher: Arc<dyn RemoteDispatcher>,
}

/// Per-connection registration. The accept loop keeps a Vec of these to
/// (a) enforce the connection budget by evicting the longest-idle entry
/// and (b) report observability if we ever surface it.
struct ConnectionSlot {
    peer: SocketAddr,
    /// Unix-millis timestamp of the last activity on this connection.
    /// Updated by the per-conn task on every inbound frame; read by the
    /// accept loop when picking an LRU victim. Atomic so the read side
    /// doesn't need a lock.
    last_activity: Arc<AtomicI64>,
    /// Drop-on-eviction signal. The connection task selects against
    /// `kill_rx`; firing it sends a clean close frame and exits.
    kill: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
struct ListenerState {
    /// IP -> Instant when the ban expires. Inserted on auth failure,
    /// pruned lazily on the next accept from any IP (so the map stays
    /// roughly bounded by the rate of recent offenders).
    banned_until: AsyncMutex<HashMap<IpAddr, Instant>>,
    active_conns: AsyncMutex<Vec<ConnectionSlot>>,
}

fn now_millis() -> i64 {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    dur.as_millis().min(i64::MAX as u128) as i64
}

/// Handle returned by `start_listener`. Dropping it triggers shutdown.
pub struct ListenerHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    bound_addr: SocketAddr,
    task: Option<JoinHandle<()>>,
}

impl ListenerHandle {
    pub fn bound_addr(&self) -> SocketAddr {
        self.bound_addr
    }
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            // Receiver may already be dropped if the task exited on its own
            // (e.g. accept errored out). Ignoring the send error is correct.
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Bind a TCP listener, build a TLS acceptor, and start the accept loop.
/// The returned handle owns the loop; dropping it shuts down the
/// listener and any in-flight connections.
pub async fn start_listener(cfg: ListenerConfig) -> Result<ListenerHandle> {
    let listener = TcpListener::bind(cfg.bind_addr)
        .await
        .with_context(|| format!("binding {:?}", cfg.bind_addr))?;
    let bound_addr = listener
        .local_addr()
        .context("reading bound local_addr")?;

    let server_config = build_tls_server_config(&cfg.cert)
        .context("building TLS ServerConfig")?;
    let acceptor = TlsAcceptor::from(Arc::new(server_config));

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let state = Arc::new(ListenerState::default());

    let task = tokio::spawn(accept_loop(
        listener,
        acceptor,
        cfg.clients_rx,
        cfg.max_connections_rx,
        cfg.dispatcher,
        state,
        shutdown_rx,
    ));

    Ok(ListenerHandle {
        shutdown_tx: Some(shutdown_tx),
        bound_addr,
        task: Some(task),
    })
}

fn build_tls_server_config(cert: &ServerCert) -> Result<ServerConfig> {
    let cert_der = CertificateDer::from(cert.cert_der.clone());
    let key_der = PrivateKeyDer::try_from(cert.key_der.clone())
        .map_err(|err| anyhow!("invalid private key: {err}"))?;

    let config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|err| anyhow!("with_single_cert: {err}"))?;
    Ok(config)
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    clients_rx: watch::Receiver<Vec<AuthorizedClient>>,
    max_connections_rx: watch::Receiver<u32>,
    dispatcher: Arc<dyn RemoteDispatcher>,
    state: Arc<ListenerState>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown_rx => {
                log::info!(target: "remote_control", "listener shutdown requested");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        // Cheapest gate first: drop the TCP stream
                        // immediately if the source IP is in the
                        // ban list. TLS never starts; the attacker
                        // pays only the SYN/ACK cost.
                        if is_banned(&state, peer.ip()).await {
                            log::debug!(
                                target: "remote_control",
                                "dropping connection from banned {peer}",
                            );
                            drop(stream);
                            continue;
                        }
                        // Second gate: connection-count budget. If
                        // the live count is already at the cap, evict
                        // the longest-idle slot before admitting the
                        // new one — so the user's freshly-tapped
                        // phone always gets in, even if a stale tab
                        // is still chewing on a previous socket.
                        let budget = *max_connections_rx.borrow();
                        evict_if_over_budget(&state, budget).await;

                        let acceptor = acceptor.clone();
                        let clients_rx = clients_rx.clone();
                        let dispatcher = dispatcher.clone();
                        let state = state.clone();
                        let last_activity = Arc::new(AtomicI64::new(now_millis()));
                        let (kill_tx, kill_rx) = oneshot::channel::<()>();
                        let slot = ConnectionSlot {
                            peer,
                            last_activity: last_activity.clone(),
                            kill: Some(kill_tx),
                        };
                        state.active_conns.lock().await.push(slot);
                        tokio::spawn(async move {
                            let result = handle_conn(
                                stream,
                                peer,
                                acceptor,
                                clients_rx,
                                dispatcher,
                                state.clone(),
                                last_activity.clone(),
                                kill_rx,
                            )
                            .await;
                            if let Err(err) = result {
                                log::debug!(
                                    target: "remote_control",
                                    "connection from {peer} ended with: {err:#}",
                                );
                            }
                            // Remove this slot on exit. Identity is by
                            // the Arc pointer of `last_activity`, which
                            // is unique per connection.
                            let mut conns = state.active_conns.lock().await;
                            conns.retain(|c| !Arc::ptr_eq(&c.last_activity, &last_activity));
                        });
                    }
                    Err(err) => {
                        // EMFILE / temporary errors shouldn't kill the loop.
                        // Sleep briefly to avoid a hot spin if the OS is
                        // refusing accepts entirely.
                        log::warn!(target: "remote_control", "accept error: {err:#}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
}

/// Lazily prune the ban list and check if `ip` is still banned.
async fn is_banned(state: &ListenerState, ip: IpAddr) -> bool {
    let mut bans = state.banned_until.lock().await;
    let now = Instant::now();
    bans.retain(|_, deadline| *deadline > now);
    bans.get(&ip).is_some_and(|deadline| *deadline > now)
}

/// Add `ip` to the ban list for [`BAN_DURATION_SECS`] starting now.
async fn ban_ip(state: &ListenerState, ip: IpAddr) {
    let mut bans = state.banned_until.lock().await;
    let deadline = Instant::now() + Duration::from_secs(BAN_DURATION_SECS);
    bans.insert(ip, deadline);
    log::info!(
        target: "remote_control",
        "ip {ip} banned for {BAN_DURATION_SECS}s after auth failure",
    );
}

/// If the active connection count is `>= budget`, evict the slot with
/// the OLDEST `last_activity` timestamp (LRU). Triggered before admitting
/// each new connection.
///
/// `budget == 0` is treated as "no limit" — useful escape hatch if the
/// user sets the setting to 0 (although the UI clamps to >= 1).
async fn evict_if_over_budget(state: &ListenerState, budget: u32) {
    if budget == 0 {
        return;
    }
    let mut conns = state.active_conns.lock().await;
    while conns.len() >= budget as usize {
        // argmin by last_activity; ties → first match (stable).
        let Some((idx, _)) = conns
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| c.last_activity.load(Ordering::Relaxed))
        else {
            break;
        };
        let mut victim = conns.swap_remove(idx);
        let victim_peer = victim.peer;
        if let Some(kill) = victim.kill.take() {
            // Receiver may have already exited (race with natural
            // close); ignoring the error is correct.
            let _ = kill.send(());
        }
        log::info!(
            target: "remote_control",
            "evicting longest-idle connection {victim_peer} to make room (budget={budget})",
        );
    }
}

async fn handle_conn(
    stream: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    clients_rx: watch::Receiver<Vec<AuthorizedClient>>,
    dispatcher: Arc<dyn RemoteDispatcher>,
    state: Arc<ListenerState>,
    last_activity: Arc<AtomicI64>,
    kill_rx: oneshot::Receiver<()>,
) -> Result<()> {
    // Disable Nagle: WS frames are small and latency-sensitive.
    let _ = stream.set_nodelay(true);

    let tls_stream = acceptor
        .accept(stream)
        .await
        .context("TLS handshake")?;

    let mut ws = tokio_tungstenite::accept_async(tls_stream)
        .await
        .context("WebSocket upgrade")?;

    // 1. Send challenge.
    let challenge = auth::make_challenge().context("make_challenge")?;
    let challenge_frame = serde_json::json!({
        "type": "challenge",
        "challenge": hex::encode(challenge),
        "v": 1,
    });
    ws.send(Message::Text(challenge_frame.to_string().into()))
        .await
        .context("sending challenge")?;

    // 2. Read response within 10s.
    let response_frame = tokio::time::timeout(
        Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        ws.next(),
    )
    .await
    .map_err(|_| anyhow!("auth timeout after {HANDSHAKE_TIMEOUT_SECS}s"))?
    .ok_or_else(|| anyhow!("connection closed during handshake"))?
    .context("reading handshake response")?;

    let response_text = match response_frame {
        Message::Text(text) => text,
        other => {
            return Err(anyhow!(
                "expected text frame during handshake, got {other:?}"
            ));
        }
    };

    let response_bytes = match parse_handshake_response(response_text.as_ref()) {
        Ok(bytes) => bytes,
        Err(err) => {
            // Malformed handshake response counts as an auth failure —
            // ban the IP so a script-kiddie scanner that's spraying
            // random bytes pays the 30s cooldown before its next try.
            ban_ip(&state, peer.ip()).await;
            return Err(err).context("parsing handshake response");
        }
    };

    // 3. Snapshot the client list at handshake time.
    let clients = clients_rx.borrow().clone();
    let identified = auth::identify_client(&challenge, &response_bytes, &clients);
    let Some(client) = identified else {
        log::info!(
            target: "remote_control",
            "auth failed for peer {peer} — banning IP for {BAN_DURATION_SECS}s",
        );
        ban_ip(&state, peer.ip()).await;
        let close = CloseFrame {
            code: CloseCode::Policy,
            reason: "unauthorized".into(),
        };
        let _ = ws.send(Message::Close(Some(close))).await;
        return Ok(());
    };
    let client_name = client.name.clone();
    log::info!(
        target: "remote_control",
        "client {client_name:?} from {peer} authenticated",
    );

    // 4. Welcome.
    let welcome = serde_json::json!({ "type": "welcome", "client": client_name });
    ws.send(Message::Text(welcome.to_string().into()))
        .await
        .context("sending welcome")?;

    // 5. Request loop.
    run_request_loop(&mut ws, &client_name, dispatcher.as_ref(), last_activity, kill_rx).await?;
    Ok(())
}

fn parse_handshake_response(text: &str) -> Result<[u8; 32]> {
    #[derive(serde::Deserialize)]
    struct ResponseFrame<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
        response: &'a str,
    }
    let frame: ResponseFrame =
        serde_json::from_str(text).context("decoding response frame as JSON")?;
    if frame.kind != "response" {
        return Err(anyhow!(
            "expected type=\"response\", got {:?}",
            frame.kind
        ));
    }
    let raw = hex::decode(frame.response.trim())
        .map_err(|err| anyhow!("response hex decode: {err}"))?;
    if raw.len() != 32 {
        return Err(anyhow!(
            "response must be 32 bytes (64 hex chars), got {} bytes",
            raw.len()
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&raw);
    Ok(out)
}

async fn run_request_loop<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    client_name: &str,
    dispatcher: &dyn RemoteDispatcher,
    last_activity: Arc<AtomicI64>,
    mut kill_rx: oneshot::Receiver<()>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // Per-connection dispatcher state (lazy: opened on first request). On
    // open, immediately `take_notifications()` so the select! arm sees
    // the receiver. If opening fails (e.g. local MCP socket missing) we
    // surface -32603 per-request and keep the WS alive — the client may
    // retry, and a flapping editor restart shouldn't kick paired phones.
    let mut conn: Option<Box<dyn ConnectionDispatcher>> = None;
    let mut notifications_rx: Option<tokio::sync::mpsc::Receiver<serde_json::Value>> =
        None;

    loop {
        // Tokio `select!` here arbitrates between WS-read, notification
        // pump, idle timeout, and the eviction-kill signal. The kill
        // arm fires when the accept loop picked this slot as LRU
        // victim to make room for a new connection.
        let select_outcome = if let Some(rx) = notifications_rx.as_mut() {
            tokio::select! {
                biased;
                _ = &mut kill_rx => SelectOutcome::Evicted,
                next = ws.next() => SelectOutcome::Frame(next),
                notification = rx.recv() => SelectOutcome::Notification(notification),
                _ = tokio::time::sleep(Duration::from_secs(IDLE_READ_TIMEOUT_SECS)) => {
                    SelectOutcome::Idle
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = &mut kill_rx => SelectOutcome::Evicted,
                next = ws.next() => SelectOutcome::Frame(next),
                _ = tokio::time::sleep(Duration::from_secs(IDLE_READ_TIMEOUT_SECS)) => {
                    SelectOutcome::Idle
                }
            }
        };

        match select_outcome {
            SelectOutcome::Frame(None) => {
                log::debug!(
                    target: "remote_control",
                    "client {client_name:?} closed connection",
                );
                return Ok(());
            }
            SelectOutcome::Frame(Some(Err(err))) => {
                return Err(anyhow!("ws read error: {err}"));
            }
            SelectOutcome::Frame(Some(Ok(frame))) => {
                // Any inbound frame counts as activity for LRU
                // eviction purposes — a client mid-conversation
                // shouldn't lose its slot to a fresh connection.
                last_activity.store(now_millis(), Ordering::Relaxed);
                match frame {
                    Message::Text(text) => {
                        let response = match parse_request(text.as_ref()) {
                            Ok(req) => {
                                // Lazily open the proxy. On first call we
                                // also grab the notifications receiver.
                                if conn.is_none() {
                                    match dispatcher.open_connection().await {
                                        Ok(mut c) => {
                                            notifications_rx = c.take_notifications();
                                            conn = Some(c);
                                        }
                                        Err(err) => {
                                            let response = JsonRpcResponse::error(
                                                req.id.clone(),
                                                -32603,
                                                format!(
                                                    "opening local MCP proxy: {err}"
                                                ),
                                            );
                                            write_response(ws, &response).await?;
                                            continue;
                                        }
                                    }
                                }
                                // Safe: `conn` is Some here.
                                let dispatcher_ref = conn.as_mut().ok_or_else(|| {
                                    anyhow!("connection dispatcher disappeared")
                                })?;
                                dispatcher_ref.dispatch(client_name, req).await
                            }
                            Err(parse_err_response) => *parse_err_response,
                        };
                        write_response(ws, &response).await?;
                    }
                    Message::Ping(payload) => {
                        ws.send(Message::Pong(payload))
                            .await
                            .context("sending pong")?;
                    }
                    Message::Pong(_) => {}
                    Message::Binary(_) => {
                        let err = JsonRpcResponse::error(
                            serde_json::Value::Null,
                            -32600,
                            "binary frames not supported on this protocol version",
                        );
                        write_response(ws, &err).await?;
                    }
                    Message::Close(frame) => {
                        log::debug!(
                            target: "remote_control",
                            "client {client_name:?} sent close frame: {frame:?}",
                        );
                        let _ = ws.send(Message::Close(None)).await;
                        return Ok(());
                    }
                    Message::Frame(_) => {}
                }
            }
            SelectOutcome::Notification(None) => {
                // Notifications channel closed (proxy reader dropped).
                // Stop pumping; keep the WS alive so the client can
                // still issue RPC calls (each call opens its own
                // upstream frame; the dispatcher will re-fail cleanly).
                notifications_rx = None;
            }
            SelectOutcome::Notification(Some(payload)) => {
                let kind = payload
                    .pointer("/params/kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !allow_list::should_forward_event(kind) {
                    continue;
                }
                let envelope = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "remote/notification",
                    "params": payload
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                });
                let serialized = match serde_json::to_string(&envelope) {
                    Ok(text) => text,
                    Err(err) => {
                        log::warn!(
                            target: "remote_control",
                            "serialising notification: {err:#}; dropping",
                        );
                        continue;
                    }
                };
                if let Err(err) = ws.send(Message::Text(serialized.into())).await {
                    return Err(anyhow!(
                        "sending notification to client {client_name:?}: {err}"
                    ));
                }
            }
            SelectOutcome::Idle => {
                log::info!(
                    target: "remote_control",
                    "client {client_name:?} idle for {IDLE_READ_TIMEOUT_SECS}s, closing",
                );
                let close = CloseFrame {
                    code: CloseCode::Away,
                    reason: "idle timeout".into(),
                };
                let _ = ws.send(Message::Close(Some(close))).await;
                return Ok(());
            }
            SelectOutcome::Evicted => {
                log::info!(
                    target: "remote_control",
                    "client {client_name:?} evicted by accept loop to free a slot",
                );
                let close = CloseFrame {
                    code: CloseCode::Away,
                    reason: "evicted by new connection".into(),
                };
                let _ = ws.send(Message::Close(Some(close))).await;
                return Ok(());
            }
        }
    }
}

enum SelectOutcome {
    Frame(
        Option<
            Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        >,
    ),
    Notification(Option<serde_json::Value>),
    Idle,
    Evicted,
}

async fn write_response<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    response: &JsonRpcResponse,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let payload = serde_json::to_string(response)
        .map_err(|err| anyhow!("serialising response: {err}"))?;
    ws.send(Message::Text(payload.into()))
        .await
        .context("sending response")?;
    Ok(())
}
