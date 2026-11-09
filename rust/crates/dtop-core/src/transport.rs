//! Byte streams to the Docker daemon: Unix sockets and TCP through `std::net`/`std::os::unix`,
//! Windows named pipes through `std::fs::File` plus `CancelIoEx`. TLS sits on top of TCP
//! (`crate::tls`). Everything above `Connection` is transport-agnostic.

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::endpoint::DockerEndpoint;
use crate::error::{DockerError, Result};

/// A single bidirectional byte stream to the daemon. Blocking; `interrupt` may be called from
/// another thread to unblock a pending `read` (used to cancel streams).
pub trait Connection: Send + Sync {
    /// Writes every byte or fails.
    fn write_all(&self, bytes: &[u8]) -> Result<()>;
    /// Reads up to `max_len` bytes. An empty result means the peer closed the stream.
    fn read(&self, max_len: usize) -> Result<Vec<u8>>;
    /// Unblocks any in-flight `read` from another thread.
    fn interrupt(&self);
    /// Releases the handle. Safe to call more than once.
    fn close(&self);
}

/// Opens connections to one endpoint.
pub trait Transport: Send + Sync {
    fn endpoint(&self) -> &DockerEndpoint;
    fn connect(&self) -> Result<Arc<dyn Connection>>;
}

/// A `std` stream (Unix socket or TCP) that can be shut down from another thread.
pub(crate) trait ShutdownStream: Send + Sync {
    fn read_into(&self, buf: &mut [u8]) -> io::Result<usize>;
    fn write_from(&self, buf: &[u8]) -> io::Result<usize>;
    fn shutdown_both(&self);
}

impl ShutdownStream for std::net::TcpStream {
    fn read_into(&self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
    fn write_from(&self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }
    fn shutdown_both(&self) {
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(unix)]
impl ShutdownStream for std::os::unix::net::UnixStream {
    fn read_into(&self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
    fn write_from(&self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }
    fn shutdown_both(&self) {
        // shutdown() (unlike close()) reliably wakes a thread blocked in recv().
        let _ = self.shutdown(std::net::Shutdown::Both);
    }
}

pub(crate) struct StreamConnection<S: ShutdownStream> {
    stream: S,
    closed: AtomicBool,
}

impl<S: ShutdownStream> StreamConnection<S> {
    pub(crate) fn new(stream: S) -> Self {
        StreamConnection {
            stream,
            closed: AtomicBool::new(false),
        }
    }

    fn check_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(DockerError::Cancelled)
        } else {
            Ok(())
        }
    }
}

pub(crate) fn map_read_error(error: &io::Error, closed: bool) -> DockerError {
    if closed {
        return DockerError::Cancelled;
    }
    match error.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            DockerError::TimedOut("socket read".into())
        }
        _ => DockerError::Io(format!("read failed: {error}")),
    }
}

impl<S: ShutdownStream> Connection for StreamConnection<S> {
    fn write_all(&self, bytes: &[u8]) -> Result<()> {
        self.check_open()?;
        let mut offset = 0;
        while offset < bytes.len() {
            match self.stream.write_from(&bytes[offset..]) {
                Ok(0) => return Err(DockerError::Io("write failed: connection closed".into())),
                Ok(n) => offset += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(DockerError::TimedOut("socket write".into()))
                }
                Err(e) => return Err(DockerError::Io(format!("write failed: {e}"))),
            }
        }
        Ok(())
    }

    fn read(&self, max_len: usize) -> Result<Vec<u8>> {
        self.check_open()?;
        let mut buffer = vec![0u8; max_len.max(1)];
        loop {
            match self.stream.read_into(&mut buffer) {
                Ok(n) => {
                    buffer.truncate(n);
                    return Ok(buffer);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(map_read_error(&e, self.closed.load(Ordering::SeqCst))),
            }
        }
    }

    fn interrupt(&self) {
        if !self.closed.load(Ordering::SeqCst) {
            self.stream.shutdown_both();
        }
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.stream.shutdown_both();
        }
    }
}

fn timeout(seconds: u64) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// Connects to a Unix domain socket such as `/var/run/docker.sock`.
#[cfg(unix)]
pub struct UnixSocketTransport {
    endpoint: DockerEndpoint,
    path: String,
    timeout_seconds: u64,
}

#[cfg(unix)]
impl UnixSocketTransport {
    pub fn new(path: &str, timeout_seconds: u64) -> Self {
        UnixSocketTransport {
            endpoint: DockerEndpoint::UnixSocket(path.to_string()),
            path: path.to_string(),
            timeout_seconds,
        }
    }
}

#[cfg(unix)]
impl Transport for UnixSocketTransport {
    fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    fn connect(&self) -> Result<Arc<dyn Connection>> {
        // sockaddr_un.sun_path is 104 bytes on macOS and 108 on Linux, including the NUL.
        let capacity = if cfg!(target_os = "macos") { 104 } else { 108 };
        if self.path.len() >= capacity {
            return Err(DockerError::SocketPathTooLong(self.path.clone()));
        }
        let stream = std::os::unix::net::UnixStream::connect(&self.path).map_err(|e| {
            DockerError::ConnectionFailed {
                endpoint: self.endpoint.to_string(),
                reason: os_reason(&e),
            }
        })?;
        let _ = stream.set_read_timeout(timeout(self.timeout_seconds));
        let _ = stream.set_write_timeout(timeout(self.timeout_seconds));
        Ok(Arc::new(StreamConnection::new(stream)))
    }
}

/// The OS's description of an error without Rust's "(os error N)" suffix, like `strerror`.
pub(crate) fn os_reason(error: &io::Error) -> String {
    let text = error.to_string();
    match text.find(" (os error") {
        Some(index) => text[..index].to_string(),
        None => text,
    }
}

/// Opens a TCP connection with timeouts applied, trying every resolved address.
pub(crate) fn connect_tcp(
    host: &str,
    port: u16,
    timeout_seconds: u64,
    endpoint: &DockerEndpoint,
) -> Result<std::net::TcpStream> {
    use std::net::ToSocketAddrs;
    let failed = |reason: String| DockerError::ConnectionFailed {
        endpoint: endpoint.to_string(),
        reason,
    };
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| failed(os_reason(&e)))?
        .collect();
    if addresses.is_empty() {
        return Err(failed("no addresses".into()));
    }
    let mut last_error = "no addresses".to_string();
    for address in addresses {
        let attempt = match timeout(timeout_seconds) {
            Some(limit) => std::net::TcpStream::connect_timeout(&address, limit),
            None => std::net::TcpStream::connect(address),
        };
        match attempt {
            Ok(stream) => {
                let _ = stream.set_read_timeout(timeout(timeout_seconds));
                let _ = stream.set_write_timeout(timeout(timeout_seconds));
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last_error = os_reason(&e),
        }
    }
    Err(failed(last_error))
}

/// Plain-HTTP TCP transport (e.g. `tcp://127.0.0.1:2375`). Works on every OS.
pub struct TcpTransport {
    endpoint: DockerEndpoint,
    host: String,
    port: u16,
    timeout_seconds: u64,
}

impl TcpTransport {
    pub fn new(host: &str, port: u16, timeout_seconds: u64) -> Self {
        TcpTransport {
            endpoint: DockerEndpoint::Tcp {
                host: host.to_string(),
                port,
            },
            host: host.to_string(),
            port,
            timeout_seconds,
        }
    }
}

impl Transport for TcpTransport {
    fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    fn connect(&self) -> Result<Arc<dyn Connection>> {
        let stream = connect_tcp(&self.host, self.port, self.timeout_seconds, &self.endpoint)?;
        Ok(Arc::new(StreamConnection::new(stream)))
    }
}

/// Builds the plain (non-TLS) transport for an endpoint on the current platform.
pub fn make_transport(
    endpoint: &DockerEndpoint,
    timeout_seconds: u64,
) -> Result<Arc<dyn Transport>> {
    match endpoint {
        DockerEndpoint::UnixSocket(path) => {
            #[cfg(unix)]
            {
                Ok(Arc::new(UnixSocketTransport::new(path, timeout_seconds)))
            }
            #[cfg(not(unix))]
            {
                Err(DockerError::UnsupportedEndpoint(format!(
                    "unix://{path} (use npipe:// on Windows)"
                )))
            }
        }
        DockerEndpoint::Tcp { host, port } => {
            Ok(Arc::new(TcpTransport::new(host, *port, timeout_seconds)))
        }
        DockerEndpoint::NamedPipe(path) => {
            #[cfg(windows)]
            {
                let _ = timeout_seconds;
                Ok(Arc::new(windows_pipe::NamedPipeTransport::new(path)))
            }
            #[cfg(not(windows))]
            {
                Err(DockerError::UnsupportedEndpoint(format!(
                    "npipe://{} (named pipes only exist on Windows)",
                    path.replace('\\', "/")
                )))
            }
        }
    }
}

#[cfg(windows)]
pub mod windows_pipe {
    //! Docker Desktop's engine pipe. `std::fs::File` opens and reads it; `CancelIoEx` interrupts
    //! a blocked read from another thread.

    use super::*;
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn CancelIoEx(handle: *mut c_void, overlapped: *mut c_void) -> i32;
        fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    }

    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_BROKEN_PIPE: i32 = 109;
    const ERROR_PIPE_BUSY: i32 = 231;
    const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
    const ERROR_MORE_DATA: i32 = 234;
    const ERROR_OPERATION_ABORTED: i32 = 995;

    pub struct NamedPipeConnection {
        file: std::fs::File,
        closed: AtomicBool,
    }

    impl Connection for NamedPipeConnection {
        fn write_all(&self, bytes: &[u8]) -> Result<()> {
            if self.closed.load(Ordering::SeqCst) {
                return Err(DockerError::Cancelled);
            }
            (&self.file)
                .write_all(bytes)
                .map_err(|e| DockerError::Io(format!("WriteFile failed: {e}")))
        }

        fn read(&self, max_len: usize) -> Result<Vec<u8>> {
            if self.closed.load(Ordering::SeqCst) {
                return Err(DockerError::Cancelled);
            }
            let mut buffer = vec![0u8; max_len.max(1)];
            match (&self.file).read(&mut buffer) {
                Ok(n) => {
                    buffer.truncate(n);
                    Ok(buffer)
                }
                Err(e) => match e.raw_os_error() {
                    Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(Vec::new()),
                    Some(ERROR_OPERATION_ABORTED) => Err(DockerError::Cancelled),
                    Some(ERROR_MORE_DATA) => Ok(buffer),
                    _ => Err(DockerError::Io(format!("ReadFile failed: {e}"))),
                },
            }
        }

        fn interrupt(&self) {
            if !self.closed.load(Ordering::SeqCst) {
                // SAFETY: the handle is owned by `self.file` and valid while `self` lives.
                unsafe {
                    CancelIoEx(self.file.as_raw_handle(), std::ptr::null_mut());
                }
            }
        }

        fn close(&self) {
            if !self.closed.swap(true, Ordering::SeqCst) {
                self.interrupt();
            }
        }
    }

    pub struct NamedPipeTransport {
        endpoint: DockerEndpoint,
        path: String,
    }

    impl NamedPipeTransport {
        pub fn new(path: &str) -> Self {
            NamedPipeTransport {
                endpoint: DockerEndpoint::NamedPipe(path.to_string()),
                path: path.to_string(),
            }
        }
    }

    impl Transport for NamedPipeTransport {
        fn endpoint(&self) -> &DockerEndpoint {
            &self.endpoint
        }

        fn connect(&self) -> Result<Arc<dyn Connection>> {
            for attempt in 0..2 {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.path)
                {
                    Ok(file) => {
                        return Ok(Arc::new(NamedPipeConnection {
                            file,
                            closed: AtomicBool::new(false),
                        }))
                    }
                    Err(e) => {
                        if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempt == 0 {
                            let wide: Vec<u16> =
                                self.path.encode_utf16().chain(std::iter::once(0)).collect();
                            // SAFETY: `wide` is a NUL-terminated UTF-16 string.
                            if unsafe { WaitNamedPipeW(wide.as_ptr(), 5_000) } != 0 {
                                continue;
                            }
                        }
                        let reason = if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) {
                            "pipe not found".to_string()
                        } else {
                            e.to_string()
                        };
                        return Err(DockerError::ConnectionFailed {
                            endpoint: self.endpoint.to_string(),
                            reason,
                        });
                    }
                }
            }
            Err(DockerError::ConnectionFailed {
                endpoint: self.endpoint.to_string(),
                reason: "pipe busy".into(),
            })
        }
    }
}
