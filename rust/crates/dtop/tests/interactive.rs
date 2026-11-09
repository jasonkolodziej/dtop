//! Opt-in end-to-end coverage using a pseudo-terminal and a real Docker daemon.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::{c_char, c_int, c_void};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const COLUMNS: usize = 160;
const ROWS: usize = 46;
const TIMEOUT: Duration = Duration::from_secs(30);
const IMAGE: &str = "alpine:3.20";

#[repr(C)]
struct WindowSize {
    rows: u16,
    columns: u16,
    x_pixels: u16,
    y_pixels: u16,
}

#[cfg_attr(target_os = "linux", link(name = "util"))]
extern "C" {
    fn openpty(
        primary: *mut c_int,
        secondary: *mut c_int,
        name: *mut c_char,
        termios: *const c_void,
        window: *const WindowSize,
    ) -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
}

fn docker(host: &str) -> Command {
    let mut command = Command::new("docker");
    command.env_remove("DOCKER_CONTEXT").args(["--host", host]);
    if host.starts_with("unix://") {
        command
            .env_remove("DOCKER_TLS")
            .env_remove("DOCKER_TLS_VERIFY")
            .env_remove("DOCKER_CERT_PATH");
    }
    command
}

fn docker_host() -> String {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        if !host.is_empty() {
            return host;
        }
    }
    let output = Command::new("docker")
        .args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])
        .output()
        .expect("inspect the active Docker context");
    assert!(
        output.status.success(),
        "inspect Docker context: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let host = String::from_utf8(output.stdout).expect("Docker endpoint is UTF-8");
    let host = host.trim();
    assert!(!host.is_empty(), "active Docker context has no endpoint");
    host.to_string()
}

struct TestContainer {
    host: String,
    name: String,
    id: Option<String>,
}

impl TestContainer {
    fn start(host: &str, name: String, marker: &str) -> Self {
        let output = docker(host)
            .args([
                "create",
                "--name",
                &name,
                "--label",
                "dtop.test=1",
                IMAGE,
                "sh",
                "-c",
                &format!("echo {marker}; sleep 600"),
            ])
            .output()
            .expect("create the terminal test container");
        assert!(
            output.status.success(),
            "create terminal test container: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let id = String::from_utf8(output.stdout)
            .expect("container ID is UTF-8")
            .trim()
            .to_string();
        assert!(
            id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Docker returned an invalid container ID: {id:?}"
        );
        let container = Self {
            host: host.to_string(),
            name,
            id: Some(id),
        };
        let output = docker(host)
            .args(["start", container.id.as_deref().unwrap()])
            .output()
            .expect("start the terminal test container");
        assert!(
            output.status.success(),
            "start terminal test container: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        container
    }

    fn emit(&self, line: &str) {
        let id = self.id.as_deref().expect("test container is still present");
        let output = docker(&self.host)
            .args([
                "exec",
                id,
                "sh",
                "-c",
                &format!("echo {line} > /proc/1/fd/1"),
            ])
            .output()
            .expect("write to the test container's log");
        assert!(
            output.status.success(),
            "write test log: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn remove(&mut self) -> std::result::Result<(), String> {
        let Some(id) = self.id.as_deref() else {
            return Ok(());
        };
        let output = docker(&self.host)
            .args(["rm", "--force", id])
            .output()
            .map_err(|error| format!("could not remove test container {}: {error}", self.name))?;
        if !output.status.success() {
            return Err(format!(
                "could not remove test container {}: {}",
                self.name,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        self.id = None;
        Ok(())
    }
}

impl Drop for TestContainer {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            eprintln!("{error}");
        }
    }
}

struct TestConfig(PathBuf);

impl TestConfig {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{name}-config"));
        fs::create_dir(&path).expect("create isolated terminal test config");
        Self(path)
    }
}

impl Drop for TestConfig {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("could not remove test config {}: {error}", self.0.display());
        }
    }
}

/// Tracks cursor-addressed updates, rather than searching an accumulation of old frames.
struct Screen {
    cells: Vec<Vec<char>>,
    x: usize,
    y: usize,
    pending: Vec<u8>,
}

impl Screen {
    fn new() -> Self {
        Self {
            cells: vec![vec![' '; COLUMNS]; ROWS],
            x: 0,
            y: 0,
            pending: Vec::new(),
        }
    }

    fn feed(&mut self, data: &[u8]) {
        self.pending.extend_from_slice(data);
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i] == 0x1b {
                if i + 1 == self.pending.len() {
                    break;
                }
                match self.pending[i + 1] {
                    b'[' => {
                        let Some(end) = (i + 2..self.pending.len())
                            .find(|&j| (0x40..=0x7e).contains(&self.pending[j]))
                        else {
                            break;
                        };
                        let parameters = &self.pending[i + 2..end];
                        match self.pending[end] {
                            b'H' | b'f' => {
                                let parameters = std::str::from_utf8(parameters)
                                    .expect("terminal cursor parameters are ASCII");
                                let mut values = parameters.split(';');
                                self.y = values
                                    .next()
                                    .and_then(|v| v.parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .saturating_sub(1);
                                self.x = values
                                    .next()
                                    .and_then(|v| v.parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .saturating_sub(1);
                            }
                            b'J' if parameters == b"2" => {
                                for row in &mut self.cells {
                                    row.fill(' ');
                                }
                            }
                            _ => {}
                        }
                        i = end + 1;
                    }
                    b']' => {
                        let Some(end) =
                            (i + 2..self.pending.len()).find(|&j| self.pending[j] == 0x07)
                        else {
                            break;
                        };
                        i = end + 1;
                    }
                    _ => i += 2,
                }
                continue;
            }
            if self.pending[i] < b' ' {
                match self.pending[i] {
                    b'\r' => self.x = 0,
                    b'\n' => self.y = (self.y + 1).min(ROWS - 1),
                    _ => {}
                }
                i += 1;
                continue;
            }
            let remaining = &self.pending[i..];
            let valid = match std::str::from_utf8(remaining) {
                Ok(text) => text,
                Err(error) if error.valid_up_to() > 0 => {
                    std::str::from_utf8(&remaining[..error.valid_up_to()]).unwrap()
                }
                Err(error) if error.error_len().is_none() => break,
                Err(error) => panic!("invalid terminal UTF-8: {error}"),
            };
            let character = valid.chars().next().unwrap();
            i += character.len_utf8();
            let width = dtop_ui::text::char_width(character);
            if width > 0 && self.y < ROWS && self.x < COLUMNS {
                self.cells[self.y][self.x] = character;
                if width == 2 && self.x + 1 < COLUMNS {
                    self.cells[self.y][self.x + 1] = ' ';
                }
            }
            self.x += width;
        }
        self.pending.drain(..i);
    }

    fn find(&self, text: &str) -> Option<(usize, usize)> {
        let characters: Vec<_> = text.chars().collect();
        self.cells.iter().enumerate().find_map(|(y, row)| {
            row.windows(characters.len())
                .position(|window| window == characters)
                .map(|x| (x, y))
        })
    }

    fn contents(&self) -> String {
        self.cells
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct Terminal {
    primary: File,
    secondary: Option<File>,
    process: Child,
    receiver: Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
    screen: Screen,
    output: Vec<u8>,
    original_mode: String,
}

impl Terminal {
    fn start(host: &str, second_host: &str, config: &TestConfig) -> Self {
        let window = WindowSize {
            rows: ROWS as u16,
            columns: COLUMNS as u16,
            x_pixels: 0,
            y_pixels: 0,
        };
        let mut primary = -1;
        let mut secondary = -1;
        let result = unsafe {
            openpty(
                &mut primary,
                &mut secondary,
                ptr::null_mut(),
                ptr::null(),
                &window,
            )
        };
        assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
        let primary = unsafe { File::from_raw_fd(primary) };
        let secondary = unsafe { File::from_raw_fd(secondary) };
        let original_mode = Self::mode(&secondary);
        let process = Command::new(env!("CARGO_BIN_EXE_dtop"))
            .args([
                "--host",
                &format!("first={host}"),
                "--host",
                &format!("second={second_host}"),
                "--no-gpu",
                "--interval",
                "0.25",
            ])
            .env("XDG_CONFIG_HOME", &config.0)
            .env("TERM", "xterm-256color")
            .env_remove("DOCKER_CONTEXT")
            .stdin(Stdio::from(secondary.try_clone().unwrap()))
            .stdout(Stdio::from(secondary.try_clone().unwrap()))
            .stderr(Stdio::from(secondary.try_clone().unwrap()))
            .spawn()
            .expect("start dtop in a pseudo-terminal");
        let mut input = primary.try_clone().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut bytes = [0; 8192];
            while let Ok(count) = input.read(&mut bytes) {
                if count == 0 || sender.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            primary,
            secondary: Some(secondary),
            process,
            receiver,
            reader: Some(reader),
            screen: Screen::new(),
            output: Vec::new(),
            original_mode,
        }
    }

    fn mode(secondary: &File) -> String {
        let output = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::from(secondary.try_clone().unwrap()))
            .output()
            .expect("inspect pseudo-terminal mode");
        assert!(
            output.status.success(),
            "stty: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn send(&mut self, bytes: &[u8]) {
        self.primary.write_all(bytes).expect("write terminal input");
    }

    fn receive(&mut self, timeout: Duration) -> bool {
        match self.receiver.recv_timeout(timeout) {
            Ok(bytes) => {
                self.screen.feed(&bytes);
                self.output.extend_from_slice(&bytes);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => panic!(
                "dtop stopped producing terminal output:\n{}",
                self.screen.contents()
            ),
        }
    }

    fn wait_for(&mut self, text: &str) -> (usize, usize) {
        self.wait_until(text, true);
        self.screen.find(text).unwrap()
    }

    fn wait_until(&mut self, text: &str, present: bool) {
        let deadline = Instant::now() + TIMEOUT;
        while self.screen.find(text).is_some() != present {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                panic!(
                    "timed out waiting for {text:?} present={present}:\n{}",
                    self.screen.contents()
                );
            };
            self.receive(remaining);
        }
    }

    fn drain_for(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if !self.receive(remaining) {
                break;
            }
        }
    }

    fn terminate(&mut self) {
        let pid = c_int::try_from(self.process.id()).expect("process ID fits in c_int");
        assert_eq!(
            unsafe { kill(pid, 15) },
            0,
            "signal dtop: {}",
            std::io::Error::last_os_error()
        );
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.process.try_wait().expect("wait for dtop") {
                assert!(status.success(), "dtop exited after SIGTERM: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "dtop did not exit after SIGTERM");
            self.receive(Duration::from_millis(50));
        }
        let deadline = Instant::now() + TIMEOUT;
        while !self.output.windows(8).any(|w| w == b"\x1b[?1049l") {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("dtop did not leave the alternate screen");
            self.receive(remaining);
        }
        assert_eq!(
            Self::mode(self.secondary.as_ref().unwrap()),
            self.original_mode,
            "dtop did not restore the terminal mode"
        );
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
            let _ = self.process.wait();
        }
        self.secondary.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn terminal_screen_tracks_incremental_updates_and_split_utf8() {
    let mut screen = Screen::new();
    screen.feed(b"\x1b[2J\x1b[2;4Halpha");
    screen.feed(b"\x1b[2;4Hbravo\x1b[3;1H\xe2\x96");
    screen.feed(b"\xb8 host");
    assert_eq!(screen.find("alpha"), None);
    assert_eq!(screen.find("bravo"), Some((3, 1)));
    assert_eq!(screen.find("▸ host"), Some((0, 2)));
}

#[test]
fn interactive_hosts_logs_themes_and_signal_cleanup() {
    if std::env::var("DTOP_INTEGRATION").as_deref() != Ok("1") {
        eprintln!("set DTOP_INTEGRATION=1 to run the real-terminal test");
        return;
    }
    let host = docker_host();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let name = format!("dtpt-{:x}-{:x}", std::process::id(), nanos as u32);
    let config = TestConfig::new(&name);
    let mut container = TestContainer::start(&host, name.clone(), "dtop-pty-ready");
    let mut terminal = Terminal::start(&host, &host, &config);

    terminal.wait_for("first");
    terminal.wait_for("second");
    assert_ne!(
        Terminal::mode(terminal.secondary.as_ref().unwrap()),
        terminal.original_mode,
        "dtop should enter raw mode"
    );
    terminal.send(format!("/{name}\r").as_bytes());
    terminal.wait_for(&format!("filter \"{name}\""));
    terminal.wait_for(&name);
    terminal.send(b"vv");
    terminal.wait_for("dtop-pty-ready");

    terminal.send(b" ");
    terminal.wait_for("updates paused");
    container.emit("dtop-pty-buffered");
    terminal.drain_for(Duration::from_millis(500));
    assert!(
        terminal.screen.find("dtop-pty-buffered").is_none(),
        "paused logs should not appear before resuming"
    );
    terminal.send(b" ");
    terminal.wait_for("dtop-pty-buffered");

    terminal.send(b"H");
    terminal.wait_for("hosts (2)");
    terminal.send(b"2");
    terminal.wait_for("Host: second");
    terminal.wait_for("dtop-pty-ready");

    let (x, y) = terminal.wait_for("first");
    terminal.send(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
    terminal.send(b"H");
    terminal.wait_for("▸ first");
    terminal.send(b"\x1b");
    terminal.wait_until("hosts (2)", false);

    terminal.send(b"T");
    terminal.wait_for("theme · harbor");
    terminal.send(b"jjjl");
    terminal.wait_for("unsaved");
    terminal.send(b"\x1b");
    terminal.wait_for("Discard unsaved theme changes?");
    terminal.send(b"y");
    terminal.wait_for("Theme changes discarded");

    terminal.send(b"T");
    terminal.wait_for("theme · harbor");
    terminal.send(b"jjjl");
    terminal.wait_for("unsaved");
    terminal.send(b"s");
    terminal.wait_for("save as: harbor-custom");
    terminal.send(b"\r");
    terminal.wait_for("Saved theme 'harbor-custom'");
    let config_text = fs::read_to_string(config.0.join("dtop/dtop.conf"))
        .expect("read the persisted test theme setting");
    assert!(config_text.contains("theme = harbor-custom"));
    assert!(
        config.0.join("dtop/themes/harbor-custom.theme").is_file(),
        "the edited theme should be saved separately"
    );

    terminal.terminate();
    container
        .remove()
        .expect("remove the terminal test container");
}

#[test]
fn distinct_daemons_keep_paused_logs_on_the_original_host() {
    if std::env::var("DTOP_INTEGRATION").as_deref() != Ok("1") {
        return;
    }
    let Ok(second_host) = std::env::var("DTOP_SECONDARY_HOST") else {
        eprintln!("set DTOP_SECONDARY_HOST to test two distinct Docker daemons");
        return;
    };
    let host = docker_host();
    assert_ne!(host, second_host, "the two Docker endpoints must differ");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let name = format!("dtmh-{:x}-{:x}", std::process::id(), nanos as u32);
    let config = TestConfig::new(&name);
    let mut first = TestContainer::start(&host, format!("{name}-a"), "dtop-pty-host-a");
    let mut second = TestContainer::start(&second_host, format!("{name}-b"), "dtop-pty-host-b");
    let mut terminal = Terminal::start(&host, &second_host, &config);

    terminal.wait_for("first");
    terminal.wait_for("second");
    terminal.send(format!("/{name}\r").as_bytes());
    terminal.wait_for(&format!("filter \"{name}\""));
    terminal.wait_for(&format!("{name}-a"));
    terminal.send(b"vv");
    terminal.wait_for("dtop-pty-host-a");
    terminal.send(b" ");
    terminal.wait_for("updates paused");
    first.emit("dtop-pty-host-a-buffered");
    terminal.drain_for(Duration::from_millis(500));

    terminal.send(b"H");
    terminal.wait_for("hosts (2)");
    terminal.send(b"2");
    terminal.wait_for("Host: second");
    terminal.send(b" ");
    terminal.wait_for(&format!("{name}-b"));
    terminal.wait_for("dtop-pty-host-b");
    assert!(
        terminal.screen.find("dtop-pty-host-a-buffered").is_none(),
        "buffered logs from the first daemon crossed into the second host"
    );
    terminal.terminate();
    first.remove().expect("remove the first host's container");
    second.remove().expect("remove the second host's container");
}
