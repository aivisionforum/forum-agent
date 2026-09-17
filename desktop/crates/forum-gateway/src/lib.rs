//! Bounded read-only projection. LAN TLS is a separate, explicitly started service.
pub mod lan;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{sync_channel, RecvTimeoutError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
pub type PublicReader = Arc<dyn Fn(Uuid) -> Result<Value, String> + Send + Sync>;
pub type Asset = (&'static str, &'static [u8]);
const REQUEST_LIMIT: Duration = Duration::from_secs(6);
const HEADER_LIMIT: usize = 16384;
// Stuck callbacks retain their slot across gateway recreation, not just until
// the HTTP response times out. At most eight callback/connection pairs exist.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
struct Slot;
impl Slot {
    fn acquire() -> Option<Arc<Self>> {
        ACTIVE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                if n < 8 {
                    Some(n + 1)
                } else {
                    None
                }
            })
            .ok()
            .map(|_| Arc::new(Self))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone)]
struct Grant {
    session: Uuid,
    token: String,
    expires_ms: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub display_id: Uuid,
    pub url: String,
    pub expires_at_ms: u64,
}
type Connections = Arc<Mutex<HashMap<usize, TcpStream>>>;
struct Connection {
    id: usize,
    all: Connections,
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut all) = self.all.lock() {
            if let Some(socket) = all.remove(&self.id) {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }
}
pub struct DisplayGateway {
    origin: String,
    grants: Arc<Mutex<HashMap<Uuid, Grant>>>,
    stop: Arc<AtomicBool>,
    connections: Connections,
    readers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    worker: Option<JoinHandle<Vec<JoinHandle<()>>>>,
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn reap(handles: &mut Vec<JoinHandle<()>>) {
    let mut i = 0;
    while i < handles.len() {
        if handles[i].is_finished() {
            let _ = handles.swap_remove(i).join();
        } else {
            i += 1;
        }
    }
}
fn close_all(all: &Connections) {
    if let Ok(all) = all.lock() {
        for socket in all.values() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}
impl DisplayGateway {
    pub fn start(reader: PublicReader, assets: Vec<Asset>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let host = listener.local_addr()?.to_string();
        let origin = format!("http://{host}");
        let grants = Arc::new(Mutex::new(HashMap::<Uuid, Grant>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let connections: Connections = Arc::new(Mutex::new(HashMap::new()));
        let readers = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
        let (state, quitting, sockets, callbacks) = (
            grants.clone(),
            stop.clone(),
            connections.clone(),
            readers.clone(),
        );
        let assets = Arc::new(assets);
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            let mut next = 0usize;
            while !quitting.load(Ordering::Acquire) {
                reap(&mut requests);
                if let Ok(mut handles) = callbacks.lock() {
                    reap(&mut handles);
                }
                match listener.accept() {
                    Ok((mut stream, peer)) => {
                        let deadline = Instant::now() + REQUEST_LIMIT;
                        let slot = if peer.ip().is_loopback() {
                            Slot::acquire()
                        } else {
                            None
                        };
                        let Some(slot) = slot else {
                            let _ = respond(
                                &mut stream,
                                503,
                                "text/plain",
                                b"Display busy",
                                Instant::now() + Duration::from_millis(200),
                                &|| !quitting.load(Ordering::Acquire),
                            );
                            continue;
                        };
                        let Ok(clone) = stream.try_clone() else {
                            continue;
                        };
                        let id = next;
                        next = next.wrapping_add(1);
                        if let Ok(mut map) = sockets.lock() {
                            map.insert(id, clone);
                        } else {
                            continue;
                        }
                        let connection = Connection {
                            id,
                            all: sockets.clone(),
                        };
                        let (state, reader, assets, host, stop, callbacks) = (
                            state.clone(),
                            reader.clone(),
                            assets.clone(),
                            host.clone(),
                            quitting.clone(),
                            callbacks.clone(),
                        );
                        requests.push(thread::spawn(move || {
                            let _connection = connection;
                            let _ = serve(
                                &mut stream,
                                &host,
                                &state,
                                &reader,
                                &assets,
                                &stop,
                                deadline,
                                slot,
                                &callbacks,
                            );
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
            close_all(&sockets);
            requests
        });
        Ok(Self {
            origin,
            grants,
            stop,
            connections,
            readers,
            worker: Some(worker),
        })
    }
    pub fn grant(&self, session: Uuid) -> Result<DisplayInfo, String> {
        if self.stop.load(Ordering::Acquire) {
            return Err("大屏服务已停止".into());
        }
        let id = Uuid::new_v4();
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let expires_ms = now_ms() + 8 * 60 * 60 * 1000;
        let mut grants = self.grants.lock().map_err(|_| "大屏授权暂不可用")?;
        grants.retain(|_, v| v.expires_ms > now_ms());
        if grants.len() >= 64 {
            return Err("大屏授权数量已达上限，请撤销旧授权".into());
        }
        grants.insert(
            id,
            Grant {
                session,
                token: token.clone(),
                expires_ms,
            },
        );
        Ok(DisplayInfo {
            display_id: id,
            url: format!(
                "{}/display.html?session={}#token={}",
                self.origin, session, token
            ),
            expires_at_ms: expires_ms,
        })
    }
    pub fn revoke(&self, id: Uuid) {
        if let Ok(mut grants) = self.grants.lock() {
            grants.remove(&id);
        }
    }
}
impl Drop for DisplayGateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut grants) = self.grants.lock() {
            grants.clear();
        }
        close_all(&self.connections);
        if let Some(worker) = self.worker.take() {
            if let Ok(requests) = worker.join() {
                for request in requests {
                    let _ = request.join();
                }
            }
        }
        // Production core callbacks are bounded. An adversarial callback cannot be
        // joined indefinitely; its global Slot remains reserved until it returns.
        if let Ok(mut handles) = self.readers.lock() {
            reap(&mut handles);
        }
    }
}
fn remaining(deadline: Instant) -> std::io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "display request deadline")
        })
}
fn respond(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    deadline: Instant,
    allowed: &dyn Fn() -> bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let header=format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",body.len());
    for bytes in [header.as_bytes(), body] {
        let mut offset = 0;
        while offset < bytes.len() {
            if !allowed() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "display access revoked",
                ));
            }
            stream.set_write_timeout(Some(remaining(deadline)?))?;
            match stream.write(&bytes[offset..(offset + 16384).min(bytes.len())]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "display closed",
                    ))
                }
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}
fn authorized(grants: &Mutex<HashMap<Uuid, Grant>>, session: Uuid, token: &str) -> bool {
    grants.lock().is_ok_and(|map| {
        map.values().any(|g| {
            g.session == session && g.expires_ms > now_ms() && constant_eq(token, &g.token)
        })
    })
}
#[allow(clippy::too_many_arguments)]
fn serve(
    stream: &mut TcpStream,
    host: &str,
    grants: &Mutex<HashMap<Uuid, Grant>>,
    reader: &PublicReader,
    assets: &[Asset],
    stop: &AtomicBool,
    deadline: Instant,
    slot: Arc<Slot>,
    readers: &Mutex<Vec<JoinHandle<()>>>,
) -> std::io::Result<()> {
    let active = || !stop.load(Ordering::Acquire);
    let header_deadline = (Instant::now() + Duration::from_secs(2)).min(deadline);
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        if !active() {
            return Ok(());
        }
        stream.set_read_timeout(Some(remaining(header_deadline)?))?;
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buffer[..n]);
        if bytes.len() > HEADER_LIMIT {
            return respond(
                stream,
                400,
                "text/plain",
                b"Invalid request",
                deadline,
                &active,
            );
        }
        if bytes.windows(4).any(|v| v == b"\r\n\r\n") {
            break;
        }
    }
    let Ok(header) = std::str::from_utf8(&bytes) else {
        return respond(
            stream,
            400,
            "text/plain",
            b"Invalid request",
            deadline,
            &active,
        );
    };
    let mut lines = header.split("\r\n");
    let parts: Vec<_> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
        return respond(
            stream,
            400,
            "text/plain",
            b"Invalid request",
            deadline,
            &active,
        );
    }
    if parts[0] != "GET" {
        return respond(
            stream,
            405,
            "text/plain",
            b"Read-only display",
            deadline,
            &active,
        );
    }
    let mut headers = HashMap::new();
    for line in lines.take_while(|v| !v.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return respond(
                stream,
                400,
                "text/plain",
                b"Invalid header",
                deadline,
                &active,
            );
        };
        if headers
            .insert(name.to_ascii_lowercase(), value.trim())
            .is_some()
        {
            return respond(
                stream,
                400,
                "text/plain",
                b"Duplicate header",
                deadline,
                &active,
            );
        }
    }
    if headers.get("host") != Some(&host)
        || headers
            .get("origin")
            .is_some_and(|v| *v != format!("http://{host}"))
    {
        return respond(
            stream,
            403,
            "text/plain",
            b"Invalid origin",
            deadline,
            &active,
        );
    }
    let path = parts[1].split('?').next().unwrap_or_default();
    if let Some(id) = path
        .strip_prefix("/v1/display/sessions/")
        .and_then(|p| p.strip_suffix("/snapshot"))
    {
        let Ok(session) = Uuid::parse_str(id) else {
            return respond(
                stream,
                400,
                "text/plain",
                b"Invalid session",
                deadline,
                &active,
            );
        };
        let token = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or_default();
        let permitted = || active() && authorized(grants, session, token);
        if !permitted() {
            return respond(
                stream,
                401,
                "text/plain",
                b"Display authorization expired or missing",
                deadline,
                &active,
            );
        }
        let (tx, rx) = sync_channel(1);
        let reader = reader.clone();
        let reader_slot = slot.clone();
        let handle = thread::spawn(move || {
            let _slot = reader_slot;
            let result = reader(session).and_then(|snapshot| {
                let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err("Public snapshot too large".into());
                }
                Ok(bytes)
            });
            let _ = tx.try_send(result);
        });
        if let Ok(mut handles) = readers.lock() {
            handles.push(handle);
        }
        loop {
            if !active() {
                return Ok(());
            }
            if !permitted() {
                return respond(
                    stream,
                    401,
                    "text/plain",
                    b"Display authorization expired or missing",
                    deadline,
                    &active,
                );
            }
            match rx.recv_timeout(remaining(deadline)?.min(Duration::from_millis(20))) {
                Ok(Ok(body)) => {
                    if !permitted() {
                        return respond(
                            stream,
                            401,
                            "text/plain",
                            b"Display authorization expired or missing",
                            deadline,
                            &active,
                        );
                    }
                    return respond(
                        stream,
                        200,
                        "application/json; charset=utf-8",
                        &body,
                        deadline,
                        &permitted,
                    );
                }
                Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                    return respond(
                        stream,
                        503,
                        "text/plain",
                        b"Published snapshot unavailable",
                        deadline,
                        &active,
                    )
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
    if path.starts_with("/v1/") || path.starts_with("/api/") {
        return respond(
            stream,
            404,
            "text/plain",
            b"No such read-only view",
            deadline,
            &active,
        );
    }
    let path = if path == "/" { "/display.html" } else { path };
    if let Some((_, body)) = assets.iter().find(|(name, _)| *name == path) {
        let mime = if path.ends_with(".html") {
            "text/html; charset=utf-8"
        } else if path.ends_with(".js") {
            "text/javascript; charset=utf-8"
        } else if path.ends_with(".css") {
            "text/css; charset=utf-8"
        } else if path.ends_with(".svg") {
            "image/svg+xml"
        } else if path.ends_with(".png") {
            "image/png"
        } else if path.ends_with(".woff2") {
            "font/woff2"
        } else {
            "application/octet-stream"
        };
        respond(stream, 200, mime, body, deadline, &active)
    } else {
        respond(
            stream,
            404,
            "text/plain",
            b"Display asset not found",
            deadline,
            &active,
        )
    }
}
fn constant_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0, |n, (a, b)| n | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(crate) static SERIAL: Mutex<()> = Mutex::new(());
    fn connection(info: &DisplayInfo, path: &str, auth: bool, origin: Option<&str>) -> TcpStream {
        let host = info
            .url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let token = info.url.split("#token=").nth(1).unwrap();
        let mut socket = TcpStream::connect(host).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        write!(
            socket,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\n{}{}\r\n",
            if auth {
                format!("Authorization: Bearer {token}\r\n")
            } else {
                String::new()
            },
            origin
                .map(|s| format!("Origin: {s}\r\n"))
                .unwrap_or_default()
        )
        .unwrap();
        socket
    }
    fn output(mut socket: TcpStream) -> String {
        let mut out = String::new();
        let _ = socket.read_to_string(&mut out);
        out
    }
    fn get(info: &DisplayInfo, path: &str, auth: bool, origin: Option<&str>) -> String {
        output(connection(info, path, auth, origin))
    }
    fn wait(mut condition: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < until, "condition timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn token_scope_and_read_only_snapshot_replacement() {
        let _serial = SERIAL.lock().unwrap();
        let sid = Uuid::new_v4();
        let count = Arc::new(AtomicUsize::new(1));
        let state = count.clone();
        let server = DisplayGateway::start(
            Arc::new(move |_| {
                Ok(serde_json::json!({"cursor":state.load(Ordering::Acquire),"artifacts":[]}))
            }),
            vec![],
        )
        .unwrap();
        let info = server.grant(sid).unwrap();
        let path = format!("/v1/display/sessions/{sid}/snapshot?after=0");
        assert!(get(&info, &path, false, None).starts_with("HTTP/1.1 401"));
        assert!(get(&info, &path, true, Some("http://evil.invalid")).starts_with("HTTP/1.1 403"));
        assert!(get(
            &info,
            &format!("/v1/display/sessions/{}/snapshot", Uuid::new_v4()),
            true,
            None
        )
        .starts_with("HTTP/1.1 401"));
        let response = get(&info, &path, true, None);
        assert!(response.contains("\"cursor\":1"));
        assert!(response.contains("Cache-Control: no-store"));
        count.store(2, Ordering::Release);
        assert!(get(&info, &path, true, None).contains("\"cursor\":2"));
        assert!(get(&info, "/api/stop", true, None).starts_with("HTTP/1.1 404"));
        server.revoke(info.display_id);
        assert!(get(&info, &path, true, None).starts_with("HTTP/1.1 401"));
    }
    #[test]
    fn slow_reader_cannot_outlive_revocation_or_expiry() {
        let _serial = SERIAL.lock().unwrap();
        for expiry in [false, true] {
            let entered = Arc::new(AtomicBool::new(false));
            let release = Arc::new(AtomicBool::new(false));
            let (started, gate) = (entered.clone(), release.clone());
            let server = DisplayGateway::start(
                Arc::new(move |_| {
                    started.store(true, Ordering::Release);
                    while !gate.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Ok(serde_json::json!({"secret":"old authorized result"}))
                }),
                vec![],
            )
            .unwrap();
            let sid = Uuid::new_v4();
            let info = server.grant(sid).unwrap();
            let socket = connection(
                &info,
                &format!("/v1/display/sessions/{sid}/snapshot"),
                true,
                None,
            );
            wait(|| entered.load(Ordering::Acquire));
            if expiry {
                server
                    .grants
                    .lock()
                    .unwrap()
                    .get_mut(&info.display_id)
                    .unwrap()
                    .expires_ms = now_ms() - 1;
            } else {
                server.revoke(info.display_id);
            }
            let result = output(socket);
            assert!(result.starts_with("HTTP/1.1 401"));
            assert!(!result.contains("old authorized result"));
            release.store(true, Ordering::Release);
            drop(server);
            wait(|| ACTIVE.load(Ordering::Acquire) == 0);
        }
    }
    #[test]
    fn drop_closes_inflight_socket_without_waiting_for_unbounded_reader() {
        let _serial = SERIAL.lock().unwrap();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let (started, gate) = (entered.clone(), release.clone());
        let server = DisplayGateway::start(
            Arc::new(move |_| {
                started.store(true, Ordering::Release);
                while !gate.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(serde_json::json!({"private":"must not arrive"}))
            }),
            vec![],
        )
        .unwrap();
        let sid = Uuid::new_v4();
        let info = server.grant(sid).unwrap();
        let socket = connection(
            &info,
            &format!("/v1/display/sessions/{sid}/snapshot"),
            true,
            None,
        );
        wait(|| entered.load(Ordering::Acquire));
        let before = Instant::now();
        drop(server);
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(!output(socket).contains("200 OK"));
        assert_eq!(ACTIVE.load(Ordering::Acquire), 1);
        release.store(true, Ordering::Release);
        wait(|| ACTIVE.load(Ordering::Acquire) == 0);
    }
    #[test]
    fn deadline_keeps_stuck_readers_in_budget() {
        let _serial = SERIAL.lock().unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicUsize::new(0));
        let (gate, count) = (release.clone(), entered.clone());
        let server = DisplayGateway::start(
            Arc::new(move |_| {
                count.fetch_add(1, Ordering::AcqRel);
                while !gate.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(serde_json::json!({"artifacts":[]}))
            }),
            vec![],
        )
        .unwrap();
        let sid = Uuid::new_v4();
        let info = server.grant(sid).unwrap();
        let path = format!("/v1/display/sessions/{sid}/snapshot");
        let before = Instant::now();
        let sockets: Vec<_> = (0..8)
            .map(|_| connection(&info, &path, true, None))
            .collect();
        wait(|| entered.load(Ordering::Acquire) == 8);
        assert!(get(&info, &path, true, None).starts_with("HTTP/1.1 503"));
        for socket in sockets {
            assert!(!output(socket).contains("200 OK"));
        }
        assert!(before.elapsed() < Duration::from_millis(6600));
        assert_eq!(ACTIVE.load(Ordering::Acquire), 8);
        assert!(get(&info, &path, true, None).starts_with("HTTP/1.1 503"));
        release.store(true, Ordering::Release);
        wait(|| ACTIVE.load(Ordering::Acquire) == 0);
        drop(server);
    }
    #[test]
    fn final_header_read_cannot_cross_size_limit() {
        let _serial = SERIAL.lock().unwrap();
        let server =
            DisplayGateway::start(Arc::new(|_| panic!("not authenticated")), vec![]).unwrap();
        let sid = Uuid::new_v4();
        let info = server.grant(sid).unwrap();
        let host = info
            .url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut socket = TcpStream::connect(host).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let prefix = format!("GET /display.html HTTP/1.1\r\nHost: {host}\r\nPadding: ");
        let request = format!(
            "{prefix}{}\r\n\r\n",
            "x".repeat(HEADER_LIMIT - prefix.len() + 1)
        );
        socket.write_all(request.as_bytes()).unwrap();
        assert!(output(socket).starts_with("HTTP/1.1 400"));
    }
}
