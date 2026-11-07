#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#elseif os(Windows)
import WinSDK
import ucrt
#endif

import Foundation

#if os(Windows)
/// Win32 `BOOL` imports as `WindowsBool` on older toolchains and as `Bool` from Swift 6.2.
@inline(__always) func win32Succeeded(_ value: Bool) -> Bool { value }
#if !compiler(>=6.2)
@inline(__always) func win32Succeeded(_ value: WindowsBool) -> Bool { value.boolValue }
#endif
#endif

#if !os(Windows)
private func systemRead(_ fd: Int32, _ buffer: UnsafeMutableRawPointer?, _ count: Int) -> Int {
    #if canImport(Darwin)
    return Darwin.read(fd, buffer, count)
    #elseif canImport(Glibc)
    return Glibc.read(fd, buffer, count)
    #else
    return Musl.read(fd, buffer, count)
    #endif
}
#endif

public struct TerminalSize: Sendable, Equatable {
    public var columns: Int
    public var rows: Int

    public init(columns: Int, rows: Int) {
        self.columns = columns
        self.rows = rows
    }

    public static let fallback = TerminalSize(columns: 80, rows: 24)

    /// Uses `COLUMNS`/`LINES` when the OS query is unavailable (e.g. output is piped).
    public static func fromEnvironment(_ environment: [String: String]) -> TerminalSize? {
        guard let columns = environment["COLUMNS"].flatMap(Int.init), columns > 0,
              let rows = environment["LINES"].flatMap(Int.init), rows > 0 else { return nil }
        return TerminalSize(columns: columns, rows: rows)
    }
}

public enum TerminalError: Error, Equatable, CustomStringConvertible {
    case notATerminal
    case configurationFailed(String)

    public var description: String {
        switch self {
        case .notATerminal:
            return "dtop needs an interactive terminal — run it directly in a terminal, or use --once for plain output"
        case .configurationFailed(let reason):
            return "Could not configure the terminal: \(reason)"
        }
    }
}

/// Raw-mode terminal I/O for macOS/Linux (termios) and Windows (console API with VT sequences).
public final class Terminal: @unchecked Sendable {
    // Raw-mode state is only touched by the thread that owns the UI; `lock` protects `isRaw`
    // because `restore()` may also run from an exit path.
    private let lock = NSLock()
    private var isRaw = false

    #if os(Windows)
    private var originalInputMode: DWORD = 0
    private var originalOutputMode: DWORD = 0
    private var originalOutputCodePage: UINT = 0
    private var originalInputCodePage: UINT = 0
    #else
    private var original = termios()
    #endif

    public init() {}

    deinit {
        restore()
    }

    public static var isInteractive: Bool {
        #if os(Windows)
        return _isatty(0) != 0 && _isatty(1) != 0
        #else
        return isatty(STDIN_FILENO) != 0 && isatty(STDOUT_FILENO) != 0
        #endif
    }

    public func enableRawMode() throws {
        lock.lock()
        defer { lock.unlock() }
        guard !isRaw else { return }
        #if os(Windows)
        let input = GetStdHandle(STD_INPUT_HANDLE)
        let output = GetStdHandle(STD_OUTPUT_HANDLE)
        guard win32Succeeded(GetConsoleMode(input, &originalInputMode)),
              win32Succeeded(GetConsoleMode(output, &originalOutputMode)) else {
            throw TerminalError.notATerminal
        }
        let enableProcessedInput: DWORD = 0x0001
        let enableLineInput: DWORD = 0x0002
        let enableEchoInput: DWORD = 0x0004
        let enableQuickEdit: DWORD = 0x0040
        let enableExtendedFlags: DWORD = 0x0080
        let enableVirtualTerminalInput: DWORD = 0x0200
        let enableProcessedOutput: DWORD = 0x0001
        let enableVirtualTerminalProcessing: DWORD = 0x0004

        var inputMode = originalInputMode
        inputMode &= ~(enableProcessedInput | enableLineInput | enableEchoInput | enableQuickEdit)
        inputMode |= enableExtendedFlags | enableVirtualTerminalInput
        guard win32Succeeded(SetConsoleMode(input, inputMode)) else {
            throw TerminalError.configurationFailed("SetConsoleMode(input) failed with error \(GetLastError())")
        }
        let outputMode = originalOutputMode | enableProcessedOutput | enableVirtualTerminalProcessing
        guard win32Succeeded(SetConsoleMode(output, outputMode)) else {
            _ = SetConsoleMode(input, originalInputMode)
            throw TerminalError.configurationFailed(
                "SetConsoleMode(output) failed with error \(GetLastError()) — Windows 10 1809 or newer is required")
        }
        originalOutputCodePage = GetConsoleOutputCP()
        originalInputCodePage = GetConsoleCP()
        _ = SetConsoleOutputCP(65001)
        _ = SetConsoleCP(65001)
        #else
        guard tcgetattr(STDIN_FILENO, &original) == 0 else { throw TerminalError.notATerminal }
        var raw = original
        raw.c_iflag &= ~tcflag_t(BRKINT | ICRNL | INPCK | ISTRIP | IXON)
        raw.c_oflag &= ~tcflag_t(OPOST)
        raw.c_cflag |= tcflag_t(CS8)
        raw.c_lflag &= ~tcflag_t(ECHO | ICANON | IEXTEN | ISIG)
        withUnsafeMutableBytes(of: &raw.c_cc) { bytes in
            bytes[Int(VMIN)] = 1
            bytes[Int(VTIME)] = 0
        }
        guard tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw) == 0 else {
            throw TerminalError.configurationFailed(String(cString: strerror(errno)))
        }
        #endif
        isRaw = true
    }

    public func restore() {
        lock.lock()
        defer { lock.unlock() }
        guard isRaw else { return }
        #if os(Windows)
        _ = SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), originalInputMode)
        _ = SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), originalOutputMode)
        if originalOutputCodePage != 0 { _ = SetConsoleOutputCP(originalOutputCodePage) }
        if originalInputCodePage != 0 { _ = SetConsoleCP(originalInputCodePage) }
        #else
        _ = tcsetattr(STDIN_FILENO, TCSAFLUSH, &original)
        #endif
        isRaw = false
    }

    public func size(environment: [String: String] = ProcessInfo.processInfo.environment) -> TerminalSize {
        #if os(Windows)
        var info = CONSOLE_SCREEN_BUFFER_INFO()
        if win32Succeeded(GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &info)) {
            let columns = Int(info.srWindow.Right) - Int(info.srWindow.Left) + 1
            let rows = Int(info.srWindow.Bottom) - Int(info.srWindow.Top) + 1
            if columns > 0, rows > 0 { return TerminalSize(columns: columns, rows: rows) }
        }
        #else
        var window = winsize()
        if ioctl(STDOUT_FILENO, UInt(TIOCGWINSZ), &window) == 0, window.ws_col > 0, window.ws_row > 0 {
            return TerminalSize(columns: Int(window.ws_col), rows: Int(window.ws_row))
        }
        #endif
        return TerminalSize.fromEnvironment(environment) ?? .fallback
    }

    public func write(_ text: String) {
        guard !text.isEmpty else { return }
        FileHandle.standardOutput.write(Data(text.utf8))
    }

    /// Blocks until input arrives. Returns an empty array on EOF or error.
    public func read(maxLength: Int = 4096) -> [UInt8] {
        var buffer = [UInt8](repeating: 0, count: maxLength)
        #if os(Windows)
        var count: DWORD = 0
        let ok = buffer.withUnsafeMutableBytes { raw in
            ReadFile(GetStdHandle(STD_INPUT_HANDLE), raw.baseAddress, DWORD(raw.count), &count, nil)
        }
        guard win32Succeeded(ok) else { return [] }
        return Array(buffer[..<Int(count)])
        #else
        while true {
            let count = buffer.withUnsafeMutableBytes { raw in
                systemRead(STDIN_FILENO, raw.baseAddress, raw.count)
            }
            if count < 0, errno == EINTR { continue }
            guard count > 0 else { return [] }
            return Array(buffer[..<count])
        }
        #endif
    }

    /// Switches to the alternate screen with mouse reporting, restoring everything on `leave`.
    public func enterFullscreen(mouse: Bool) {
        var sequence = TerminalSequence.enterAlternateScreen + TerminalSequence.hideCursor
            + TerminalSequence.disableLineWrap + TerminalSequence.enableBracketedPaste + TerminalSequence.clearScreen
        if mouse { sequence += TerminalSequence.enableMouse }
        write(sequence)
    }

    public func leaveFullscreen() {
        write(TerminalSequence.disableMouse + TerminalSequence.disableBracketedPaste + TerminalSequence.reset
            + TerminalSequence.enableLineWrap + TerminalSequence.showCursor + TerminalSequence.exitAlternateScreen)
    }
}
