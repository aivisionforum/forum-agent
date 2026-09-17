//! One bounded JSON request per authenticated Unix stream. No TCP listener.
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const MAX_RPC_BYTES: usize = 1024 * 1024;
const IO_DEADLINE: Duration = Duration::from_secs(2);
pub type RpcResult<T> = Result<T, RpcError>;
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: String,
    pub retryable: bool,
    pub message: String,
}
impl RpcError {
    pub fn new(code: &str, retryable: bool, message: impl ToString) -> Self {
        Self {
            code: code.into(),
            retryable,
            message: message.to_string(),
        }
    }
    pub fn transport(error: impl ToString) -> Self {
        Self::new("TRANSPORT_UNAVAILABLE", true, error)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub socket_path: PathBuf,
    pub auth_token: String,
}
impl Endpoint {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            auth_token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        }
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.socket_path.is_absolute() && self.socket_path.as_os_str().len() <= 100,
            "Unix endpoint must be an absolute path of at most 100 bytes"
        );
        ensure!(
            self.auth_token.len() == 64 && self.auth_token.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid local RPC credential"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    pub request_id: Uuid,
    pub method: String,
    pub params: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedRequest {
    protocol_version: u32,
    auth_token: String,
    request: RpcRequest,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcResponse {
    request_id: Uuid,
    result: RpcResult<Value>,
}

fn wait_socket(stream: &UnixStream, event: libc::c_short, deadline: Instant) -> anyhow::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .context("RPC IO deadline")?;
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: event,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().max(1).min(i32::MAX as u128) as i32,
            )
        };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            anyhow::bail!("RPC IO deadline exceeded");
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}
fn frame_read(stream: &mut UnixStream) -> anyhow::Result<Vec<u8>> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + IO_DEADLINE;
    let mut read_exact = |bytes: &mut [u8]| -> anyhow::Result<()> {
        let mut cursor = 0;
        while cursor < bytes.len() {
            wait_socket(stream, libc::POLLIN, deadline)?;
            match stream.read(&mut bytes[cursor..]) {
                Ok(0) => anyhow::bail!("RPC peer closed frame"),
                Ok(count) => cursor += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    };
    let mut header = [0; 4];
    read_exact(&mut header)?;
    let length = u32::from_le_bytes(header) as usize;
    ensure!(
        (1..=MAX_RPC_BYTES).contains(&length),
        "RPC frame exceeds limit"
    );
    let mut body = vec![0; length];
    read_exact(&mut body)?;
    Ok(body)
}
fn frame_write(stream: &mut UnixStream, bytes: &[u8]) -> anyhow::Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_RPC_BYTES,
        "RPC frame exceeds limit"
    );
    stream.set_nonblocking(true)?;
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    let deadline = Instant::now() + IO_DEADLINE;
    let mut cursor = 0;
    while cursor < frame.len() {
        wait_socket(stream, libc::POLLOUT, deadline)?;
        match stream.write(&frame[cursor..]) {
            Ok(0) => anyhow::bail!("RPC peer closed write"),
            Ok(count) => cursor += count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
fn connect_bounded(path: &std::path::Path) -> anyhow::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    ensure!(
        raw >= 0,
        "cannot create RPC socket: {}",
        std::io::Error::last_os_error()
    );
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "cannot set nonblocking RPC connect"
    );
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    let bytes = path.as_os_str().as_bytes();
    ensure!(bytes.len() < address.sun_path.len(), "RPC path too long");
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as _;
    }
    #[cfg(target_os = "macos")]
    {
        address.sun_len = (2 + bytes.len() + 1) as u8;
    }
    let connected = unsafe {
        libc::connect(
            raw,
            &address as *const _ as *const libc::sockaddr,
            (2 + bytes.len() + 1) as _,
        )
    };
    if connected != 0 {
        let error = std::io::Error::last_os_error();
        ensure!(
            matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
            ),
            "RPC connect failed: {error}"
        );
        let mut descriptor = libc::pollfd {
            fd: raw,
            events: libc::POLLOUT,
            revents: 0,
        };
        ensure!(
            unsafe { libc::poll(&mut descriptor, 1, IO_DEADLINE.as_millis() as _) } > 0,
            "RPC connect deadline exceeded"
        );
        let mut error = 0i32;
        let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
        ensure!(
            unsafe {
                libc::getsockopt(
                    raw,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut error as *mut _ as *mut _,
                    &mut length,
                )
            } == 0
                && error == 0,
            "RPC connect failed: {}",
            std::io::Error::from_raw_os_error(error)
        );
    }
    ensure!(
        unsafe { libc::fcntl(raw, libc::F_SETFL, flags) } == 0,
        "cannot restore RPC blocking mode"
    );
    Ok(unsafe { UnixStream::from_raw_fd(fd.into_raw_fd()) })
}

#[derive(Debug, Clone)]
pub struct RuntimeClient {
    endpoint: Endpoint,
}
impl RuntimeClient {
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }
    pub fn call(&self, method: &str, params: Value) -> RpcResult<Value> {
        let inner = || -> anyhow::Result<RpcResult<Value>> {
            self.endpoint.validate()?;
            let metadata = fs::symlink_metadata(&self.endpoint.socket_path)?;
            ensure!(
                metadata.file_type().is_socket(),
                "RPC endpoint is not a socket"
            );
            let mut stream = connect_bounded(&self.endpoint.socket_path).context("RPC connect")?;
            let request = RpcRequest {
                request_id: Uuid::new_v4(),
                method: method.into(),
                params,
            };
            let body = serde_json::to_vec(&AuthenticatedRequest {
                protocol_version: 1,
                auth_token: self.endpoint.auth_token.clone(),
                request: request.clone(),
            })?;
            frame_write(&mut stream, &body).context("RPC request write")?;
            let response: RpcResponse =
                serde_json::from_slice(&frame_read(&mut stream).context("RPC response read")?)?;
            ensure!(
                response.request_id == request.request_id,
                "RPC response identity mismatch"
            );
            Ok(response.result)
        };
        inner().map_err(|error| RpcError::transport(format!("{error:#}")))?
    }
}

pub struct UdsServer {
    endpoint: Endpoint,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl UdsServer {
    /// Handler must provide a bounded actor/DB call, and only return success after commit.
    pub fn bind(
        endpoint: Endpoint,
        handler: impl Fn(RpcRequest) -> RpcResult<Value> + Send + 'static,
    ) -> anyhow::Result<Self> {
        endpoint.validate()?;
        crate::private_directory(endpoint.socket_path.parent().context("socket parent")?)?;
        ensure!(
            !endpoint.socket_path.exists(),
            "refusing to replace an existing RPC endpoint"
        );
        let listener = UnixListener::bind(&endpoint.socket_path)?;
        fs::set_permissions(&endpoint.socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let token = endpoint.auth_token.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS/Linux getpeereid-equivalent: socket access and token scope
                        // are mandatory; on macOS additionally verify effective user.
                        #[cfg(target_os = "macos")]
                        {
                            let mut uid = 0;
                            let mut gid = 0;
                            if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) }
                                != 0
                                || uid != unsafe { libc::geteuid() }
                            {
                                continue;
                            }
                        }
                        let result = (|| -> anyhow::Result<()> {
                            let request: AuthenticatedRequest =
                                serde_json::from_slice(&frame_read(&mut stream)?)?;
                            ensure!(
                                request.protocol_version == 1 && request.auth_token == token,
                                "unauthorized RPC request"
                            );
                            ensure!(
                                !request.request.request_id.is_nil()
                                    && !request.request.method.is_empty()
                                    && request.request.method.len() <= 64,
                                "invalid request identity"
                            );
                            let id = request.request.request_id;
                            let result = handler(request.request);
                            frame_write(
                                &mut stream,
                                &serde_json::to_vec(&RpcResponse {
                                    request_id: id,
                                    result,
                                })?,
                            )
                        })();
                        // Invalid/oversized/auth failures close this connection without
                        // handing any data to the writer or exposing a token in logs.
                        let _ = result;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            endpoint,
            stop,
            worker: Some(worker),
        })
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    pub fn stop(&mut self) -> anyhow::Result<()> {
        self.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.worker.as_ref().is_some_and(|h| !h.is_finished()) {
            ensure!(
                Instant::now() < deadline,
                "RPC handler did not stop; server ownership retained"
            );
            thread::sleep(Duration::from_millis(10));
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("RPC worker panicked"))?;
        }
        if self.endpoint.socket_path.exists() {
            fs::remove_file(&self.endpoint.socket_path)?;
        }
        Ok(())
    }
}
impl Drop for UdsServer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
