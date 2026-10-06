//! Explicit LAN TLS service. Only typed, reviewed public projections cross this boundary.
//! Browser grants and peer grants are separate capabilities; no desktop command proxy exists.
use crate::{constant_eq, now_ms, reap, remaining, Asset, Slot};
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use forum_contracts::{
    PeerPublicationBatch, PeerPublicationSnapshot, PeerSession, PublicArtifact, PublicSnapshot,
    PublicationChange,
};
use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, ServerConfig,
    ServerConnection, SignatureScheme, StreamOwned,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::sync_channel,
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const LIMIT: usize = 16 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(6);
const MAX_GRANTS: usize = 64;
const MAX_AGE: u64 = 8 * 60 * 60 * 1000;
pub type SnapshotReader = Arc<dyn Fn(uuid::Uuid) -> Result<PublicSnapshot, String> + Send + Sync>;
pub type ChangeReader =
    Arc<dyn Fn(uuid::Uuid, u64) -> Result<(u64, Vec<PublicationChange>), String> + Send + Sync>;
#[derive(Clone)]
pub struct LanReaders {
    pub snapshot: SnapshotReader,
    pub changes: ChangeReader,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateInfo {
    pub directory: PathBuf,
    pub address: IpAddr,
    pub ca_certificate_path: PathBuf,
    pub certificate_sha256: String,
    pub ca_sha256: String,
    pub expires_days: u32,
}
pub struct LanIdentity {
    pub info: CertificateInfo,
    certificate: Vec<u8>,
    key: Vec<u8>,
    ca: Vec<u8>,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}
fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
impl LanIdentity {
    /// Creates a new local CA, never installs it in any trust store. A unique directory is required.
    /// Mobile users must install this CA using their OS certificate settings and compare its SHA-256.
    pub fn generate(directory: &Path, address: IpAddr) -> Result<Self> {
        Self::generate_cancellable(directory, address, &|| false)
    }
    pub fn generate_cancellable(
        directory: &Path,
        address: IpAddr,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self> {
        ensure!(!cancelled(), "证书准备已取消");
        ensure!(private_ip(address), "请选择本机局域网 IP");
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir(directory).context("证书目录必须是新的独立目录")?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let config=format!("[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=AI Vision Forum Local\n[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n[server]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:{address}\n");
        private_file(&directory.join("certificate.cnf"), config.as_bytes())?;
        let run = |args: &[&str]| -> Result<()> {
            ensure!(!cancelled(), "证书准备已取消");
            let mut child = Command::new("/usr/bin/openssl")
                .current_dir(directory)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = child.try_wait()? {
                    ensure!(status.success(), "本机 OpenSSL 证书生成失败");
                    return Ok(());
                }
                if cancelled() || Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("证书准备已取消或超时")
                }
                thread::sleep(Duration::from_millis(20));
            }
        };
        run(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-x509",
            "-days",
            "30",
            "-out",
            "ca.pem",
            "-config",
            "certificate.cnf",
            "-extensions",
            "ca",
        ])?;
        run(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
            "-config",
            "certificate.cnf",
        ])?;
        run(&[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "7",
            "-out",
            "server.pem",
            "-extfile",
            "certificate.cnf",
            "-extensions",
            "server",
        ])?;
        for path in fs::read_dir(directory)? {
            fs::set_permissions(path?.path(), fs::Permissions::from_mode(0o600))?;
        }
        // The CA key is not needed to run or pair. Keep no signing capability after provisioning.
        fs::remove_file(directory.join("ca.key"))?;
        let ca = CertificateDer::from_pem_file(directory.join("ca.pem"))?;
        private_file(&directory.join("ca.cer"), ca.as_ref())?;
        Self::load(directory, address)
    }
    pub fn load(directory: &Path, address: IpAddr) -> Result<Self> {
        ensure!(private_ip(address), "LAN address required");
        for name in ["server.pem", "server.key", "ca.pem", "ca.cer"] {
            let meta = fs::symlink_metadata(directory.join(name))?;
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 65536,
                "Invalid certificate file"
            );
        }
        let certificate = CertificateDer::from_pem_file(directory.join("server.pem"))?
            .as_ref()
            .to_vec();
        let key = PrivateKeyDer::from_pem_file(directory.join("server.key"))?
            .secret_der()
            .to_vec();
        let ca = CertificateDer::from_pem_file(directory.join("ca.pem"))?
            .as_ref()
            .to_vec();
        ensure!(
            fs::read(directory.join("ca.cer"))? == ca,
            "Exported CA differs from TLS trust identity"
        );
        let info = CertificateInfo {
            directory: directory.into(),
            address,
            ca_certificate_path: directory.join("ca.cer"),
            certificate_sha256: digest(&certificate),
            ca_sha256: digest(&ca),
            expires_days: 7,
        };
        Ok(Self {
            info,
            certificate,
            key,
            ca,
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Audience {
    Participant,
    Peer,
    Invite,
}
#[derive(Clone)]
struct Grant {
    source: PeerSession,
    token: String,
    expires: u64,
    audience: Audience,
    recipient: Option<uuid::Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipantInfo {
    pub grant_id: uuid::Uuid,
    pub url: String,
    pub expires_at_ms: u64,
}
#[derive(Debug, Clone, Serialize)]
pub struct LanGrantInfo {
    pub grant_id: uuid::Uuid,
    pub source: PeerSession,
    pub kind: String,
    pub expires_at_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerInvite {
    pub invite_id: uuid::Uuid,
    pub endpoint: String,
    pub source: PeerSession,
    pub invite_token: String,
    pub certificate_sha256: String,
    pub ca_der_base64: String,
    pub expires_at_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerLease {
    pub grant_id: uuid::Uuid,
    pub source: PeerSession,
    pub token: String,
    pub expires_at_ms: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    invite_token: String,
    recipient_device_id: uuid::Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicSearch {
    pub cursor: u64,
    pub artifacts: Vec<PublicArtifact>,
}
struct Shared {
    host: String,
    grants: Mutex<HashMap<uuid::Uuid, Grant>>,
    stop: AtomicBool,
    readers: LanReaders,
    assets: Vec<Asset>,
    callbacks: Mutex<Vec<JoinHandle<()>>>,
    rates: Mutex<HashMap<String, (u64, u16)>>,
}
pub struct LanGateway {
    pub endpoint: String,
    pub certificate: CertificateInfo,
    ca: Vec<u8>,
    state: Arc<Shared>,
    connections: Arc<Mutex<HashMap<usize, TcpStream>>>,
    worker: Option<JoinHandle<Vec<JoinHandle<()>>>>,
}
fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn valid_source(s: &PeerSession) -> Result<()> {
    ensure!(
        !s.owner_device_id.is_nil()
            && !s.event_id.is_nil()
            && !s.session_id.is_nil()
            && !s.title.trim().is_empty()
            && s.title.len() <= 500
            && s.room_name.len() <= 200,
        "Invalid public session metadata"
    );
    Ok(())
}
impl LanGateway {
    /// Bind one explicit local interface; callers must make this an operator action.
    pub fn start(
        identity: LanIdentity,
        port: u16,
        readers: LanReaders,
        assets: Vec<Asset>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::new(identity.info.address, port))?;
        listener.set_nonblocking(true)?;
        let host = listener.local_addr()?.to_string();
        let endpoint = format!("https://{host}");
        let tls =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(identity.certificate)],
                    PrivateKeyDer::try_from(identity.key).map_err(anyhow::Error::msg)?,
                )?;
        let tls = Arc::new(tls);
        let state = Arc::new(Shared {
            host,
            grants: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            readers,
            assets,
            callbacks: Mutex::new(Vec::new()),
            rates: Mutex::new(HashMap::new()),
        });
        let connections = Arc::new(Mutex::new(HashMap::<usize, TcpStream>::new()));
        let (shared, sockets) = (state.clone(), connections.clone());
        let worker = thread::spawn(move || {
            let mut handles = Vec::new();
            let mut next = 0usize;
            while !shared.stop.load(Ordering::Acquire) {
                reap(&mut handles);
                if let Ok(mut h) = shared.callbacks.lock() {
                    reap(&mut h);
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let Some(slot) = Slot::acquire() else {
                            let _ = stream.shutdown(Shutdown::Both);
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
                        let (s, c, t) = (shared.clone(), sockets.clone(), tls.clone());
                        handles.push(thread::spawn(move || {
                            let _result = serve_tls(stream, t, s, slot);
                            if let Ok(mut map) = c.lock() {
                                if let Some(socket) = map.remove(&id) {
                                    let _ = socket.shutdown(Shutdown::Both);
                                }
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
            if let Ok(map) = sockets.lock() {
                for socket in map.values() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
            handles
        });
        Ok(Self {
            endpoint,
            certificate: identity.info,
            ca: identity.ca,
            state,
            connections,
            worker: Some(worker),
        })
    }
    fn grant(
        &self,
        source: PeerSession,
        audience: Audience,
        ttl_ms: u64,
    ) -> Result<(uuid::Uuid, Grant)> {
        valid_source(&source)?;
        ensure!(
            !self.state.stop.load(Ordering::Acquire),
            "LAN service stopped"
        );
        ensure!(
            (1000..=MAX_AGE).contains(&ttl_ms),
            "TTL must be 1 second to 8 hours"
        );
        let mut map = self
            .state
            .grants
            .lock()
            .map_err(|_| anyhow::anyhow!("Grant unavailable"))?;
        map.retain(|_, g| g.expires > now_ms());
        ensure!(
            map.len() < MAX_GRANTS,
            "Revoke old grants before creating more"
        );
        let id = uuid::Uuid::new_v4();
        let grant = Grant {
            source,
            token: token(),
            expires: now_ms() + ttl_ms,
            audience,
            recipient: None,
        };
        map.insert(id, grant.clone());
        Ok((id, grant))
    }
    pub fn participant(&self, source: PeerSession, ttl_ms: u64) -> Result<ParticipantInfo> {
        let (id, g) = self.grant(source, Audience::Participant, ttl_ms)?;
        Ok(ParticipantInfo {
            grant_id: id,
            url: format!(
                "{}/display.html?session={}#token={}",
                self.endpoint, g.source.session_id, g.token
            ),
            expires_at_ms: g.expires,
        })
    }
    pub fn invite(&self, source: PeerSession) -> Result<PeerInvite> {
        let (id, g) = self.grant(source, Audience::Invite, 5 * 60 * 1000)?;
        Ok(PeerInvite {
            invite_id: id,
            endpoint: self.endpoint.clone(),
            source: g.source,
            invite_token: g.token,
            certificate_sha256: self.certificate.certificate_sha256.clone(),
            ca_der_base64: STANDARD.encode(&self.ca),
            expires_at_ms: g.expires,
        })
    }
    pub fn revoke(&self, id: uuid::Uuid) {
        if let Ok(mut grants) = self.state.grants.lock() {
            grants.remove(&id);
        }
    }
    /// Control-plane inventory deliberately omits all bearer secrets.
    pub fn grants(&self) -> Vec<LanGrantInfo> {
        let Ok(mut grants) = self.state.grants.lock() else {
            return vec![];
        };
        grants.retain(|_, grant| grant.expires > now_ms());
        grants
            .iter()
            .map(|(id, grant)| LanGrantInfo {
                grant_id: *id,
                source: grant.source.clone(),
                kind: match grant.audience {
                    Audience::Participant => "participant",
                    Audience::Peer => "peer",
                    Audience::Invite => "invite",
                }
                .into(),
                expires_at_ms: grant.expires,
            })
            .collect()
    }
}
impl Drop for LanGateway {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        if let Ok(mut grants) = self.state.grants.lock() {
            grants.clear();
        }
        if let Ok(map) = self.connections.lock() {
            for socket in map.values() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        if let Some(worker) = self.worker.take() {
            if let Ok(handles) = worker.join() {
                for handle in handles {
                    let _ = handle.join();
                }
            }
        }
        if let Ok(mut handles) = self.state.callbacks.lock() {
            reap(&mut handles);
        }
    }
}
fn auth(
    state: &Shared,
    session: uuid::Uuid,
    bearer: &str,
    audience: Audience,
    recipient: Option<uuid::Uuid>,
) -> Option<Grant> {
    state
        .grants
        .lock()
        .ok()?
        .values()
        .find(|g| {
            g.source.session_id == session
                && g.audience == audience
                && g.expires > now_ms()
                && g.recipient == recipient
                && constant_eq(bearer, &g.token)
        })
        .cloned()
}
// Reset the OS timeout on every actual TLS transport operation, so a peer that
// trickles handshake bytes cannot extend the absolute request deadline.
struct DeadlineSocket {
    inner: TcpStream,
    deadline: Instant,
}
impl DeadlineSocket {
    fn set_read_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
        self.inner.set_read_timeout(Some(remaining(self.deadline)?))
    }
    fn set_write_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
        self.inner
            .set_write_timeout(Some(remaining(self.deadline)?))
    }
}
impl Read for DeadlineSocket {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.set_read_timeout(None)?;
        self.inner.read(b)
    }
}
impl Write for DeadlineSocket {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.set_write_timeout(None)?;
        self.inner.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.set_write_timeout(None)?;
        self.inner.flush()
    }
}
fn rate_allowed(state: &Shared, bearer: &str) -> bool {
    let now = now_ms();
    let Ok(mut rates) = state.rates.lock() else {
        return false;
    };
    rates.retain(|_, (start, _)| now.saturating_sub(*start) < 10000);
    let entry = rates.entry(digest(bearer.as_bytes())).or_insert((now, 0));
    if entry.1 >= 30 {
        return false;
    }
    entry.1 += 1;
    true
}
fn write_response(
    tls: &mut StreamOwned<ServerConnection, DeadlineSocket>,
    status: u16,
    mime: &str,
    body: &[u8],
    deadline: Instant,
    allowed: &dyn Fn() -> bool,
) -> Result<()> {
    let header=format!("HTTP/1.1 {status} Response\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n\r\n",body.len());
    for bytes in [header.as_bytes(), body] {
        for chunk in bytes.chunks(16384) {
            ensure!(allowed(), "Access revoked");
            let timeout = remaining(deadline)?;
            tls.sock.set_write_timeout(Some(timeout))?;
            tls.sock.set_read_timeout(Some(timeout))?;
            tls.write_all(chunk)?;
            tls.flush()?;
        }
    }
    Ok(())
}
fn read_request(
    tls: &mut StreamOwned<ServerConnection, DeadlineSocket>,
    deadline: Instant,
) -> Result<(String, String, HashMap<String, String>, Vec<u8>)> {
    let mut data = Vec::new();
    let end = loop {
        let mut buf = [0u8; 1024];
        let timeout = remaining(deadline)?;
        tls.sock.set_read_timeout(Some(timeout))?;
        tls.sock.set_write_timeout(Some(timeout))?;
        let n = tls.read(&mut buf)?;
        ensure!(n > 0, "Connection closed");
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = data.windows(4).position(|v| v == b"\r\n\r\n") {
            ensure!(i <= 16384, "Header too large");
            break i + 4;
        }
        ensure!(data.len() <= 16384, "Header too large");
    };
    let mut lines = std::str::from_utf8(&data[..end])?.split("\r\n");
    let first = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>();
    ensure!(
        first.len() == 3 && first[2] == "HTTP/1.1",
        "Invalid request"
    );
    let (method, path) = (first[0].to_string(), first[1].to_string());
    let mut headers = HashMap::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').context("Invalid header")?;
        ensure!(
            headers
                .insert(k.to_ascii_lowercase(), v.trim().to_string())
                .is_none(),
            "Duplicate header"
        );
    }
    ensure!(
        !headers.contains_key("transfer-encoding"),
        "Chunked requests prohibited"
    );
    let len = headers
        .get("content-length")
        .map(|n| n.parse::<usize>())
        .transpose()?
        .unwrap_or(0);
    ensure!(len <= 4096, "Request body too large");
    let mut body = data[end..].to_vec();
    while body.len() < len {
        let mut buf = vec![0u8; (len - body.len()).min(4096)];
        tls.sock.set_read_timeout(Some(remaining(deadline)?))?;
        let n = tls.read(&mut buf)?;
        ensure!(n > 0, "Incomplete request");
        body.extend_from_slice(&buf[..n]);
    }
    ensure!(body.len() == len, "Pipelined requests prohibited");
    Ok((method, path, headers, body))
}
fn bounded_read(
    state: &Arc<Shared>,
    slot: Arc<Slot>,
    deadline: Instant,
    allowed: &dyn Fn() -> bool,
    work: impl FnOnce() -> Result<Vec<u8>> + Send + 'static,
) -> Result<Vec<u8>> {
    let (tx, rx) = sync_channel(1);
    let h = thread::spawn(move || {
        let _slot = slot;
        let result = work().and_then(|b| {
            ensure!(b.len() <= LIMIT, "Projection too large");
            Ok(b)
        });
        let _ = tx.try_send(result);
    });
    state
        .callbacks
        .lock()
        .map_err(|_| anyhow::anyhow!("Reader unavailable"))?
        .push(h);
    loop {
        ensure!(allowed(), "Read authorization expired");
        match rx.recv_timeout(remaining(deadline)?.min(Duration::from_millis(20))) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => bail!("Reader stopped"),
        }
    }
}
fn serve_tls(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    state: Arc<Shared>,
    slot: Arc<Slot>,
) -> Result<()> {
    let deadline = Instant::now() + DEADLINE;
    let mut tls = StreamOwned::new(
        ServerConnection::new(config)?,
        DeadlineSocket {
            inner: stream,
            deadline,
        },
    );
    let active = || !state.stop.load(Ordering::Acquire);
    let (method, target, headers, body) = read_request(&mut tls, deadline)?;
    if headers.get("host") != Some(&state.host)
        || headers
            .get("origin")
            .is_some_and(|v| v != &format!("https://{}", state.host))
    {
        return write_response(
            &mut tls,
            403,
            "text/plain",
            b"Invalid origin",
            deadline,
            &active,
        );
    }
    let path = target.split('?').next().unwrap_or_default();
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if path == "/v1/peer/pair" {
        if method != "POST" || headers.contains_key("origin") {
            return write_response(
                &mut tls,
                403,
                "text/plain",
                b"Peer pairing only",
                deadline,
                &active,
            );
        }
        let request: PairRequest = serde_json::from_slice(&body)?;
        ensure!(
            !request.recipient_device_id.is_nil(),
            "Peer identity required"
        );
        let result = {
            let mut map = state
                .grants
                .lock()
                .map_err(|_| anyhow::anyhow!("Grant unavailable"))?;
            let id = map
                .iter()
                .find(|(_, g)| {
                    g.audience == Audience::Invite
                        && g.expires > now_ms()
                        && constant_eq(&request.invite_token, &g.token)
                })
                .map(|(id, _)| *id);
            id.map(|id| {
                let g = map.get_mut(&id).expect("locked grant");
                g.audience = Audience::Peer;
                g.recipient = Some(request.recipient_device_id);
                g.token = token();
                g.expires = now_ms() + MAX_AGE;
                PeerLease {
                    grant_id: id,
                    source: g.source.clone(),
                    token: g.token.clone(),
                    expires_at_ms: g.expires,
                }
            })
        };
        return match result {
            Some(lease) => write_response(
                &mut tls,
                200,
                "application/json",
                &serde_json::to_vec(&lease)?,
                deadline,
                &active,
            ),
            None => write_response(
                &mut tls,
                401,
                "text/plain",
                b"Pairing code expired or already used",
                deadline,
                &active,
            ),
        };
    }
    if method != "GET" {
        return write_response(
            &mut tls,
            405,
            "text/plain",
            b"Read-only service",
            deadline,
            &active,
        );
    }
    let peer = path.starts_with("/v1/peer/sessions/");
    let prefix = if peer {
        "/v1/peer/sessions/"
    } else {
        "/v1/display/sessions/"
    };
    if let Some(rest) = path.strip_prefix(prefix) {
        let Some((id, action)) = rest.split_once('/') else {
            bail!("Invalid route")
        };
        let session = uuid::Uuid::parse_str(id)?;
        let audience = if peer {
            Audience::Peer
        } else {
            Audience::Participant
        };
        let recipient = if peer {
            headers.get("x-forum-device").and_then(|s| s.parse().ok())
        } else {
            None
        };
        let permitted = || active() && auth(&state, session, bearer, audience, recipient).is_some();
        if (peer && (recipient.is_none() || headers.contains_key("origin"))) || !permitted() {
            return write_response(
                &mut tls,
                401,
                "text/plain",
                b"Read grant expired or unavailable",
                deadline,
                &active,
            );
        }
        if !rate_allowed(&state, bearer) {
            return write_response(
                &mut tls,
                429,
                "text/plain",
                b"Too many requests",
                deadline,
                &active,
            );
        }
        let source = auth(&state, session, bearer, audience, recipient)
            .context("Grant expired")?
            .source;
        let read = state.readers.clone();
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
        let output = if action == "snapshot" {
            bounded_read(&state, slot, deadline, &permitted, move || {
                let s = (read.snapshot)(session).map_err(anyhow::Error::msg)?;
                if peer {
                    Ok(serde_json::to_vec(&PeerPublicationSnapshot {
                        owner_device_id: source.owner_device_id,
                        event_id: source.event_id,
                        session_id: session,
                        cursor: s.cursor,
                        artifacts: s.artifacts,
                    })?)
                } else {
                    Ok(serde_json::to_vec(&s)?)
                }
            })
        } else if peer && action == "changes" {
            let after = query
                .strip_prefix("after=")
                .context("after cursor required")?
                .parse::<u64>()?;
            bounded_read(&state, slot, deadline, &permitted, move || {
                let (cursor, changes) =
                    (read.changes)(session, after).map_err(anyhow::Error::msg)?;
                Ok(serde_json::to_vec(&PeerPublicationBatch {
                    owner_device_id: source.owner_device_id,
                    event_id: source.event_id,
                    session_id: session,
                    after_cursor: after,
                    cursor,
                    changes,
                })?)
            })
        } else if !peer && action == "search" {
            let query = decode_query(query.strip_prefix("q=").context("Query required")?)?;
            ensure!(
                !query.trim().is_empty() && query.len() <= 512,
                "Query must be 1 to 512 UTF-8 bytes"
            );
            bounded_read(&state, slot, deadline, &permitted, move || {
                let s = (read.snapshot)(session).map_err(anyhow::Error::msg)?;
                let needle = query.to_lowercase();
                let artifacts = s
                    .artifacts
                    .into_iter()
                    .filter(|a| {
                        a.title.to_lowercase().contains(&needle)
                            || a.text.to_lowercase().contains(&needle)
                            || a.evidence
                                .iter()
                                .any(|e| e.text.to_lowercase().contains(&needle))
                    })
                    .take(100)
                    .collect();
                Ok(serde_json::to_vec(&PublicSearch {
                    cursor: s.cursor,
                    artifacts,
                })?)
            })
        } else {
            return write_response(&mut tls, 404, "text/plain", b"Not found", deadline, &active);
        };
        return match output {
            Ok(output) => write_response(
                &mut tls,
                200,
                "application/json",
                &output,
                deadline,
                &permitted,
            ),
            Err(_) => write_response(
                &mut tls,
                503,
                "text/plain",
                b"Public projection unavailable",
                deadline,
                &active,
            ),
        };
    }
    if path.starts_with("/v1/") {
        return write_response(&mut tls, 404, "text/plain", b"Not found", deadline, &active);
    }
    if let Some((mime, bytes)) =
        state
            .assets
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(name, bytes)| {
                (
                    if name.ends_with(".html") {
                        "text/html; charset=utf-8"
                    } else if name.ends_with(".js") {
                        "text/javascript"
                    } else if name.ends_with(".css") {
                        "text/css"
                    } else {
                        "application/octet-stream"
                    },
                    *bytes,
                )
            })
    {
        write_response(&mut tls, 200, mime, bytes, deadline, &active)
    } else {
        write_response(&mut tls, 404, "text/plain", b"Not found", deadline, &active)
    }
}
fn decode_query(value: &str) -> Result<String> {
    let mut out = Vec::new();
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hi = bytes.next().context("Bad percent encoding")?;
                let lo = bytes.next().context("Bad percent encoding")?;
                out.push(u8::from_str_radix(std::str::from_utf8(&[hi, lo])?, 16)?);
            }
            b'+' => out.push(b' '),
            b'&' => bail!("Unexpected query parameter"),
            _ => out.push(b),
        }
    }
    Ok(String::from_utf8(out)?)
}
#[derive(Debug)]
struct PinnedVerifier {
    inner: Arc<WebPkiServerVerifier>,
    fingerprint: String,
}
impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if !constant_eq(&digest(end.as_ref()), &self.fingerprint) {
            return Err(rustls::Error::General(
                "Peer certificate fingerprint mismatch".into(),
            ));
        }
        self.inner
            .verify_server_cert(end, intermediates, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, signature)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, signature)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
/// Uses normal CA/hostname/expiry/signature checks AND a pinned leaf fingerprint.
/// Call only after an operator compared the displayed fingerprint on both devices.
pub struct PeerClient {
    address: SocketAddr,
    config: Arc<ClientConfig>,
    pub lease: PeerLease,
    device: uuid::Uuid,
}
impl PeerClient {
    pub fn pair(
        invite: &PeerInvite,
        confirmed_fingerprint: &str,
        local_device_id: uuid::Uuid,
    ) -> Result<Self> {
        ensure!(
            invite.expires_at_ms > now_ms()
                && invite.certificate_sha256.len() == 64
                && constant_eq(confirmed_fingerprint, &invite.certificate_sha256),
            "Compare and confirm the source certificate fingerprint before pairing"
        );
        valid_source(&invite.source)?;
        let address = invite
            .endpoint
            .strip_prefix("https://")
            .context("HTTPS required")?
            .parse::<SocketAddr>()?;
        ensure!(private_ip(address.ip()), "LAN endpoint required");
        ensure!(invite.ca_der_base64.len() < 100000, "CA too large");
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(
            STANDARD.decode(&invite.ca_der_base64)?,
        ))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()?;
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
                inner: verifier,
                fingerprint: confirmed_fingerprint.to_string(),
            }))
            .with_no_client_auth();
        let mut client = Self {
            address,
            config: Arc::new(config),
            lease: PeerLease {
                grant_id: uuid::Uuid::nil(),
                source: invite.source.clone(),
                token: String::new(),
                expires_at_ms: invite.expires_at_ms,
            },
            device: local_device_id,
        };
        let body = serde_json::to_vec(&PairRequest {
            invite_token: invite.invite_token.clone(),
            recipient_device_id: local_device_id,
        })?;
        let lease: PeerLease =
            serde_json::from_slice(&client.request("POST", "/v1/peer/pair", &body)?)?;
        ensure!(
            lease.source == invite.source
                && lease.expires_at_ms > now_ms()
                && lease.token.len() == 64
                && lease
                    .token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Pair response identity mismatch"
        );
        client.lease = lease;
        Ok(client)
    }
    pub fn snapshot(&self) -> Result<PeerPublicationSnapshot> {
        let value: PeerPublicationSnapshot = serde_json::from_slice(&self.request(
            "GET",
            &format!(
                "/v1/peer/sessions/{}/snapshot",
                self.lease.source.session_id
            ),
            &[],
        )?)?;
        ensure!(
            value.owner_device_id == self.lease.source.owner_device_id
                && value.event_id == self.lease.source.event_id
                && value.session_id == self.lease.source.session_id,
            "Peer changed source identity"
        );
        Ok(value)
    }
    pub fn changes(&self, after: u64) -> Result<PeerPublicationBatch> {
        let value: PeerPublicationBatch = serde_json::from_slice(&self.request(
            "GET",
            &format!(
                "/v1/peer/sessions/{}/changes?after={after}",
                self.lease.source.session_id
            ),
            &[],
        )?)?;
        ensure!(
            value.owner_device_id == self.lease.source.owner_device_id
                && value.event_id == self.lease.source.event_id
                && value.session_id == self.lease.source.session_id
                && value.after_cursor == after,
            "Peer changed source identity or cursor"
        );
        Ok(value)
    }
    fn request(&self, method: &str, path: &str, body: &[u8]) -> Result<Vec<u8>> {
        ensure!(self.lease.expires_at_ms > now_ms(), "Peer lease expired");
        let deadline = Instant::now() + DEADLINE;
        let stream = TcpStream::connect_timeout(&self.address, remaining(deadline)?)
            .context("TLS connect")?;
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        let name = ServerName::IpAddress(self.address.ip().into());
        let mut tls = StreamOwned::new(
            ClientConnection::new(self.config.clone(), name)?,
            DeadlineSocket {
                inner: stream,
                deadline,
            },
        );
        let head=format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nX-Forum-Device: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",self.address,self.lease.token,self.device,body.len());
        tls.write_all(head.as_bytes())
            .context("TLS request header")?;
        tls.write_all(body).context("TLS request body")?;
        tls.flush().context("TLS request flush")?;
        let mut bytes = Vec::new();
        let mut expected = None;
        let mut header_end = 0;
        loop {
            let mut buf = [0u8; 8192];
            let timeout = remaining(deadline)?;
            tls.sock.set_read_timeout(Some(timeout))?;
            tls.sock.set_write_timeout(Some(timeout))?;
            let n = match tls.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
            ensure!(bytes.len() <= LIMIT + 16384, "Peer response too large");
            if expected.is_none() {
                if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    ensure!(i < 16384, "Response header too large");
                    header_end = i + 4;
                    let head = std::str::from_utf8(&bytes[..i])?;
                    ensure!(
                        head.starts_with("HTTP/1.1 200 "),
                        "Peer authorization expired or request unavailable"
                    );
                    let headers = head
                        .split("\r\n")
                        .skip(1)
                        .map(|line| line.split_once(':').context("Bad peer header"))
                        .collect::<Result<Vec<_>>>()?;
                    ensure!(
                        !headers
                            .iter()
                            .any(|(n, _)| n.eq_ignore_ascii_case("transfer-encoding")),
                        "Chunked peer response unsupported"
                    );
                    let lengths = headers
                        .iter()
                        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                        .collect::<Vec<_>>();
                    ensure!(lengths.len() == 1, "Invalid peer response length");
                    let len = lengths[0].1.trim().parse::<usize>()?;
                    ensure!(len <= LIMIT, "Peer response too large");
                    expected = Some(len);
                } else {
                    ensure!(bytes.len() <= 16384, "Response header too large");
                }
            }
            if expected.is_some_and(|len| bytes.len() >= header_end + len) {
                break;
            }
        }
        let len = expected.context("Missing peer response")?;
        ensure!(bytes.len() == header_end + len, "Truncated peer response");
        Ok(bytes[header_end..].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forum_contracts::{AnalysisKind, PublicEvidence, Revision};
    fn source() -> PeerSession {
        PeerSession {
            owner_device_id: uuid::Uuid::new_v4(),
            event_id: uuid::Uuid::new_v4(),
            session_id: uuid::Uuid::new_v4(),
            title: "经确认的公开场次".into(),
            room_name: "A 会场".into(),
        }
    }
    fn setup() -> (PathBuf, LanGateway, PeerSession, Arc<Mutex<PublicSnapshot>>) {
        let path = std::env::temp_dir().join(format!("forum-lan-test-{}", uuid::Uuid::new_v4()));
        let identity = LanIdentity::generate(&path, "127.0.0.1".parse().unwrap()).unwrap();
        let source = source();
        let content = Arc::new(Mutex::new(PublicSnapshot {
            wall: None,
            cursor: 1,
            artifacts: vec![PublicArtifact {
                public_id: uuid::Uuid::new_v4(),
                revision: Revision::FIRST,
                kind: AnalysisKind::Minutes,
                title: "公开摘要".into(),
                text: "预算待评估，不含内部姓名".into(),
                evidence: vec![PublicEvidence {
                    public_evidence_id: uuid::Uuid::new_v4(),
                    revision: Revision::FIRST,
                    text: "公开依据".into(),
                }],
                publication_seq: 1,
            }],
        }));
        let data = content.clone();
        let changes = content.clone();
        let readers = LanReaders {
            snapshot: Arc::new(move |_| Ok(data.lock().unwrap().clone())),
            changes: Arc::new(move |_, after| {
                let s = changes.lock().unwrap();
                Ok((
                    s.cursor,
                    s.artifacts
                        .iter()
                        .filter(|a| a.publication_seq > after)
                        .map(|a| PublicationChange {
                            publication_seq: a.publication_seq,
                            public_id: a.public_id,
                            withdrawn: false,
                            artifact: Some(a.clone()),
                        })
                        .collect(),
                ))
            }),
        };
        let gateway = LanGateway::start(
            identity,
            0,
            readers,
            vec![("/display.html", b"public shell")],
        )
        .unwrap();
        (path, gateway, source, content)
    }
    #[test]
    fn tls_pinning_pair_scope_search_revoke_and_owner() {
        let _serial = crate::tests::SERIAL.lock().unwrap();
        let (path, gateway, source, data) = setup();
        let invite = gateway.invite(source.clone()).unwrap();
        let device = uuid::Uuid::new_v4();
        assert!(PeerClient::pair(&invite, "wrong fingerprint", device).is_err());
        let mut wrong = invite.clone();
        wrong.certificate_sha256 = "0".repeat(64);
        assert!(PeerClient::pair(&wrong, &wrong.certificate_sha256, device).is_err());
        let peer = PeerClient::pair(&invite, &invite.certificate_sha256, device).unwrap();
        assert!(PeerClient::pair(&invite, &invite.certificate_sha256, device).is_err());
        let snapshot = peer.snapshot().unwrap();
        assert_eq!(snapshot.owner_device_id, source.owner_device_id);
        assert_eq!(snapshot.artifacts.len(), 1);
        assert_eq!(peer.changes(0).unwrap().changes.len(), 1);
        assert!(peer
            .request(
                "GET",
                &format!("/v1/display/sessions/{}/snapshot", source.session_id),
                &[]
            )
            .is_err());
        assert!(peer.request("POST", "/begin_translation", b"{}").is_err());
        let grant = gateway.participant(source.clone(), 10000).unwrap();
        let browser_token = grant.url.split("#token=").nth(1).unwrap();
        let browser = PeerClient {
            address: peer.address,
            config: peer.config.clone(),
            device,
            lease: PeerLease {
                token: browser_token.into(),
                source: source.clone(),
                grant_id: grant.grant_id,
                expires_at_ms: grant.expires_at_ms,
            },
        };
        assert!(browser.snapshot().is_err());
        let body = browser
            .request(
                "GET",
                &format!(
                    "/v1/display/sessions/{}/search?q=%E9%A2%84%E7%AE%97",
                    source.session_id
                ),
                &[],
            )
            .unwrap();
        let searched: PublicSearch = serde_json::from_slice(&body).unwrap();
        assert_eq!(searched.artifacts.len(), 1);
        assert!(!String::from_utf8(body).unwrap().contains("source_span"));
        assert!(browser
            .request(
                "GET",
                &format!("/v1/display/sessions/{}/snapshot", uuid::Uuid::new_v4()),
                &[]
            )
            .is_err());
        data.lock().unwrap().artifacts.clear();
        data.lock().unwrap().cursor = 2;
        assert!(peer.snapshot().unwrap().artifacts.is_empty());
        gateway.revoke(grant.grant_id);
        assert!(browser
            .request(
                "GET",
                &format!("/v1/display/sessions/{}/snapshot", source.session_id),
                &[]
            )
            .is_err());
        gateway.revoke(invite.invite_id);
        assert!(peer.snapshot().is_err());
        drop(gateway);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn expired_grants_and_stop_close_idle_tls_connections() {
        let _serial = crate::tests::SERIAL.lock().unwrap();
        let (path, gateway, source, _) = setup();
        let invite = gateway.invite(source).unwrap();
        gateway
            .state
            .grants
            .lock()
            .unwrap()
            .get_mut(&invite.invite_id)
            .unwrap()
            .expires = now_ms() - 1;
        assert!(
            PeerClient::pair(&invite, &invite.certificate_sha256, uuid::Uuid::new_v4()).is_err()
        );
        let address = gateway
            .endpoint
            .strip_prefix("https://")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        let mut idle = TcpStream::connect(address).unwrap();
        thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        drop(gateway);
        assert!(start.elapsed() < Duration::from_secs(1));
        idle.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut byte = [0];
        assert!(matches!(idle.read(&mut byte), Ok(0) | Err(_)));
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn tls_revocation_during_reader_and_rate_limit() {
        let _serial = crate::tests::SERIAL.lock().unwrap();
        let (path, gateway, source, data) = setup();
        let invite = gateway.invite(source.clone()).unwrap();
        let peer = Arc::new(
            PeerClient::pair(&invite, &invite.certificate_sha256, uuid::Uuid::new_v4()).unwrap(),
        );
        let held = data.lock().unwrap();
        let started = Instant::now();
        let client = peer.clone();
        let result = thread::spawn(move || client.snapshot());
        while gateway.state.callbacks.lock().unwrap().is_empty()
            && started.elapsed() < Duration::from_secs(1)
        {
            thread::sleep(Duration::from_millis(10));
        }
        gateway.revoke(invite.invite_id);
        assert!(result.join().unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(held);
        let second = gateway.invite(source).unwrap();
        let client =
            PeerClient::pair(&second, &second.certificate_sha256, uuid::Uuid::new_v4()).unwrap();
        for _ in 0..30 {
            client.snapshot().unwrap();
        }
        assert!(client.snapshot().is_err());
        let inventory = serde_json::to_string(&gateway.grants()).unwrap();
        assert!(!inventory.contains(&second.invite_token));
        assert!(!inventory.contains(&client.lease.token));
        drop(gateway);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn incomplete_tls_handshake_has_absolute_deadline() {
        let _serial = crate::tests::SERIAL.lock().unwrap();
        let (path, gateway, _, _) = setup();
        let addr = gateway
            .endpoint
            .strip_prefix("https://")
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        // An incomplete TLS record must not keep extending the socket timeout.
        stream.write_all(&[0x16, 0x03, 0x03, 0x10, 0x00]).unwrap();
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(6600) {
            if stream.write_all(&[0]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        let mut byte = [0];
        assert!(matches!(stream.read(&mut byte), Ok(0) | Err(_)));
        assert!(gateway.connections.lock().unwrap().is_empty());
        assert!(start.elapsed() >= Duration::from_millis(5500));
        assert!(start.elapsed() < Duration::from_secs(7));
        drop(gateway);
        fs::remove_dir_all(path).unwrap();
    }
}
