//! Byte streams to the Docker daemon: Unix sockets and TCP through `std::net`/`std::os::unix`,
//! Windows named pipes through overlapped I/O and `CancelIoEx`. TLS sits on top of TCP
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
    /// Lets later reads wait indefinitely (`interrupt` still wakes them), for long-lived
    /// streams. Called once connection setup — TCP connect and any TLS handshake — finished
    /// under the transport's finite timeout. Transports without read timeouts ignore it.
    fn disable_read_timeout(&self) -> Result<()> {
        Ok(())
    }
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
    fn clear_read_timeout(&self) -> io::Result<()>;
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
        // On Windows, shutdown() does not wake a thread already blocked in recv() while the
        // peer stays silent (a followed log with no output). Cancelling the socket's pending
        // I/O does; reads issued afterwards see the shutdown and return at once.
        #[cfg(windows)]
        cancel_pending_socket_io(self);
    }
    fn clear_read_timeout(&self) -> io::Result<()> {
        self.set_read_timeout(None)
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
    fn clear_read_timeout(&self) -> io::Result<()> {
        self.set_read_timeout(None)
    }
}

#[cfg(windows)]
fn cancel_pending_socket_io(stream: &std::net::TcpStream) {
    use std::os::windows::io::AsRawSocket;
    // A SOCKET is a kernel handle.
    windows_pipe::cancel_all_io(stream.as_raw_socket() as usize as std::os::windows::io::RawHandle);
}

pub(crate) struct StreamConnection<S: ShutdownStream> {
    stream: S,
    closed: AtomicBool,
    /// Set by `interrupt`, so a read it cut short (EINTR, or a cancelled Windows recv) returns
    /// instead of being retried.
    interrupted: AtomicBool,
}

impl<S: ShutdownStream> StreamConnection<S> {
    pub(crate) fn new(stream: S) -> Self {
        StreamConnection {
            stream,
            closed: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
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
                Err(e)
                    if e.kind() == io::ErrorKind::Interrupted
                        && !self.interrupted.load(Ordering::SeqCst) =>
                {
                    continue
                }
                Err(e) => {
                    let stopped = self.closed.load(Ordering::SeqCst)
                        || self.interrupted.load(Ordering::SeqCst);
                    return Err(map_read_error(&e, stopped));
                }
            }
        }
    }

    fn interrupt(&self) {
        if !self.closed.load(Ordering::SeqCst) {
            self.interrupted.store(true, Ordering::SeqCst);
            self.stream.shutdown_both();
        }
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.stream.shutdown_both();
        }
    }

    fn disable_read_timeout(&self) -> Result<()> {
        self.stream
            .clear_read_timeout()
            .map_err(|e| DockerError::Io(format!("could not clear the read timeout: {e}")))
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
    streaming: bool,
}

#[cfg(unix)]
impl UnixSocketTransport {
    pub fn new(path: &str, timeout_seconds: u64) -> Self {
        UnixSocketTransport {
            endpoint: DockerEndpoint::UnixSocket(path.to_string()),
            path: path.to_string(),
            timeout_seconds,
            streaming: false,
        }
    }

    /// For long-lived streams: setup keeps the timeout, reads afterwards wait indefinitely.
    pub fn streaming(mut self) -> Self {
        self.streaming = true;
        self
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
        let connection = Arc::new(StreamConnection::new(stream));
        if self.streaming {
            connection.disable_read_timeout()?;
        }
        Ok(connection)
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
    streaming: bool,
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
            streaming: false,
        }
    }

    /// For long-lived streams: setup keeps the timeout, reads afterwards wait indefinitely.
    pub fn streaming(mut self) -> Self {
        self.streaming = true;
        self
    }
}

impl Transport for TcpTransport {
    fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    fn connect(&self) -> Result<Arc<dyn Connection>> {
        let stream = connect_tcp(&self.host, self.port, self.timeout_seconds, &self.endpoint)?;
        let connection = Arc::new(StreamConnection::new(stream));
        if self.streaming {
            connection.disable_read_timeout()?;
        }
        Ok(connection)
    }
}

/// Builds the plain (non-TLS) transport for an endpoint on the current platform.
pub fn make_transport(
    endpoint: &DockerEndpoint,
    timeout_seconds: u64,
) -> Result<Arc<dyn Transport>> {
    build_transport(endpoint, timeout_seconds, false)
}

/// Like `make_transport`, for long-lived streams (`/events`, `logs -f`): connecting still times
/// out after `setup_timeout_seconds`, then reads wait indefinitely.
pub fn make_streaming_transport(
    endpoint: &DockerEndpoint,
    setup_timeout_seconds: u64,
) -> Result<Arc<dyn Transport>> {
    build_transport(endpoint, setup_timeout_seconds, true)
}

fn build_transport(
    endpoint: &DockerEndpoint,
    timeout_seconds: u64,
    streaming: bool,
) -> Result<Arc<dyn Transport>> {
    match endpoint {
        DockerEndpoint::UnixSocket(path) => {
            #[cfg(unix)]
            {
                let transport = UnixSocketTransport::new(path, timeout_seconds);
                Ok(Arc::new(if streaming {
                    transport.streaming()
                } else {
                    transport
                }))
            }
            #[cfg(not(unix))]
            {
                Err(DockerError::UnsupportedEndpoint(format!(
                    "unix://{path} (use npipe:// on Windows)"
                )))
            }
        }
        DockerEndpoint::Tcp { host, port } => {
            let transport = TcpTransport::new(host, *port, timeout_seconds);
            Ok(Arc::new(if streaming {
                transport.streaming()
            } else {
                transport
            }))
        }
        DockerEndpoint::NamedPipe(path) => {
            #[cfg(windows)]
            {
                // Pipes have no timeouts; streams are cancelled with CancelIoEx.
                let _ = (timeout_seconds, streaming);
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
    //! Docker Desktop's engine pipe, using cancellable overlapped I/O.

    use super::*;
    use std::ffi::c_void;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: RawHandle,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateEventW(
            attributes: *mut c_void,
            manual_reset: i32,
            initial_state: i32,
            name: *const u16,
        ) -> RawHandle;
        fn SetEvent(event: RawHandle) -> i32;
        fn ReadFile(
            handle: RawHandle,
            buffer: *mut c_void,
            bytes_to_read: u32,
            bytes_read: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn WriteFile(
            handle: RawHandle,
            buffer: *const c_void,
            bytes_to_write: u32,
            bytes_written: *mut u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn GetOverlappedResult(
            handle: RawHandle,
            overlapped: *mut Overlapped,
            bytes_transferred: *mut u32,
            wait: i32,
        ) -> i32;
        fn CancelIoEx(handle: RawHandle, overlapped: *mut Overlapped) -> i32;
        fn WaitForMultipleObjects(
            count: u32,
            handles: *const RawHandle,
            wait_all: i32,
            ms: u32,
        ) -> u32;
        fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    }

    /// Cancels every pending I/O operation on `handle`, from any thread. Failure means there
    /// was nothing to cancel (ERROR_NOT_FOUND).
    pub(crate) fn cancel_all_io(handle: RawHandle) {
        // SAFETY: a null OVERLAPPED cancels all of the handle's operations; the caller owns
        // the handle for the duration of the call.
        unsafe {
            CancelIoEx(handle, std::ptr::null_mut());
        }
    }

    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_BROKEN_PIPE: i32 = 109;
    const ERROR_PIPE_BUSY: i32 = 231;
    const ERROR_PIPE_NOT_CONNECTED: i32 = 233;
    const ERROR_MORE_DATA: i32 = 234;
    const ERROR_OPERATION_ABORTED: i32 = 995;
    const ERROR_IO_PENDING: i32 = 997;
    const ERROR_NOT_FOUND: i32 = 1168;
    const WAIT_FAILED: u32 = u32::MAX;
    const INFINITE: u32 = u32::MAX;

    fn event() -> io::Result<OwnedHandle> {
        // SAFETY: null attributes/name create an unnamed manual-reset event.
        let handle = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: CreateEventW returned a new handle owned by this value.
            Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
        }
    }

    enum Operation<'a> {
        Read(&'a mut [u8]),
        Write(&'a [u8]),
    }

    pub struct NamedPipeConnection {
        file: std::fs::File,
        cancelled: AtomicBool,
        cancellation_event: OwnedHandle,
    }

    impl NamedPipeConnection {
        fn check_open(&self) -> Result<()> {
            if self.cancelled.load(Ordering::SeqCst) {
                Err(DockerError::Cancelled)
            } else {
                Ok(())
            }
        }

        fn io_error(&self, error: io::Error, reading: bool, len: usize) -> Result<usize> {
            if self.cancelled.load(Ordering::SeqCst)
                || error.raw_os_error() == Some(ERROR_OPERATION_ABORTED)
            {
                return Err(DockerError::Cancelled);
            }
            match error.raw_os_error() {
                Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) if reading => Ok(0),
                Some(ERROR_MORE_DATA) if reading => Ok(len),
                _ => Err(DockerError::Io(format!(
                    "{} failed: {error}",
                    if reading { "ReadFile" } else { "WriteFile" }
                ))),
            }
        }

        fn cancel_pending(&self, overlapped: &mut Overlapped) -> Option<io::Error> {
            // SAFETY: the pipe and overlapped operation remain alive until completion below.
            let cancelled = unsafe { CancelIoEx(self.file.as_raw_handle(), overlapped) };
            let error = if cancelled == 0 {
                let error = io::Error::last_os_error();
                (error.raw_os_error() != Some(ERROR_NOT_FOUND)).then_some(error)
            } else {
                None
            };
            let mut transferred = 0;
            // SAFETY: waiting for completion keeps both OVERLAPPED and its buffer alive.
            unsafe {
                GetOverlappedResult(self.file.as_raw_handle(), overlapped, &mut transferred, 1);
            }
            error
        }

        fn transfer(&self, operation: Operation<'_>) -> Result<usize> {
            self.check_open()?;
            let completion_event = event().map_err(|error| {
                DockerError::Io(format!("create pipe I/O event failed: {error}"))
            })?;
            let mut overlapped = Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: completion_event.as_raw_handle(),
            };
            let reading = matches!(&operation, Operation::Read(_));
            let len = match &operation {
                Operation::Read(buffer) => buffer.len(),
                Operation::Write(buffer) => buffer.len(),
            };
            let size = u32::try_from(len)
                .map_err(|_| DockerError::Io("pipe I/O request is too large".into()))?;
            // SAFETY: the buffer, pipe handle, event and OVERLAPPED remain valid through
            // completion, including when a pending operation must first be cancelled.
            let started = unsafe {
                match operation {
                    Operation::Read(buffer) => ReadFile(
                        self.file.as_raw_handle(),
                        buffer.as_mut_ptr().cast(),
                        size,
                        std::ptr::null_mut(),
                        &mut overlapped,
                    ),
                    Operation::Write(buffer) => WriteFile(
                        self.file.as_raw_handle(),
                        buffer.as_ptr().cast(),
                        size,
                        std::ptr::null_mut(),
                        &mut overlapped,
                    ),
                }
            };
            if started == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING) {
                    return self.io_error(error, reading, len);
                }
                let handles = [
                    self.cancellation_event.as_raw_handle(),
                    completion_event.as_raw_handle(),
                ];
                // SAFETY: both event handles are valid for the duration of this wait.
                let status = unsafe {
                    WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
                };
                match status {
                    0 => {
                        if let Some(error) = self.cancel_pending(&mut overlapped) {
                            return Err(DockerError::Io(format!("CancelIoEx failed: {error}")));
                        }
                        return Err(DockerError::Cancelled);
                    }
                    1 => {}
                    _ => {
                        let reason = if status == WAIT_FAILED {
                            io::Error::last_os_error().to_string()
                        } else {
                            format!("unexpected wait status {status}")
                        };
                        let cancellation = self
                            .cancel_pending(&mut overlapped)
                            .map(|error| format!("; CancelIoEx failed: {error}"))
                            .unwrap_or_default();
                        return Err(DockerError::Io(format!(
                            "wait for pipe I/O failed: {reason}{cancellation}"
                        )));
                    }
                }
            }
            let mut transferred = 0;
            // SAFETY: immediate success or the completion event means the operation is finished.
            let completed = unsafe {
                GetOverlappedResult(
                    self.file.as_raw_handle(),
                    &mut overlapped,
                    &mut transferred,
                    0,
                )
            };
            if completed == 0 {
                self.io_error(io::Error::last_os_error(), reading, len)
            } else {
                self.check_open()?;
                Ok(transferred as usize)
            }
        }
    }

    impl Connection for NamedPipeConnection {
        fn write_all(&self, mut bytes: &[u8]) -> Result<()> {
            self.check_open()?;
            while !bytes.is_empty() {
                let size = bytes.len().min(u32::MAX as usize);
                let written = self.transfer(Operation::Write(&bytes[..size]))?;
                if written == 0 {
                    return Err(DockerError::Io(
                        "WriteFile failed: connection closed".into(),
                    ));
                }
                bytes = &bytes[written..];
            }
            Ok(())
        }

        fn read(&self, max_len: usize) -> Result<Vec<u8>> {
            self.check_open()?;
            let mut buffer = vec![0u8; max_len.max(1).min(u32::MAX as usize)];
            let read = self.transfer(Operation::Read(&mut buffer))?;
            buffer.truncate(read);
            Ok(buffer)
        }

        fn interrupt(&self) {
            if !self.cancelled.swap(true, Ordering::SeqCst) {
                // SAFETY: the cancellation event is owned by this live connection.
                if unsafe { SetEvent(self.cancellation_event.as_raw_handle()) } == 0 {
                    eprintln!(
                        "dtop: failed to signal named-pipe cancellation: {}",
                        io::Error::last_os_error()
                    );
                }
            }
        }

        fn close(&self) {
            self.interrupt();
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
                    .custom_flags(FILE_FLAG_OVERLAPPED)
                    .open(&self.path)
                {
                    Ok(file) => {
                        let cancellation_event = event().map_err(|error| {
                            DockerError::Io(format!(
                                "create named-pipe cancellation event failed: {error}"
                            ))
                        })?;
                        return Ok(Arc::new(NamedPipeConnection {
                            file,
                            cancelled: AtomicBool::new(false),
                            cancellation_event,
                        }));
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
