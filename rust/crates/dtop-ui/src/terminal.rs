//! Raw-mode terminal I/O: termios on macOS and Linux, the console API with VT sequences on
//! Windows. The C APIs are declared by hand (no `libc` crate, ADR 0001); struct layouts follow
//! each platform's headers.

use std::collections::HashMap;
use std::fmt;
use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use crate::render::seq;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: i32,
    pub rows: i32,
}

impl TerminalSize {
    pub const FALLBACK: TerminalSize = TerminalSize {
        columns: 80,
        rows: 24,
    };

    /// `COLUMNS`/`LINES`, for when the OS query is unavailable (e.g. piped output).
    pub fn from_environment(environment: &HashMap<String, String>) -> Option<TerminalSize> {
        let columns: i32 = environment
            .get("COLUMNS")?
            .parse()
            .ok()
            .filter(|c| *c > 0)?;
        let rows: i32 = environment.get("LINES")?.parse().ok().filter(|r| *r > 0)?;
        Some(TerminalSize { columns, rows })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalError {
    NotATerminal,
    ConfigurationFailed(String),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminalError::NotATerminal => f.write_str(
                "dtop needs an interactive terminal — run it directly in a terminal, or use --once for plain output",
            ),
            TerminalError::ConfigurationFailed(reason) => write!(f, "Could not configure the terminal: {reason}"),
        }
    }
}

impl std::error::Error for TerminalError {}

#[cfg(unix)]
pub(crate) mod sys {
    use std::ffi::{c_int, c_ulong, c_void};

    #[cfg(target_os = "macos")]
    mod layout {
        pub type TcFlag = u64;
        pub const NCCS: usize = 20;
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct Termios {
            pub c_iflag: TcFlag,
            pub c_oflag: TcFlag,
            pub c_cflag: TcFlag,
            pub c_lflag: TcFlag,
            pub c_cc: [u8; NCCS],
            pub c_ispeed: u64,
            pub c_ospeed: u64,
        }
        pub const BRKINT: TcFlag = 0x2;
        pub const ICRNL: TcFlag = 0x100;
        pub const INPCK: TcFlag = 0x10;
        pub const ISTRIP: TcFlag = 0x20;
        pub const IXON: TcFlag = 0x200;
        pub const OPOST: TcFlag = 0x1;
        pub const CS8: TcFlag = 0x300;
        pub const ECHO: TcFlag = 0x8;
        pub const ICANON: TcFlag = 0x100;
        pub const IEXTEN: TcFlag = 0x400;
        pub const ISIG: TcFlag = 0x80;
        pub const VMIN: usize = 16;
        pub const VTIME: usize = 17;
        pub const TIOCGWINSZ: std::ffi::c_ulong = 0x4008_7468;
    }

    #[cfg(not(target_os = "macos"))]
    mod layout {
        // glibc and musl on Linux.
        pub type TcFlag = u32;
        pub const NCCS: usize = 32;
        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct Termios {
            pub c_iflag: TcFlag,
            pub c_oflag: TcFlag,
            pub c_cflag: TcFlag,
            pub c_lflag: TcFlag,
            pub c_line: u8,
            pub c_cc: [u8; NCCS],
            pub c_ispeed: u32,
            pub c_ospeed: u32,
        }
        pub const BRKINT: TcFlag = 0o2;
        pub const ICRNL: TcFlag = 0o400;
        pub const INPCK: TcFlag = 0o20;
        pub const ISTRIP: TcFlag = 0o40;
        pub const IXON: TcFlag = 0o2000;
        pub const OPOST: TcFlag = 0o1;
        pub const CS8: TcFlag = 0o60;
        pub const ECHO: TcFlag = 0o10;
        pub const ICANON: TcFlag = 0o2;
        pub const IEXTEN: TcFlag = 0o100000;
        pub const ISIG: TcFlag = 0o1;
        pub const VMIN: usize = 6;
        pub const VTIME: usize = 5;
        pub const TIOCGWINSZ: std::ffi::c_ulong = 0x5413;
    }

    pub use layout::*;

    #[repr(C)]
    #[derive(Default)]
    pub struct Winsize {
        pub ws_row: u16,
        pub ws_col: u16,
        pub ws_xpixel: u16,
        pub ws_ypixel: u16,
    }

    pub const TCSAFLUSH: c_int = 2;

    extern "C" {
        pub fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
        pub fn tcsetattr(fd: c_int, action: c_int, termios: *const Termios) -> c_int;
        pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
        pub fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
        pub fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize;
    }

    pub fn zeroed() -> Termios {
        // SAFETY: Termios is plain old data; all-zero is a valid bit pattern.
        unsafe { std::mem::zeroed() }
    }
}

#[cfg(windows)]
pub(crate) mod sys {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;
    pub const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    pub const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Coord {
        pub x: i16,
        pub y: i16,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct SmallRect {
        pub left: i16,
        pub top: i16,
        pub right: i16,
        pub bottom: i16,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct ScreenBufferInfo {
        pub size: Coord,
        pub cursor: Coord,
        pub attributes: u16,
        pub window: SmallRect,
        pub maximum_window_size: Coord,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetStdHandle(which: u32) -> Handle;
        pub fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
        pub fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
        pub fn GetConsoleScreenBufferInfo(handle: Handle, info: *mut ScreenBufferInfo) -> i32;
        pub fn GetConsoleOutputCP() -> u32;
        pub fn GetConsoleCP() -> u32;
        pub fn SetConsoleOutputCP(page: u32) -> i32;
        pub fn SetConsoleCP(page: u32) -> i32;
        pub fn ReadFile(
            handle: Handle,
            buffer: *mut c_void,
            length: u32,
            read: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        pub fn GetLastError() -> u32;
    }
}

#[cfg(unix)]
struct SavedMode(sys::Termios);
#[cfg(windows)]
#[derive(Clone, Copy)]
struct SavedMode {
    input_mode: u32,
    output_mode: u32,
    output_code_page: u32,
    input_code_page: u32,
}

// SAFETY: the saved mode is plain data.
unsafe impl Send for SavedMode {}

/// The mode to restore, shared with signal handlers and exit paths.
static SAVED: Mutex<Option<SavedMode>> = Mutex::new(None);

pub struct Terminal;

impl Default for Terminal {
    fn default() -> Self {
        Terminal
    }
}

impl Terminal {
    pub fn new() -> Terminal {
        Terminal
    }

    pub fn is_interactive() -> bool {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }

    #[cfg(unix)]
    pub fn enable_raw_mode(&self) -> Result<(), TerminalError> {
        use sys::*;
        let mut saved = SAVED
            .lock()
            .map_err(|_| TerminalError::ConfigurationFailed("lock poisoned".into()))?;
        if saved.is_some() {
            return Ok(());
        }
        let mut original = zeroed();
        // SAFETY: valid pointer to a correctly laid-out termios.
        if unsafe { tcgetattr(0, &mut original) } != 0 {
            return Err(TerminalError::NotATerminal);
        }
        let mut raw = original;
        raw.c_iflag &= !(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
        raw.c_oflag &= !OPOST;
        raw.c_cflag |= CS8;
        raw.c_lflag &= !(ECHO | ICANON | IEXTEN | ISIG);
        raw.c_cc[VMIN] = 1;
        raw.c_cc[VTIME] = 0;
        // SAFETY: as above.
        if unsafe { tcsetattr(0, TCSAFLUSH, &raw) } != 0 {
            return Err(TerminalError::ConfigurationFailed(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        *saved = Some(SavedMode(original));
        Ok(())
    }

    #[cfg(windows)]
    pub fn enable_raw_mode(&self) -> Result<(), TerminalError> {
        use sys::*;
        let mut saved = SAVED
            .lock()
            .map_err(|_| TerminalError::ConfigurationFailed("lock poisoned".into()))?;
        if saved.is_some() {
            return Ok(());
        }
        // SAFETY: console API calls with valid handles and out-pointers.
        unsafe {
            let input = GetStdHandle(STD_INPUT_HANDLE);
            let output = GetStdHandle(STD_OUTPUT_HANDLE);
            let (mut input_mode, mut output_mode) = (0u32, 0u32);
            if GetConsoleMode(input, &mut input_mode) == 0
                || GetConsoleMode(output, &mut output_mode) == 0
            {
                return Err(TerminalError::NotATerminal);
            }
            const PROCESSED_INPUT: u32 = 0x0001;
            const LINE_INPUT: u32 = 0x0002;
            const ECHO_INPUT: u32 = 0x0004;
            const QUICK_EDIT: u32 = 0x0040;
            const EXTENDED_FLAGS: u32 = 0x0080;
            const VT_INPUT: u32 = 0x0200;
            const PROCESSED_OUTPUT: u32 = 0x0001;
            const VT_PROCESSING: u32 = 0x0004;
            let raw_input = (input_mode
                & !(PROCESSED_INPUT | LINE_INPUT | ECHO_INPUT | QUICK_EDIT))
                | EXTENDED_FLAGS
                | VT_INPUT;
            if SetConsoleMode(input, raw_input) == 0 {
                return Err(TerminalError::ConfigurationFailed(format!(
                    "SetConsoleMode(input) failed with error {}",
                    GetLastError()
                )));
            }
            if SetConsoleMode(output, output_mode | PROCESSED_OUTPUT | VT_PROCESSING) == 0 {
                SetConsoleMode(input, input_mode);
                return Err(TerminalError::ConfigurationFailed(format!(
                    "SetConsoleMode(output) failed with error {} — Windows 10 1809 or newer is required",
                    GetLastError()
                )));
            }
            let mode = SavedMode {
                input_mode,
                output_mode,
                output_code_page: GetConsoleOutputCP(),
                input_code_page: GetConsoleCP(),
            };
            SetConsoleOutputCP(65001);
            SetConsoleCP(65001);
            *saved = Some(mode);
        }
        Ok(())
    }

    /// Restores the original mode. Safe to call more than once and from exit paths.
    pub fn restore(&self) {
        restore_saved_mode();
    }

    pub fn size(&self, environment: &HashMap<String, String>) -> TerminalSize {
        Self::query_size()
            .or_else(|| TerminalSize::from_environment(environment))
            .unwrap_or(TerminalSize::FALLBACK)
    }

    #[cfg(unix)]
    fn query_size() -> Option<TerminalSize> {
        let mut window = sys::Winsize::default();
        // SAFETY: TIOCGWINSZ fills a winsize struct.
        let ok = unsafe { sys::ioctl(1, sys::TIOCGWINSZ, &mut window as *mut sys::Winsize) } == 0;
        (ok && window.ws_col > 0 && window.ws_row > 0).then(|| TerminalSize {
            columns: i32::from(window.ws_col),
            rows: i32::from(window.ws_row),
        })
    }

    #[cfg(windows)]
    fn query_size() -> Option<TerminalSize> {
        let mut info = sys::ScreenBufferInfo::default();
        // SAFETY: valid handle and out-pointer.
        let ok = unsafe {
            sys::GetConsoleScreenBufferInfo(sys::GetStdHandle(sys::STD_OUTPUT_HANDLE), &mut info)
        } != 0;
        let columns = i32::from(info.window.right) - i32::from(info.window.left) + 1;
        let rows = i32::from(info.window.bottom) - i32::from(info.window.top) + 1;
        (ok && columns > 0 && rows > 0).then_some(TerminalSize { columns, rows })
    }

    pub fn write(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    /// Blocks until input arrives. Empty on EOF or error.
    #[cfg(unix)]
    pub fn read(&self, max: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; max.max(1)];
        loop {
            // SAFETY: the buffer is valid for `len` bytes.
            let count = unsafe { sys::read(0, buffer.as_mut_ptr().cast(), buffer.len()) };
            if count < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            if count <= 0 {
                return Vec::new();
            }
            buffer.truncate(count as usize);
            return buffer;
        }
    }

    #[cfg(windows)]
    pub fn read(&self, max: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; max.max(1)];
        let mut count = 0u32;
        // SAFETY: the buffer is valid for `len` bytes.
        let ok = unsafe {
            sys::ReadFile(
                sys::GetStdHandle(sys::STD_INPUT_HANDLE),
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Vec::new();
        }
        buffer.truncate(count as usize);
        buffer
    }

    /// The alternate screen, hidden cursor, no line wrap, bracketed paste and optional mouse.
    pub fn enter_fullscreen(&self, mouse: bool) {
        let mut sequence = format!(
            "{}{}{}{}{}",
            seq::ENTER_ALTERNATE_SCREEN,
            seq::HIDE_CURSOR,
            seq::DISABLE_LINE_WRAP,
            seq::ENABLE_BRACKETED_PASTE,
            seq::CLEAR_SCREEN
        );
        if mouse {
            sequence.push_str(seq::ENABLE_MOUSE);
        }
        self.write(&sequence);
    }

    pub fn leave_fullscreen(&self) {
        self.write(&leave_sequence());
    }
}

pub fn leave_sequence() -> String {
    format!(
        "{}{}{}{}{}{}",
        seq::DISABLE_MOUSE,
        seq::DISABLE_BRACKETED_PASTE,
        seq::RESET,
        seq::ENABLE_LINE_WRAP,
        seq::SHOW_CURSOR,
        seq::EXIT_ALTERNATE_SCREEN
    )
}

/// Restores the saved terminal mode, if any. Uses `try_lock` so a signal arriving while the
/// lock is held cannot deadlock.
pub fn restore_saved_mode() {
    let Ok(mut saved) = SAVED.try_lock() else {
        return;
    };
    let Some(mode) = saved.take() else { return };
    #[cfg(unix)]
    {
        // SAFETY: restores a termios previously read from the same fd.
        unsafe {
            sys::tcsetattr(0, sys::TCSAFLUSH, &mode.0);
        }
    }
    #[cfg(windows)]
    {
        // SAFETY: console API calls with valid handles.
        unsafe {
            sys::SetConsoleMode(sys::GetStdHandle(sys::STD_INPUT_HANDLE), mode.input_mode);
            sys::SetConsoleMode(sys::GetStdHandle(sys::STD_OUTPUT_HANDLE), mode.output_mode);
            if mode.output_code_page != 0 {
                sys::SetConsoleOutputCP(mode.output_code_page);
            }
            if mode.input_code_page != 0 {
                sys::SetConsoleCP(mode.input_code_page);
            }
        }
    }
}

/// Writes raw bytes to stdout without locks or allocation (usable from signal handlers).
pub fn write_raw_stdout(bytes: &[u8]) {
    #[cfg(unix)]
    {
        // SAFETY: write(2) is async-signal-safe and the slice is valid.
        unsafe {
            sys::write(1, bytes.as_ptr().cast(), bytes.len());
        }
    }
    #[cfg(windows)]
    {
        let _ = std::io::stdout().write_all(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_falls_back_to_environment() {
        let env: HashMap<String, String> = [
            ("COLUMNS".to_string(), "120".to_string()),
            ("LINES".to_string(), "40".to_string()),
        ]
        .into();
        assert_eq!(
            TerminalSize::from_environment(&env),
            Some(TerminalSize {
                columns: 120,
                rows: 40
            })
        );
        assert_eq!(TerminalSize::from_environment(&HashMap::new()), None);
    }

    #[test]
    fn leave_sequence_restores_everything() {
        let s = leave_sequence();
        assert!(s.contains(seq::SHOW_CURSOR) && s.ends_with(seq::EXIT_ALTERNATE_SCREEN));
    }

    #[cfg(unix)]
    #[test]
    fn termios_layout_matches_the_platform() {
        let expected = if cfg!(target_os = "macos") { 72 } else { 60 };
        assert_eq!(std::mem::size_of::<sys::Termios>(), expected);
    }
}
