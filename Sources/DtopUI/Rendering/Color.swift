/// A terminal color. `ansi` is the 16-color palette (0-7 normal, 8-15 bright), `indexed` the
/// 256-color palette, `rgb` 24-bit truecolor.
public enum Color: Sendable, Hashable, CustomStringConvertible {
    case `default`
    case ansi(UInt8)
    case indexed(UInt8)
    case rgb(UInt8, UInt8, UInt8)

    public static let black = Color.ansi(0)
    public static let red = Color.ansi(1)
    public static let green = Color.ansi(2)
    public static let yellow = Color.ansi(3)
    public static let blue = Color.ansi(4)
    public static let magenta = Color.ansi(5)
    public static let cyan = Color.ansi(6)
    public static let white = Color.ansi(7)
    public static let brightBlack = Color.ansi(8)
    public static let brightRed = Color.ansi(9)
    public static let brightGreen = Color.ansi(10)
    public static let brightYellow = Color.ansi(11)
    public static let brightBlue = Color.ansi(12)
    public static let brightMagenta = Color.ansi(13)
    public static let brightCyan = Color.ansi(14)
    public static let brightWhite = Color.ansi(15)

    static let ansiNames = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        "bright-black", "bright-red", "bright-green", "bright-yellow",
        "bright-blue", "bright-magenta", "bright-cyan", "bright-white"
    ]

    /// xterm's default RGB values for the 16 base colors, used for nearest-color matching.
    static let ansiRGB: [(UInt8, UInt8, UInt8)] = [
        (0, 0, 0), (205, 0, 0), (0, 205, 0), (205, 205, 0), (0, 0, 238), (205, 0, 205), (0, 205, 205), (229, 229, 229),
        (127, 127, 127), (255, 0, 0), (0, 255, 0), (255, 255, 0), (92, 92, 255), (255, 0, 255), (0, 255, 255),
        (255, 255, 255)
    ]

    static let cubeLevels: [Int] = [0, 95, 135, 175, 215, 255]

    public var description: String {
        switch self {
        case .default:
            return "default"
        case .ansi(let index):
            return Int(index) < Self.ansiNames.count ? Self.ansiNames[Int(index)] : "ansi:\(index)"
        case .indexed(let index):
            return "color(\(index))"
        case .rgb(let r, let g, let b):
            return "#" + [r, g, b].map { byte in
                let hex = String(byte, radix: 16)
                return hex.count == 1 ? "0" + hex : hex
            }.joined()
        }
    }

    /// Parses `#rgb`, `#rrggbb`, `rgb(r,g,b)`, `color(n)` / `256:n`, `ansi:n`, named colors and `default`.
    public static func parse(_ raw: String) -> Color? {
        let text = raw.trimmed().lowercased()
        if text.isEmpty { return nil }
        if text == "default" || text == "none" { return .default }
        if text.hasPrefix("#") {
            let hex = Array(text.dropFirst())
            func value(_ chars: ArraySlice<Character>) -> UInt8? { UInt8(String(chars), radix: 16) }
            if hex.count == 6, let r = value(hex[0..<2]), let g = value(hex[2..<4]), let b = value(hex[4..<6]) {
                return .rgb(r, g, b)
            }
            if hex.count == 3, let r = value(hex[0..<1]), let g = value(hex[1..<2]), let b = value(hex[2..<3]) {
                return .rgb(r * 17, g * 17, b * 17)
            }
            return nil
        }
        if text.hasPrefix("rgb(") && text.hasSuffix(")") {
            let inner = text.dropFirst(4).dropLast()
            let parts = inner.split(separator: ",").map { UInt8(String($0).trimmed()) }
            guard parts.count == 3, let r = parts[0], let g = parts[1], let b = parts[2] else { return nil }
            return .rgb(r, g, b)
        }
        if text.hasPrefix("color(") && text.hasSuffix(")") {
            return UInt8(String(text.dropFirst(6).dropLast()).trimmed()).map(Color.indexed)
        }
        if text.hasPrefix("256:") {
            return UInt8(String(text.dropFirst(4))).map(Color.indexed)
        }
        if text.hasPrefix("ansi:") {
            guard let index = UInt8(String(text.dropFirst(5))), index < 16 else { return nil }
            return .ansi(index)
        }
        let normalized = text.replacingCharacter("_", with: "-").replacingCharacter(" ", with: "-")
        let aliases = ["gray": "bright-black", "grey": "bright-black", "bright-gray": "white", "bright-grey": "white"]
        let name = aliases[normalized] ?? normalized
        if let index = ansiNames.firstIndex(of: name) {
            return .ansi(UInt8(index))
        }
        return nil
    }

    /// Approximate RGB for any color (nil for `.default`).
    public var rgbComponents: (UInt8, UInt8, UInt8)? {
        switch self {
        case .default:
            return nil
        case .ansi(let index):
            return Self.ansiRGB[Int(index % 16)]
        case .indexed(let index):
            if index < 16 { return Self.ansiRGB[Int(index)] }
            if index >= 232 {
                let level = UInt8(8 + 10 * (Int(index) - 232))
                return (level, level, level)
            }
            let cube = Int(index) - 16
            return (UInt8(Self.cubeLevels[cube / 36]), UInt8(Self.cubeLevels[(cube / 6) % 6]), UInt8(Self.cubeLevels[cube % 6]))
        case .rgb(let r, let g, let b):
            return (r, g, b)
        }
    }

    /// Converts the color to something the terminal's color mode can display.
    public func downsampled(to mode: ColorMode) -> Color {
        switch (self, mode) {
        case (.default, _), (_, .none):
            return .default
        case (_, .trueColor):
            return self
        case (.rgb(let r, let g, let b), .palette256):
            return .indexed(Self.nearest256(r, g, b))
        case (.indexed, .palette256), (.ansi, .palette256), (.ansi, .basic16):
            return self
        case (.indexed(let index), .basic16):
            if index < 16 { return .ansi(index) }
            guard let rgb = rgbComponents else { return .default }
            return .ansi(Self.nearest16(rgb.0, rgb.1, rgb.2))
        case (.rgb(let r, let g, let b), .basic16):
            return .ansi(Self.nearest16(r, g, b))
        }
    }

    static func distance(_ a: (Int, Int, Int), _ b: (Int, Int, Int)) -> Int {
        // Weighted Euclidean distance; green matters most to perceived brightness.
        let dr = a.0 - b.0
        let dg = a.1 - b.1
        let db = a.2 - b.2
        return 2 * dr * dr + 4 * dg * dg + 3 * db * db
    }

    static func nearest256(_ r: UInt8, _ g: UInt8, _ b: UInt8) -> UInt8 {
        let target = (Int(r), Int(g), Int(b))
        func cubeIndex(_ value: Int) -> Int {
            var best = 0
            for (index, level) in cubeLevels.enumerated() where abs(level - value) < abs(cubeLevels[best] - value) {
                best = index
            }
            return best
        }
        let ri = cubeIndex(target.0), gi = cubeIndex(target.1), bi = cubeIndex(target.2)
        let cubeColor = (cubeLevels[ri], cubeLevels[gi], cubeLevels[bi])
        let cube = 16 + 36 * ri + 6 * gi + bi

        let average = (target.0 + target.1 + target.2) / 3
        let grayStep = max(0, min(23, (average - 8 + 5) / 10))
        let grayLevel = 8 + 10 * grayStep
        let gray = 232 + grayStep

        let cubeDistance = distance(target, cubeColor)
        let grayDistance = distance(target, (grayLevel, grayLevel, grayLevel))
        return UInt8(grayDistance < cubeDistance ? gray : cube)
    }

    static func nearest16(_ r: UInt8, _ g: UInt8, _ b: UInt8) -> UInt8 {
        let target = (Int(r), Int(g), Int(b))
        var best = 0
        var bestDistance = Int.max
        for (index, rgb) in ansiRGB.enumerated() {
            let candidate = distance(target, (Int(rgb.0), Int(rgb.1), Int(rgb.2)))
            if candidate < bestDistance {
                best = index
                bestDistance = candidate
            }
        }
        return UInt8(best)
    }

    /// SGR parameters for this color, already downsampled by the caller.
    public func sgrParameters(foreground: Bool) -> String {
        switch self {
        case .default:
            return foreground ? "39" : "49"
        case .ansi(let index):
            let base = foreground ? 30 : 40
            return index < 8 ? String(base + Int(index)) : String(base + 60 + Int(index) - 8)
        case .indexed(let index):
            return (foreground ? "38;5;" : "48;5;") + String(index)
        case .rgb(let r, let g, let b):
            return (foreground ? "38;2;" : "48;2;") + "\(r);\(g);\(b)"
        }
    }

    /// Linear blend between two colors (falls back to the nearer endpoint for non-RGB colors).
    public static func blend(_ from: Color, _ to: Color, fraction: Double) -> Color {
        let t = max(0, min(1, fraction.isFinite ? fraction : 0))
        guard case .rgb(let r1, let g1, let b1) = from, case .rgb(let r2, let g2, let b2) = to else {
            return t < 0.5 ? from : to
        }
        func mix(_ a: UInt8, _ b: UInt8) -> UInt8 {
            UInt8(max(0, min(255, (Double(a) + (Double(b) - Double(a)) * t).rounded())))
        }
        return .rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
    }
}

/// How many colors the terminal supports.
public enum ColorMode: String, Sendable, Equatable, CaseIterable {
    case trueColor = "truecolor"
    case palette256 = "256"
    case basic16 = "16"
    case none

    /// Heuristic detection from the environment, following the `NO_COLOR` convention.
    public static func detect(environment: [String: String]) -> ColorMode {
        if let noColor = environment["NO_COLOR"], !noColor.isEmpty { return .none }
        let term = (environment["TERM"] ?? "").lowercased()
        if term == "dumb" { return .none }
        let colorTerm = (environment["COLORTERM"] ?? "").lowercased()
        if colorTerm == "truecolor" || colorTerm == "24bit" { return .trueColor }
        if environment["WT_SESSION"] != nil { return .trueColor } // Windows Terminal
        if let program = environment["TERM_PROGRAM"], ["iTerm.app", "WezTerm", "vscode", "ghostty"].contains(program) {
            return .trueColor
        }
        if term.contains("truecolor") || term.contains("24bit") || term.contains("direct") { return .trueColor }
        if term.contains("256") { return .palette256 }
        if term.isEmpty, environment["ConEmuANSI"] == "ON" { return .palette256 }
        return .basic16
    }

    public static func parse(_ raw: String) -> ColorMode? {
        switch raw.trimmed().lowercased() {
        case "truecolor", "24bit", "true", "rgb": return .trueColor
        case "256", "palette", "palette256": return .palette256
        case "16", "basic", "ansi": return .basic16
        case "none", "off", "mono", "no": return ColorMode.none
        default: return nil
        }
    }
}

extension String {
    func trimmed() -> String {
        var view = Substring(self)
        while let first = view.first, first.isWhitespace { view = view.dropFirst() }
        while let last = view.last, last.isWhitespace { view = view.dropLast() }
        return String(view)
    }

    func replacingCharacter(_ target: Character, with replacement: Character) -> String {
        String(map { $0 == target ? replacement : $0 })
    }
}
