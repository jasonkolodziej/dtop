public struct KeyModifiers: OptionSet, Sendable, Hashable {
    public let rawValue: UInt8

    public init(rawValue: UInt8) {
        self.rawValue = rawValue
    }

    public static let shift = KeyModifiers(rawValue: 1 << 0)
    public static let alt = KeyModifiers(rawValue: 1 << 1)
    public static let ctrl = KeyModifiers(rawValue: 1 << 2)

    /// xterm encodes modifiers in CSI parameters as `1 + bitmask(shift=1, alt=2, ctrl=4)`.
    init(xtermParameter: Int) {
        let mask = max(0, xtermParameter - 1)
        var result: KeyModifiers = []
        if mask & 1 != 0 { result.insert(.shift) }
        if mask & 2 != 0 { result.insert(.alt) }
        if mask & 4 != 0 { result.insert(.ctrl) }
        self = result
    }
}

public enum Key: Sendable, Hashable {
    case character(Character)
    case enter
    case tab
    case backTab
    case backspace
    case escape
    case up
    case down
    case left
    case right
    case home
    case end
    case pageUp
    case pageDown
    case insert
    case delete
    case function(Int)
}

public struct KeyEvent: Sendable, Hashable {
    public var key: Key
    public var modifiers: KeyModifiers

    public init(_ key: Key, modifiers: KeyModifiers = []) {
        self.key = key
        self.modifiers = modifiers
    }

    public static func char(_ character: Character, _ modifiers: KeyModifiers = []) -> KeyEvent {
        KeyEvent(.character(character), modifiers: modifiers)
    }

    public static func ctrl(_ character: Character) -> KeyEvent {
        KeyEvent(.character(character), modifiers: .ctrl)
    }
}

public enum MouseButton: Sendable, Hashable {
    case left
    case middle
    case right
    case none
    case wheelUp
    case wheelDown
    case wheelLeft
    case wheelRight
}

public enum MouseAction: Sendable, Hashable {
    case press
    case release
    case drag
    case move
}

public struct MouseEvent: Sendable, Hashable {
    public var button: MouseButton
    public var action: MouseAction
    /// Zero-based column and row.
    public var x: Int
    public var y: Int
    public var modifiers: KeyModifiers

    public init(button: MouseButton, action: MouseAction, x: Int, y: Int, modifiers: KeyModifiers = []) {
        self.button = button
        self.action = action
        self.x = x
        self.y = y
        self.modifiers = modifiers
    }

    public var isWheel: Bool {
        [.wheelUp, .wheelDown, .wheelLeft, .wheelRight].contains(button)
    }
}

public enum InputEvent: Sendable, Hashable {
    case key(KeyEvent)
    case mouse(MouseEvent)
    case paste(String)
    case focus(Bool)
}
