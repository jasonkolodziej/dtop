/// Decodes raw terminal input (VT/xterm sequences, UTF-8, SGR and X10 mouse reports, bracketed
/// paste) into events. Incremental: partial sequences are kept until the next `feed`, and
/// `flush()` resolves a lone ESC once the caller knows no more bytes are coming.
public struct InputParser: Sendable {
    private var buffer: [UInt8] = []
    private var pasteBuffer: [UInt8]?

    static let escapeByte: UInt8 = 0x1B
    static let pasteStart: [UInt8] = Array("\u{1B}[200~".utf8)
    static let pasteEnd: [UInt8] = Array("\u{1B}[201~".utf8)

    public init() {}

    /// True when bytes are buffered waiting for the rest of a sequence.
    public var hasPendingInput: Bool { !buffer.isEmpty || pasteBuffer != nil }

    public mutating func feed(_ bytes: [UInt8]) -> [InputEvent] {
        buffer.append(contentsOf: bytes)
        var events: [InputEvent] = []
        while !buffer.isEmpty {
            if pasteBuffer != nil {
                guard consumePaste(into: &events) else { break }
                continue
            }
            switch parseNext() {
            case .event(let event, let length):
                buffer.removeSubrange(..<length)
                if let event { events.append(event) }
            case .incomplete:
                return events
            case .pasteBegins(let length):
                buffer.removeSubrange(..<length)
                pasteBuffer = []
            }
        }
        return events
    }

    /// True while a CSI/SS3 sequence (e.g. a mouse report) or a bracketed paste has started but
    /// not finished. Such input deserves a longer wait than a lone ESC: over SSH a report can
    /// arrive in two reads, and resolving it early would turn its tail into keystrokes.
    public var hasIncompleteSequence: Bool {
        if pasteBuffer != nil { return true }
        guard buffer.count >= 2, buffer[0] == Self.escapeByte else { return false }
        return buffer[1] == UInt8(ascii: "[") || buffer[1] == UInt8(ascii: "O")
    }

    /// Suggested wait before calling `flush()` for the current pending input.
    public var flushDelay: Double { hasIncompleteSequence ? 0.5 : 0.03 }

    /// Resolves whatever is pending: a lone ESC becomes `.escape`, a dangling `ESC x` becomes
    /// Alt+x, and an unterminated paste is delivered as-is. An unfinished CSI/SS3 sequence longer
    /// than its introducer is dropped — never re-read as keys — because its parameter bytes
    /// (digits, `;`, `M`) would otherwise trigger commands.
    public mutating func flush() -> [InputEvent] {
        var events: [InputEvent] = []
        if pasteBuffer == nil, buffer.count > 2, hasIncompleteSequence {
            buffer.removeAll()
            return events
        }
        if let paste = pasteBuffer {
            events.append(.paste(String(decoding: paste + buffer, as: UTF8.self)))
            pasteBuffer = nil
            buffer.removeAll()
            return events
        }
        while !buffer.isEmpty {
            if buffer[0] == Self.escapeByte {
                buffer.removeFirst()
                if buffer.isEmpty {
                    events.append(.key(KeyEvent(.escape)))
                } else if case .event(let event?, let length) = parseNext(), case .key(var key) = event {
                    buffer.removeSubrange(..<length)
                    key.modifiers.insert(.alt)
                    events.append(.key(key))
                } else {
                    events.append(.key(KeyEvent(.escape)))
                }
                continue
            }
            switch parseNext() {
            case .event(let event, let length):
                buffer.removeSubrange(..<length)
                if let event { events.append(event) }
            case .incomplete, .pasteBegins:
                // Invalid trailing bytes (e.g. truncated UTF-8): drop them.
                buffer.removeAll()
            }
        }
        return events
    }

    private enum ParseResult {
        case event(InputEvent?, length: Int)
        case incomplete
        case pasteBegins(length: Int)
    }

    private mutating func consumePaste(into events: inout [InputEvent]) -> Bool {
        guard let end = Self.find(Self.pasteEnd, in: buffer) else {
            // Keep a tail that might be the start of the terminator.
            let keep = min(buffer.count, Self.pasteEnd.count - 1)
            pasteBuffer?.append(contentsOf: buffer[..<(buffer.count - keep)])
            buffer.removeSubrange(..<(buffer.count - keep))
            return false
        }
        pasteBuffer?.append(contentsOf: buffer[..<end])
        buffer.removeSubrange(..<(end + Self.pasteEnd.count))
        let text = String(decoding: pasteBuffer ?? [], as: UTF8.self)
        pasteBuffer = nil
        events.append(.paste(text))
        return true
    }

    private func parseNext() -> ParseResult {
        let byte = buffer[0]
        if byte == Self.escapeByte {
            return parseEscape()
        }
        if let control = Self.controlKey(byte) {
            return .event(.key(control), length: 1)
        }
        return parseUTF8(at: 0).map { character, length in
            .event(.key(KeyEvent(.character(character))), length: length)
        } ?? .incomplete
    }

    static func controlKey(_ byte: UInt8) -> KeyEvent? {
        switch byte {
        case 0x0D, 0x0A: return KeyEvent(.enter)
        case 0x09: return KeyEvent(.tab)
        case 0x7F, 0x08: return KeyEvent(.backspace)
        case 0x00: return KeyEvent(.character(" "), modifiers: .ctrl)
        case 0x01...0x1A:
            return KeyEvent(.character(Character(Unicode.Scalar(byte + 0x60))), modifiers: .ctrl)
        case 0x1C: return KeyEvent(.character("\\"), modifiers: .ctrl)
        case 0x1D: return KeyEvent(.character("]"), modifiers: .ctrl)
        case 0x1E: return KeyEvent(.character("^"), modifiers: .ctrl)
        case 0x1F: return KeyEvent(.character("_"), modifiers: .ctrl)
        default: return nil
        }
    }

    /// Returns nil when the sequence is incomplete; invalid bytes decode to U+FFFD.
    private func parseUTF8(at index: Int) -> (Character, Int)? {
        let lead = buffer[index]
        let length: Int
        switch lead {
        case 0x00...0x7F: length = 1
        case 0xC2...0xDF: length = 2
        case 0xE0...0xEF: length = 3
        case 0xF0...0xF4: length = 4
        default: return ("\u{FFFD}", 1)
        }
        guard buffer.count - index >= length else { return nil }
        let bytes = Array(buffer[index..<(index + length)])
        for continuation in bytes.dropFirst() where continuation & 0xC0 != 0x80 {
            return ("\u{FFFD}", 1)
        }
        let text = String(decoding: bytes, as: UTF8.self)
        return (text.first ?? "\u{FFFD}", length)
    }

    private func parseEscape() -> ParseResult {
        guard buffer.count >= 2 else { return .incomplete }
        let next = buffer[1]
        switch next {
        case UInt8(ascii: "["):
            return parseCSI()
        case UInt8(ascii: "O"):
            guard buffer.count >= 3 else { return .incomplete }
            return .event(Self.ss3Key(buffer[2]).map { .key($0) }, length: 3)
        case Self.escapeByte:
            return .event(.key(KeyEvent(.escape)), length: 1)
        default:
            // Alt+key: ESC followed by a normal key.
            if let control = Self.controlKey(next) {
                var key = control
                key.modifiers.insert(.alt)
                return .event(.key(key), length: 2)
            }
            guard let (character, length) = parseUTF8(at: 1) else { return .incomplete }
            return .event(.key(KeyEvent(.character(character), modifiers: .alt)), length: 1 + length)
        }
    }

    static func ss3Key(_ byte: UInt8) -> KeyEvent? {
        switch byte {
        case UInt8(ascii: "A"): return KeyEvent(.up)
        case UInt8(ascii: "B"): return KeyEvent(.down)
        case UInt8(ascii: "C"): return KeyEvent(.right)
        case UInt8(ascii: "D"): return KeyEvent(.left)
        case UInt8(ascii: "H"): return KeyEvent(.home)
        case UInt8(ascii: "F"): return KeyEvent(.end)
        case UInt8(ascii: "M"): return KeyEvent(.enter)
        case UInt8(ascii: "P"): return KeyEvent(.function(1))
        case UInt8(ascii: "Q"): return KeyEvent(.function(2))
        case UInt8(ascii: "R"): return KeyEvent(.function(3))
        case UInt8(ascii: "S"): return KeyEvent(.function(4))
        default: return nil
        }
    }

    private func parseCSI() -> ParseResult {
        // X10 mouse: ESC [ M Cb Cx Cy
        if buffer.count >= 3, buffer[2] == UInt8(ascii: "M") {
            guard buffer.count >= 6 else { return .incomplete }
            let code = Int(buffer[3]) - 32
            let x = Int(buffer[4]) - 33
            let y = Int(buffer[5]) - 33
            return .event(Self.mouseEvent(code: code, x: x, y: y, isRelease: false, legacy: true).map { .mouse($0) }, length: 6)
        }
        var index = 2
        while index < buffer.count, (0x30...0x3F).contains(buffer[index]) { index += 1 }
        while index < buffer.count, (0x20...0x2F).contains(buffer[index]) { index += 1 }
        guard index < buffer.count else {
            return buffer.count > 64 ? .event(nil, length: buffer.count) : .incomplete
        }
        let final = buffer[index]
        let length = index + 1
        guard (0x40...0x7E).contains(final) else {
            // Malformed: drop the introducer so parsing can resynchronize.
            return .event(nil, length: 2)
        }
        let parameterText = String(decoding: buffer[2..<index], as: UTF8.self)

        if parameterText.hasPrefix("<"), final == UInt8(ascii: "M") || final == UInt8(ascii: "m") {
            let numbers = parameterText.dropFirst().split(separator: ";").map { Int($0) }
            guard numbers.count == 3, let code = numbers[0], let x = numbers[1], let y = numbers[2] else {
                return .event(nil, length: length)
            }
            let event = Self.mouseEvent(code: code, x: x - 1, y: y - 1, isRelease: final == UInt8(ascii: "m"), legacy: false)
            return .event(event.map { .mouse($0) }, length: length)
        }

        let parameters = parameterText.split(separator: ";", omittingEmptySubsequences: false).map { Int($0) }
        let first = parameters.first.flatMap { $0 }
        let modifiers = parameters.count > 1 ? KeyModifiers(xtermParameter: parameters[1] ?? 1) : []

        func key(_ key: Key) -> ParseResult {
            .event(.key(KeyEvent(key, modifiers: modifiers)), length: length)
        }

        switch final {
        case UInt8(ascii: "A"): return key(.up)
        case UInt8(ascii: "B"): return key(.down)
        case UInt8(ascii: "C"): return key(.right)
        case UInt8(ascii: "D"): return key(.left)
        case UInt8(ascii: "H"): return key(.home)
        case UInt8(ascii: "F"): return key(.end)
        case UInt8(ascii: "Z"): return .event(.key(KeyEvent(.backTab, modifiers: .shift)), length: length)
        case UInt8(ascii: "P"): return key(.function(1))
        case UInt8(ascii: "Q"): return key(.function(2))
        case UInt8(ascii: "R"): return key(.function(3))
        case UInt8(ascii: "S"): return key(.function(4))
        case UInt8(ascii: "I"): return .event(.focus(true), length: length)
        case UInt8(ascii: "O"): return .event(.focus(false), length: length)
        case UInt8(ascii: "u"):
            // CSI codepoint ; modifiers u (fixterms / kitty keyboard protocol)
            guard let codepoint = first, let scalar = Unicode.Scalar(codepoint) else { return .event(nil, length: length) }
            switch codepoint {
            case 13: return key(.enter)
            case 9: return key(.tab)
            case 27: return key(.escape)
            case 127: return key(.backspace)
            default: return key(.character(Character(scalar)))
            }
        case UInt8(ascii: "~"):
            switch first ?? 0 {
            case 200: return .pasteBegins(length: length)
            case 1, 7: return key(.home)
            case 2: return key(.insert)
            case 3: return key(.delete)
            case 4, 8: return key(.end)
            case 5: return key(.pageUp)
            case 6: return key(.pageDown)
            case 11...15: return key(.function((first ?? 11) - 10))
            case 17...21: return key(.function((first ?? 17) - 11))
            case 23, 24: return key(.function((first ?? 23) - 12))
            default: return .event(nil, length: length)
            }
        default:
            return .event(nil, length: length)
        }
    }

    static func mouseEvent(code: Int, x: Int, y: Int, isRelease: Bool, legacy: Bool) -> MouseEvent? {
        guard code >= 0, x >= 0, y >= 0 else { return nil }
        var modifiers: KeyModifiers = []
        if code & 4 != 0 { modifiers.insert(.shift) }
        if code & 8 != 0 { modifiers.insert(.alt) }
        if code & 16 != 0 { modifiers.insert(.ctrl) }
        let isMotion = code & 32 != 0
        let isWheel = code & 64 != 0
        let low = code & 3

        if isWheel {
            let buttons: [MouseButton] = [.wheelUp, .wheelDown, .wheelLeft, .wheelRight]
            return MouseEvent(button: buttons[low], action: .press, x: x, y: y, modifiers: modifiers)
        }
        let button: MouseButton
        switch low {
        case 0: button = .left
        case 1: button = .middle
        case 2: button = .right
        default: button = .none
        }
        let action: MouseAction
        if isMotion {
            action = button == .none ? .move : .drag
        } else if isRelease || (legacy && low == 3) {
            action = .release
        } else {
            action = .press
        }
        return MouseEvent(button: button, action: action, x: x, y: y, modifiers: modifiers)
    }

    static func find(_ needle: [UInt8], in haystack: [UInt8]) -> Int? {
        guard haystack.count >= needle.count, !needle.isEmpty else { return nil }
        for start in 0...(haystack.count - needle.count) where haystack[start] == needle[0] {
            if Array(haystack[start..<(start + needle.count)]) == needle { return start }
        }
        return nil
    }
}
