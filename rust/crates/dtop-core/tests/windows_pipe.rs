#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dtop_core::transport::windows_pipe::NamedPipeTransport;
use dtop_core::transport::{Connection, Transport};
use dtop_core::DockerError;

#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        max_instances: u32,
        out_buffer_size: u32,
        in_buffer_size: u32,
        default_timeout: u32,
        security_attributes: *mut c_void,
    ) -> RawHandle;
    fn ConnectNamedPipe(pipe: RawHandle, overlapped: *mut c_void) -> i32;
}

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
const ERROR_PIPE_CONNECTED: i32 = 535;
const TIMEOUT: Duration = Duration::from_secs(5);

fn pipe_pair() -> (File, Arc<dyn Connection>) {
    static NEXT_PIPE: AtomicUsize = AtomicUsize::new(0);
    let path = format!(
        r"\\.\pipe\dtop-test-{}-{}",
        std::process::id(),
        NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
    );
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the name is NUL-terminated and the remaining arguments describe a local byte pipe.
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            0,
            1,
            4096,
            4096,
            0,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        handle as isize,
        -1,
        "create test pipe: {}",
        io::Error::last_os_error()
    );
    // SAFETY: CreateNamedPipeW returned a new, valid handle owned by this test.
    let server = unsafe { File::from_raw_handle(handle) };
    let client = NamedPipeTransport::new(&path)
        .connect()
        .expect("connect to test pipe");
    // SAFETY: the server handle is valid and is used synchronously.
    let connected = unsafe { ConnectNamedPipe(server.as_raw_handle(), std::ptr::null_mut()) };
    if connected == 0 {
        let error = io::Error::last_os_error();
        assert_eq!(
            error.raw_os_error(),
            Some(ERROR_PIPE_CONNECTED),
            "connect test pipe: {error}"
        );
    }
    (server, client)
}

fn start_read(client: Arc<dyn Connection>) -> Receiver<Result<Vec<u8>, DockerError>> {
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    thread::spawn(move || {
        started_tx.send(()).expect("announce the read");
        let _ = result_tx.send(client.read(64));
    });
    started_rx.recv_timeout(TIMEOUT).expect("start the read");
    result_rx
}

fn assert_pending<T>(result: &Receiver<T>) {
    assert!(
        matches!(
            result.recv_timeout(Duration::from_millis(100)),
            Err(RecvTimeoutError::Timeout)
        ),
        "operation completed before it could be cancelled"
    );
}

fn assert_cancelled<T>(result: Receiver<Result<T, DockerError>>) {
    assert!(
        matches!(
            result.recv_timeout(TIMEOUT),
            Ok(Err(DockerError::Cancelled))
        ),
        "operation did not report cancellation within {TIMEOUT:?}"
    );
}

#[test]
fn named_pipe_round_trips_bytes() {
    let (mut server, client) = pipe_pair();
    let (server_tx, server_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut request = [0; 4];
        let result = server
            .read_exact(&mut request)
            .and_then(|()| server.write_all(b"pong"))
            .map(|()| request);
        let _ = server_tx.send(result);
    });
    let (client_tx, client_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = client.write_all(b"ping").and_then(|()| client.read(4));
        let _ = client_tx.send(result);
    });
    assert_eq!(
        client_rx.recv_timeout(TIMEOUT).expect("read response"),
        Ok(b"pong".to_vec())
    );
    assert_eq!(
        server_rx
            .recv_timeout(TIMEOUT)
            .expect("read request")
            .expect("round-trip through server"),
        *b"ping"
    );
}

#[test]
fn interrupt_before_read_is_latched() {
    let (_server, client) = pipe_pair();
    client.interrupt();
    assert_cancelled(start_read(Arc::clone(&client)));
    assert_eq!(client.write_all(b"request"), Err(DockerError::Cancelled));
}

#[test]
fn interrupt_wakes_a_blocked_read() {
    let (_server, client) = pipe_pair();
    let result = start_read(Arc::clone(&client));
    assert_pending(&result);
    client.interrupt();
    assert_cancelled(result);
}

#[test]
fn close_wakes_a_blocked_read_and_is_idempotent() {
    let (_server, client) = pipe_pair();
    let result = start_read(Arc::clone(&client));
    assert_pending(&result);
    client.close();
    client.close();
    assert_cancelled(result);
    assert_cancelled(start_read(Arc::clone(&client)));
}

#[test]
fn interrupt_cancels_simultaneous_read_and_write() {
    let (_server, client) = pipe_pair();
    let read = start_read(Arc::clone(&client));
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let writer = Arc::clone(&client);
    thread::spawn(move || {
        let payload = vec![b'x'; 1024 * 1024];
        started_tx.send(()).expect("announce the write");
        let _ = result_tx.send(writer.write_all(&payload));
    });
    started_rx.recv_timeout(TIMEOUT).expect("start the write");
    assert_pending(&read);
    assert_pending(&result_rx);
    client.interrupt();
    assert_cancelled(read);
    assert_cancelled(result_rx);
}

#[test]
fn interrupt_racing_a_new_read_never_loses_the_cancellation() {
    for _ in 0..50 {
        let (_server, client) = pipe_pair();
        let result = start_read(Arc::clone(&client));
        client.interrupt();
        assert_cancelled(result);
    }
}

#[test]
fn peer_disconnect_is_reported_as_eof() {
    let (server, client) = pipe_pair();
    drop(server);
    let result = start_read(client);
    assert_eq!(
        result.recv_timeout(TIMEOUT).expect("read after disconnect"),
        Ok(Vec::new())
    );
}
