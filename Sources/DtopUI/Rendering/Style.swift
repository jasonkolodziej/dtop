public struct TextAttributes: OptionSet, Sendable, Hashable {
    public let rawValue: UInt8

    public init(rawValue: UInt8) {
        self.rawValue = rawValue
    }

    public static let bold = TextAttributes(rawValue: 1 << 0)
    public static let dim = TextAttributes(rawValue: 1 << 1)
    public static let italic = TextAttributes(rawValue: 1 << 2)
    public static let underline = TextAttributes(rawValue: 1 << 3)
    public static let reverse = TextAttributes(rawValue: 1 << 4)
    public static let strikethrough = TextAttributes(rawValue: 1 << 5)

    /// SGR codes in a fixed order so output is deterministic.
    var sgrCodes: [String] {
        var codes: [String] = []
        if contains(.bold) { codes.append("1") }
        if contains(.dim) { codes.append("2") }
        if contains(.italic) { codes.append("3") }
        if contains(.underline) { codes.append("4") }
        if contains(.reverse) { codes.append("7") }
        if contains(.strikethrough) { codes.append("9") }
        return codes
    }
}

public struct Style: Sendable, Hashable {
    public var foreground: Color
    public var background: Color
    public var attributes: TextAttributes

    public init(foreground: Color = .default, background: Color = .default, attributes: TextAttributes = []) {
        self.foreground = foreground
        self.background = background
        self.attributes = attributes
    }

    public static let plain = Style()

    public func foreground(_ color: Color) -> Style {
        var copy = self
        copy.foreground = color
        return copy
    }

    public func background(_ color: Color) -> Style {
        var copy = self
        copy.background = color
        return copy
    }

    public func adding(_ extra: TextAttributes) -> Style {
        var copy = self
        copy.attributes.formUnion(extra)
        return copy
    }

    public var bold: Style { adding(.bold) }

    /// Full SGR sequence (starting from a reset) for this style in the given color mode.
    public func sgr(mode: ColorMode) -> String {
        var codes = ["0"] + attributes.sgrCodes
        if mode != .none {
            let fg = foreground.downsampled(to: mode)
            let bg = background.downsampled(to: mode)
            if fg != .default { codes.append(fg.sgrParameters(foreground: true)) }
            if bg != .default { codes.append(bg.sgrParameters(foreground: false)) }
        }
        return "\u{1B}[" + codes.joined(separator: ";") + "m"
    }
}
