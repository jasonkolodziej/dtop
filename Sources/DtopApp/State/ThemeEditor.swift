import DtopUI

/// The interactive theme menu. Every change is applied to `draft`, which the whole UI renders
/// with while the editor is open, so edits are visible immediately. Nothing touches disk until
/// the user saves; closing without saving leaves the original theme in place.
public struct ThemeEditor: Sendable, Equatable {
    public enum Row: Sendable, Equatable {
        case base
        case border
        case emphasis
        case role(ThemeRole)

        public var label: String {
            switch self {
            case .base: return "Theme"
            case .border: return "Borders"
            case .emphasis: return "Emphasis"
            case .role(let role): return role.rawValue.replacingCharacter("_", with: " ")
            }
        }
    }

    public enum Channel: Int, Sendable, CaseIterable {
        case red
        case green
        case blue

        public var label: String { ["R", "G", "B"][rawValue] }
    }

    /// What the bottom input line is doing.
    public enum Field: Sendable, Equatable {
        case none
        case colorInput(String)
        case saveName(String)
        case confirmDiscard
    }

    public enum Outcome: Sendable, Equatable {
        case editing
        case cancelled
        case save(name: String, theme: Theme)
    }

    public static let rows: [Row] = [.base, .border, .emphasis] + ThemeRole.allCases.map(Row.role)
    public static let borderStyles: [(String, BorderCharacters)] = [
        ("rounded", .rounded), ("single", .single), ("double", .double), ("heavy", .heavy), ("ascii", .ascii)
    ]
    public static let smallStep = 8
    public static let largeStep = 32
    public static let builtInNames = Set(Theme.builtIn.map(\.name))

    /// The theme in effect when the editor opened; restored on cancel.
    public let original: Theme
    /// Themes the "Theme" row can switch between.
    public let available: [Theme]
    /// The theme the current draft started from; `d` resets a row to its value.
    public private(set) var base: Theme
    public private(set) var draft: Theme
    public var selected = 0
    public var channel: Channel = .red
    public var field: Field = .none
    public var message: String?

    public init(current: Theme, available: [Theme]) {
        original = current
        var themes = available
        if !themes.contains(where: { $0.name == current.name }) { themes.insert(current, at: 0) }
        self.available = themes
        base = themes.first { $0.name == current.name } ?? current
        draft = current
    }

    public var isDirty: Bool { draft != original }
    public var selectedRow: Row { Self.rows[selected] }

    public var defaultSaveName: String {
        if !Self.builtInNames.contains(draft.name) { return draft.name }
        return draft == base ? base.name : base.name + "-custom"
    }

    // MARK: - Input

    public mutating func handle(_ event: InputEvent, rowHits: [RowHit] = []) -> Outcome {
        switch field {
        case .none: return handleNavigation(event, rowHits: rowHits)
        case .colorInput(let text): return handleColorInput(event, text: text)
        case .saveName(let text): return handleSaveName(event, text: text)
        case .confirmDiscard: return handleConfirmDiscard(event)
        }
    }

    private mutating func handleNavigation(_ event: InputEvent, rowHits: [RowHit]) -> Outcome {
        switch event {
        case .key(let key):
            let big = key.modifiers.contains(.shift)
            switch key.key {
            case .up, .character("k"): select(selected - 1)
            case .down, .character("j"): select(selected + 1)
            case .pageUp: select(selected - 10)
            case .pageDown: select(selected + 10)
            case .home, .character("g"): select(0)
            case .end, .character("G"): select(Self.rows.count - 1)
            case .left, .character("h"): adjust(by: -1, big: big)
            case .right, .character("l"): adjust(by: 1, big: big)
            case .character("H"): adjust(by: -1, big: true)
            case .character("L"): adjust(by: 1, big: true)
            case .tab: channel = Channel(rawValue: (channel.rawValue + 1) % 3)!
            case .backTab: channel = Channel(rawValue: (channel.rawValue + 2) % 3)!
            case .enter:
                if case .role(let role) = selectedRow {
                    field = .colorInput(draft[role].description)
                    message = nil
                } else {
                    adjust(by: 1, big: false)
                }
            case .character("d"), .delete, .backspace: resetSelectedRow()
            case .character("r"):
                draft = original
                base = available.first { $0.name == original.name } ?? original
                message = "All changes reverted"
            case .character("s"):
                field = .saveName(defaultSaveName)
                message = nil
            case .escape, .character("q"):
                if isDirty {
                    field = .confirmDiscard
                } else {
                    return .cancelled
                }
            default:
                break
            }
        case .mouse(let mouse):
            switch mouse.button {
            case .wheelUp: select(selected - 1)
            case .wheelDown: select(selected + 1)
            case .left where mouse.action == .press:
                if let hit = rowHits.first(where: { $0.rect.contains(x: mouse.x, y: mouse.y) }) {
                    if hit.index == selected { adjust(by: 1, big: false) } else { select(hit.index) }
                }
            default:
                break
            }
        case .paste(let text):
            if case .role = selectedRow { field = .colorInput(text.trimmed()) }
        case .focus:
            break
        }
        return .editing
    }

    private mutating func handleColorInput(_ event: InputEvent, text: String) -> Outcome {
        switch event {
        case .key(let key):
            switch key.key {
            case .escape:
                field = .none
            case .enter:
                guard case .role(let role) = selectedRow else {
                    field = .none
                    break
                }
                if let color = Color.parse(text) {
                    draft.colors[role] = color
                    field = .none
                    message = nil
                } else {
                    message = "'\(text)' is not a color — try #4fb0f5, rgb(1,2,3), color(42), red or default"
                }
            case .backspace:
                field = .colorInput(String(text.dropLast()))
            case .character(let character) where key.modifiers.contains(.ctrl):
                if character == "u" { field = .colorInput("") }
            case .character(let character) where !key.modifiers.contains(.alt):
                if text.count < 32 { field = .colorInput(text + String(character)) }
            default:
                break
            }
        case .paste(let pasted):
            field = .colorInput(text + pasted.filter { !$0.isNewline })
        default:
            break
        }
        return .editing
    }

    private mutating func handleSaveName(_ event: InputEvent, text: String) -> Outcome {
        switch event {
        case .key(let key):
            switch key.key {
            case .escape:
                field = .none
            case .enter:
                if let problem = Self.validate(name: text, draft: draft) {
                    message = problem
                    return .editing
                }
                var theme = draft
                theme.name = text
                message = nil
                return .save(name: text, theme: theme)
            case .backspace:
                field = .saveName(String(text.dropLast()))
            case .character(let character) where key.modifiers.isEmpty || key.modifiers == .shift:
                let lowered = Character(character.lowercased())
                if Self.isNameCharacter(lowered), text.count < 40 { field = .saveName(text + String(lowered)) }
            default:
                break
            }
        case .paste(let pasted):
            let cleaned = pasted.lowercased().filter(Self.isNameCharacter)
            field = .saveName(String((text + cleaned).prefix(40)))
        default:
            break
        }
        return .editing
    }

    private mutating func handleConfirmDiscard(_ event: InputEvent) -> Outcome {
        guard case .key(let key) = event else { return .editing }
        switch key.key {
        case .character("y"), .character("Y"):
            return .cancelled
        case .character("n"), .character("N"), .escape:
            field = .none
        case .character("s"):
            field = .saveName(defaultSaveName)
        default:
            break
        }
        return .editing
    }

    static func isNameCharacter(_ character: Character) -> Bool {
        character.isASCII && (character.isLetter || character.isNumber || character == "-" || character == "_")
    }

    /// Theme names become file names, so keep them simple and never shadow a built-in theme.
    static func validate(name: String, draft: Theme) -> String? {
        if name.isEmpty { return "Enter a name for the theme" }
        if !name.allSatisfy(isNameCharacter) { return "Use only letters, digits, - and _" }
        if name == "default" { return "'default' is reserved — pick another name" }
        if let builtIn = Theme.named(name), builtIn.name == name {
            var unchanged = draft
            unchanged.name = builtIn.name
            if unchanged != builtIn {
                return "'\(name)' is a built-in theme — save your changes under a new name"
            }
        }
        return nil
    }

    // MARK: - Editing

    mutating func select(_ index: Int) {
        selected = min(max(0, index), Self.rows.count - 1)
        message = nil
    }

    mutating func adjust(by direction: Int, big: Bool) {
        message = nil
        switch selectedRow {
        case .base:
            guard !available.isEmpty else { return }
            let current = available.firstIndex { $0.name == base.name } ?? 0
            let next = (current + direction + available.count) % available.count
            base = available[next]
            draft = base
        case .border:
            let current = Self.borderStyles.firstIndex { $0.1 == draft.border } ?? 0
            draft.border = Self.borderStyles[(current + direction + Self.borderStyles.count) % Self.borderStyles.count].1
        case .emphasis:
            draft.usesAttributesForEmphasis.toggle()
        case .role(let role):
            let fallback = draft[.foreground].rgbComponents ?? (128, 128, 128)
            var components = draft[role].rgbComponents ?? fallback
            let step = (big ? Self.largeStep : Self.smallStep) * direction
            func moved(_ value: UInt8) -> UInt8 { UInt8(max(0, min(255, Int(value) + step))) }
            switch channel {
            case .red: components.0 = moved(components.0)
            case .green: components.1 = moved(components.1)
            case .blue: components.2 = moved(components.2)
            }
            draft.colors[role] = .rgb(components.0, components.1, components.2)
        }
    }

    mutating func resetSelectedRow() {
        switch selectedRow {
        case .base:
            draft = base
        case .border:
            draft.border = base.border
        case .emphasis:
            draft.usesAttributesForEmphasis = base.usesAttributesForEmphasis
        case .role(let role):
            draft.colors[role] = base[role]
        }
        message = nil
    }

    /// Text shown for a row's current value.
    public func valueText(for row: Row) -> String {
        switch row {
        case .base: return "‹ \(base.name) ›"
        case .border: return "‹ \(Self.borderStyles.first { $0.1 == draft.border }?.0 ?? "custom") ›"
        case .emphasis: return draft.usesAttributesForEmphasis ? "‹ attributes ›" : "‹ color ›"
        case .role(let role): return draft[role].description
        }
    }

    public func isModified(_ row: Row) -> Bool {
        switch row {
        case .base: return base.name != original.name
        case .border: return draft.border != original.border
        case .emphasis: return draft.usesAttributesForEmphasis != original.usesAttributesForEmphasis
        case .role(let role): return draft[role] != original[role]
        }
    }
}

extension String {
    func replacingCharacter(_ target: Character, with replacement: Character) -> String {
        String(map { $0 == target ? replacement : $0 })
    }

    func trimmed() -> String { trimmingSpaces() }
}
