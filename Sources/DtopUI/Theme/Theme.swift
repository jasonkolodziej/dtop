/// Semantic color slots. Views ask for roles, never raw colors, so themes stay interchangeable.
public enum ThemeRole: String, Sendable, CaseIterable, Hashable {
    case background
    case foreground
    case muted
    case accent
    case title
    case border
    case borderFocused = "border_focused"
    case headerBackground = "header_background"
    case headerForeground = "header_foreground"
    case tabActive = "tab_active"
    case tabInactive = "tab_inactive"
    case tableHeader = "table_header"
    case selectionBackground = "selection_background"
    case selectionForeground = "selection_foreground"
    case statusRunning = "status_running"
    case statusPaused = "status_paused"
    case statusExited = "status_exited"
    case statusRestarting = "status_restarting"
    case statusDead = "status_dead"
    case statusCreated = "status_created"
    case healthy
    case unhealthy
    case graphLow = "graph_low"
    case graphMid = "graph_mid"
    case graphHigh = "graph_high"
    case cpu
    case memory
    case networkRx = "network_rx"
    case networkTx = "network_tx"
    case disk
    case success
    case warning
    case error
    case dialogBackground = "dialog_background"
    case dialogBorder = "dialog_border"
}

public struct Theme: Sendable, Equatable {
    public var name: String
    public var colors: [ThemeRole: Color]
    /// Monochrome themes lean on attributes (bold/reverse) instead of color.
    public var usesAttributesForEmphasis: Bool
    public var border: BorderCharacters

    public init(name: String, colors: [ThemeRole: Color], usesAttributesForEmphasis: Bool = false,
                border: BorderCharacters = .rounded) {
        self.name = name
        self.colors = colors
        self.usesAttributesForEmphasis = usesAttributesForEmphasis
        self.border = border
    }

    public subscript(role: ThemeRole) -> Color {
        colors[role] ?? .default
    }

    /// Foreground style for a role on the theme background.
    public func style(_ role: ThemeRole) -> Style {
        Style(foreground: self[role], background: self[.background])
    }

    public var base: Style { Style(foreground: self[.foreground], background: self[.background]) }

    public var selection: Style {
        let style = Style(foreground: self[.selectionForeground], background: self[.selectionBackground])
        return usesAttributesForEmphasis ? style.adding(.reverse) : style
    }

    /// Low → mid → high color ramp for a 0...1 utilization fraction.
    public func gradient(_ fraction: Double) -> Color {
        let t = fraction.isFinite ? max(0, min(1, fraction)) : 0
        if t <= 0.5 {
            return Color.blend(self[.graphLow], self[.graphMid], fraction: t * 2)
        }
        return Color.blend(self[.graphMid], self[.graphHigh], fraction: (t - 0.5) * 2)
    }

    // MARK: - Built-in themes

    public static let builtIn: [Theme] = [
        .harbor, .daylight, .midnight, .ember, .forest, .glacier, .dusk, .contrast, .ansi, .monochrome
    ]

    public static func named(_ name: String) -> Theme? {
        let lowered = name.trimmed().lowercased()
        if lowered == "default" { return .harbor }
        return builtIn.first { $0.name == lowered }
    }

    /// Default dark theme: calm blues with clear status colors.
    public static let harbor = Theme(name: "harbor", colors: [
        .background: .default,
        .foreground: .rgb(0xD8, 0xDE, 0xE9),
        .muted: .rgb(0x7B, 0x86, 0x99),
        .accent: .rgb(0x4F, 0xB0, 0xF5),
        .title: .rgb(0xE8, 0xEC, 0xF2),
        .border: .rgb(0x3C, 0x48, 0x5C),
        .borderFocused: .rgb(0x4F, 0xB0, 0xF5),
        .headerBackground: .rgb(0x1A, 0x23, 0x33),
        .headerForeground: .rgb(0xD8, 0xDE, 0xE9),
        .tabActive: .rgb(0x4F, 0xB0, 0xF5),
        .tabInactive: .rgb(0x7B, 0x86, 0x99),
        .tableHeader: .rgb(0x9F, 0xB4, 0xD1),
        .selectionBackground: .rgb(0x23, 0x3A, 0x57),
        .selectionForeground: .rgb(0xFF, 0xFF, 0xFF),
        .statusRunning: .rgb(0x5C, 0xCF, 0x8B),
        .statusPaused: .rgb(0xE3, 0xBF, 0x6E),
        .statusExited: .rgb(0x8A, 0x93, 0xA3),
        .statusRestarting: .rgb(0xF0, 0x98, 0x4A),
        .statusDead: .rgb(0xE5, 0x5F, 0x5F),
        .statusCreated: .rgb(0x7C, 0xA3, 0xF5),
        .healthy: .rgb(0x5C, 0xCF, 0x8B),
        .unhealthy: .rgb(0xE5, 0x5F, 0x5F),
        .graphLow: .rgb(0x4F, 0xC0, 0xA0),
        .graphMid: .rgb(0xE3, 0xBF, 0x6E),
        .graphHigh: .rgb(0xE5, 0x5F, 0x5F),
        .cpu: .rgb(0x4F, 0xB0, 0xF5),
        .memory: .rgb(0xB7, 0x8C, 0xF2),
        .networkRx: .rgb(0x4F, 0xC0, 0xC8),
        .networkTx: .rgb(0xE3, 0xBF, 0x6E),
        .disk: .rgb(0xF0, 0x98, 0x4A),
        .success: .rgb(0x5C, 0xCF, 0x8B),
        .warning: .rgb(0xE3, 0xBF, 0x6E),
        .error: .rgb(0xE5, 0x5F, 0x5F),
        .dialogBackground: .rgb(0x16, 0x1D, 0x2A),
        .dialogBorder: .rgb(0x4F, 0xB0, 0xF5)
    ])

    /// For light terminal backgrounds.
    public static let daylight = Theme(name: "daylight", colors: [
        .background: .default,
        .foreground: .rgb(0x24, 0x29, 0x33),
        .muted: .rgb(0x6B, 0x73, 0x80),
        .accent: .rgb(0x1F, 0x6F, 0xC5),
        .title: .rgb(0x10, 0x14, 0x1C),
        .border: .rgb(0xB4, 0xBC, 0xC8),
        .borderFocused: .rgb(0x1F, 0x6F, 0xC5),
        .headerBackground: .rgb(0xE4, 0xE9, 0xF0),
        .headerForeground: .rgb(0x24, 0x29, 0x33),
        .tabActive: .rgb(0x1F, 0x6F, 0xC5),
        .tabInactive: .rgb(0x6B, 0x73, 0x80),
        .tableHeader: .rgb(0x3D, 0x4F, 0x6B),
        .selectionBackground: .rgb(0xCF, 0xE2, 0xF8),
        .selectionForeground: .rgb(0x10, 0x14, 0x1C),
        .statusRunning: .rgb(0x1E, 0x8E, 0x4E),
        .statusPaused: .rgb(0x9A, 0x6B, 0x00),
        .statusExited: .rgb(0x6B, 0x73, 0x80),
        .statusRestarting: .rgb(0xB8, 0x5C, 0x00),
        .statusDead: .rgb(0xC0, 0x30, 0x30),
        .statusCreated: .rgb(0x2D, 0x5B, 0xC7),
        .healthy: .rgb(0x1E, 0x8E, 0x4E),
        .unhealthy: .rgb(0xC0, 0x30, 0x30),
        .graphLow: .rgb(0x1E, 0x8E, 0x6E),
        .graphMid: .rgb(0xB0, 0x80, 0x00),
        .graphHigh: .rgb(0xC0, 0x30, 0x30),
        .cpu: .rgb(0x1F, 0x6F, 0xC5),
        .memory: .rgb(0x7A, 0x3F, 0xC0),
        .networkRx: .rgb(0x0E, 0x7C, 0x86),
        .networkTx: .rgb(0x9A, 0x6B, 0x00),
        .disk: .rgb(0xB8, 0x5C, 0x00),
        .success: .rgb(0x1E, 0x8E, 0x4E),
        .warning: .rgb(0x9A, 0x6B, 0x00),
        .error: .rgb(0xC0, 0x30, 0x30),
        .dialogBackground: .rgb(0xF4, 0xF6, 0xF9),
        .dialogBorder: .rgb(0x1F, 0x6F, 0xC5)
    ])

    /// Deep violet/teal dark theme.
    public static let midnight = Theme(name: "midnight", colors: [
        .background: .rgb(0x12, 0x10, 0x1C),
        .foreground: .rgb(0xDD, 0xD8, 0xF0),
        .muted: .rgb(0x7E, 0x76, 0x9C),
        .accent: .rgb(0x3D, 0xD6, 0xC6),
        .title: .rgb(0xF2, 0xEE, 0xFF),
        .border: .rgb(0x3A, 0x33, 0x55),
        .borderFocused: .rgb(0x3D, 0xD6, 0xC6),
        .headerBackground: .rgb(0x1E, 0x1A, 0x2E),
        .headerForeground: .rgb(0xDD, 0xD8, 0xF0),
        .tabActive: .rgb(0x3D, 0xD6, 0xC6),
        .tabInactive: .rgb(0x7E, 0x76, 0x9C),
        .tableHeader: .rgb(0xB0, 0xA4, 0xDC),
        .selectionBackground: .rgb(0x34, 0x2B, 0x52),
        .selectionForeground: .rgb(0xFF, 0xFF, 0xFF),
        .statusRunning: .rgb(0x3D, 0xD6, 0x8C),
        .statusPaused: .rgb(0xF2, 0xC4, 0x5A),
        .statusExited: .rgb(0x86, 0x7F, 0xA3),
        .statusRestarting: .rgb(0xFF, 0x9F, 0x5A),
        .statusDead: .rgb(0xFF, 0x5F, 0x87),
        .statusCreated: .rgb(0x8F, 0x9C, 0xFF),
        .healthy: .rgb(0x3D, 0xD6, 0x8C),
        .unhealthy: .rgb(0xFF, 0x5F, 0x87),
        .graphLow: .rgb(0x3D, 0xD6, 0xC6),
        .graphMid: .rgb(0xC0, 0x8C, 0xFF),
        .graphHigh: .rgb(0xFF, 0x5F, 0x87),
        .cpu: .rgb(0x3D, 0xD6, 0xC6),
        .memory: .rgb(0xC0, 0x8C, 0xFF),
        .networkRx: .rgb(0x5A, 0xB8, 0xFF),
        .networkTx: .rgb(0xF2, 0xC4, 0x5A),
        .disk: .rgb(0xFF, 0x9F, 0x5A),
        .success: .rgb(0x3D, 0xD6, 0x8C),
        .warning: .rgb(0xF2, 0xC4, 0x5A),
        .error: .rgb(0xFF, 0x5F, 0x87),
        .dialogBackground: .rgb(0x1E, 0x1A, 0x2E),
        .dialogBorder: .rgb(0x3D, 0xD6, 0xC6)
    ])

    /// Uses only the 16 base colors so it follows the terminal's own palette.
    public static let ansi = Theme(name: "ansi", colors: [
        .background: .default,
        .foreground: .default,
        .muted: .brightBlack,
        .accent: .brightBlue,
        .title: .brightWhite,
        .border: .brightBlack,
        .borderFocused: .brightBlue,
        .headerBackground: .default,
        .headerForeground: .brightWhite,
        .tabActive: .brightBlue,
        .tabInactive: .brightBlack,
        .tableHeader: .cyan,
        .selectionBackground: .blue,
        .selectionForeground: .brightWhite,
        .statusRunning: .green,
        .statusPaused: .yellow,
        .statusExited: .brightBlack,
        .statusRestarting: .brightYellow,
        .statusDead: .red,
        .statusCreated: .cyan,
        .healthy: .green,
        .unhealthy: .red,
        .graphLow: .green,
        .graphMid: .yellow,
        .graphHigh: .red,
        .cpu: .brightBlue,
        .memory: .magenta,
        .networkRx: .cyan,
        .networkTx: .yellow,
        .disk: .brightYellow,
        .success: .green,
        .warning: .yellow,
        .error: .red,
        .dialogBackground: .default,
        .dialogBorder: .brightBlue
    ])

    /// No colors at all; emphasis via reverse video.
    public static let monochrome = Theme(
        name: "monochrome",
        colors: Dictionary(uniqueKeysWithValues: ThemeRole.allCases.map { ($0, Color.default) }),
        usesAttributesForEmphasis: true,
        border: .single
    )
}

public struct ThemeParseError: Error, Equatable, CustomStringConvertible {
    public var line: Int
    public var message: String

    public init(line: Int, message: String) {
        self.line = line
        self.message = message
    }

    public var description: String { "theme line \(line): \(message)" }
}

/// Reads and writes the simple `key = value` theme format:
///
///     # comment
///     name = ocean
///     inherits = harbor
///     accent = #4fb0f5
///     border_style = rounded
public enum ThemeFile {
    public static func parse(_ text: String, fallbackName: String, resolveBase: (String) -> Theme? = Theme.named) throws -> Theme {
        var theme = Theme.harbor
        theme.name = fallbackName.lowercased()
        var assigned: [ThemeRole: Color] = [:]
        var explicitName: String?
        var border: BorderCharacters?
        var emphasis: Bool?

        for (offset, rawLine) in text.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
            let lineNumber = offset + 1
            let line = String(rawLine).trimmed()
            if line.isEmpty || line.hasPrefix("#") || line.hasPrefix(";") || line.hasPrefix("//") { continue }
            guard let equals = line.firstIndex(of: "=") else {
                throw ThemeParseError(line: lineNumber, message: "expected 'key = value', found '\(line)'")
            }
            let key = String(line[..<equals]).trimmed().lowercased().replacingCharacter("-", with: "_")
            var value = String(line[line.index(after: equals)...]).trimmed()
            if value.count >= 2, value.hasPrefix("\""), value.hasSuffix("\"") {
                value = String(value.dropFirst().dropLast())
            }
            switch key {
            case "name":
                guard !value.isEmpty else { throw ThemeParseError(line: lineNumber, message: "name must not be empty") }
                explicitName = value.lowercased()
            case "inherits", "base":
                guard let base = resolveBase(value) else {
                    throw ThemeParseError(line: lineNumber, message: "unknown base theme '\(value)'")
                }
                let name = theme.name
                theme = base
                theme.name = name
            case "border_style", "borders":
                guard let characters = BorderCharacters.named(value) else {
                    throw ThemeParseError(line: lineNumber,
                                          message: "unknown border style '\(value)' (rounded, single, double, heavy, ascii)")
                }
                border = characters
            case "emphasis":
                switch value.lowercased() {
                case "attributes", "reverse": emphasis = true
                case "color", "colors": emphasis = false
                default:
                    throw ThemeParseError(line: lineNumber, message: "emphasis must be 'color' or 'attributes'")
                }
            default:
                guard let role = ThemeRole(rawValue: key) else {
                    throw ThemeParseError(line: lineNumber, message: "unknown theme key '\(key)'")
                }
                guard let color = Color.parse(value) else {
                    throw ThemeParseError(line: lineNumber,
                                          message: "invalid color '\(value)' (use #rrggbb, color(0-255), ansi:0-15 or a name)")
                }
                assigned[role] = color
            }
        }
        for (role, color) in assigned { theme.colors[role] = color }
        if let explicitName { theme.name = explicitName }
        if let border { theme.border = border }
        if let emphasis { theme.usesAttributesForEmphasis = emphasis }
        return theme
    }

    public static func serialize(_ theme: Theme) -> String {
        var lines = ["# dtop theme", "name = \(theme.name)"]
        let borderName: String
        switch theme.border {
        case .single: borderName = "single"
        case .double: borderName = "double"
        case .heavy: borderName = "heavy"
        case .ascii: borderName = "ascii"
        default: borderName = "rounded"
        }
        lines.append("border_style = \(borderName)")
        lines.append("emphasis = \(theme.usesAttributesForEmphasis ? "attributes" : "color")")
        for role in ThemeRole.allCases {
            lines.append("\(role.rawValue) = \(theme[role])")
        }
        return lines.joined(separator: "\n") + "\n"
    }
}
