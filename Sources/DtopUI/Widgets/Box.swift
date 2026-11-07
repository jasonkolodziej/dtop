public struct BorderCharacters: Sendable, Equatable {
    public var topLeft: Character
    public var topRight: Character
    public var bottomLeft: Character
    public var bottomRight: Character
    public var horizontal: Character
    public var vertical: Character

    public init(topLeft: Character, topRight: Character, bottomLeft: Character, bottomRight: Character,
                horizontal: Character, vertical: Character) {
        self.topLeft = topLeft
        self.topRight = topRight
        self.bottomLeft = bottomLeft
        self.bottomRight = bottomRight
        self.horizontal = horizontal
        self.vertical = vertical
    }

    public static let rounded = BorderCharacters(topLeft: "╭", topRight: "╮", bottomLeft: "╰", bottomRight: "╯",
                                                 horizontal: "─", vertical: "│")
    public static let single = BorderCharacters(topLeft: "┌", topRight: "┐", bottomLeft: "└", bottomRight: "┘",
                                                horizontal: "─", vertical: "│")
    public static let double = BorderCharacters(topLeft: "╔", topRight: "╗", bottomLeft: "╚", bottomRight: "╝",
                                                horizontal: "═", vertical: "║")
    public static let heavy = BorderCharacters(topLeft: "┏", topRight: "┓", bottomLeft: "┗", bottomRight: "┛",
                                               horizontal: "━", vertical: "┃")
    /// For terminals/fonts without box-drawing glyphs.
    public static let ascii = BorderCharacters(topLeft: "+", topRight: "+", bottomLeft: "+", bottomRight: "+",
                                               horizontal: "-", vertical: "|")

    public static func named(_ name: String) -> BorderCharacters? {
        switch name.trimmed().lowercased() {
        case "rounded": return .rounded
        case "single", "square": return .single
        case "double": return .double
        case "heavy", "thick": return .heavy
        case "ascii": return .ascii
        default: return nil
        }
    }
}

public enum Box {
    /// Draws a border with an optional title (left) and label (right) embedded in the top edge,
    /// returning the inner content rect.
    @discardableResult
    public static func draw(
        into buffer: inout ScreenBuffer,
        rect: Rect,
        title: String? = nil,
        label: String? = nil,
        borderStyle: Style,
        titleStyle: Style? = nil,
        characters: BorderCharacters = .rounded
    ) -> Rect {
        guard rect.width >= 2, rect.height >= 2 else { return .zero }
        let right = rect.maxX - 1
        let bottom = rect.maxY - 1
        buffer.put(characters.topLeft, x: rect.x, y: rect.y, style: borderStyle)
        buffer.put(characters.topRight, x: right, y: rect.y, style: borderStyle)
        buffer.put(characters.bottomLeft, x: rect.x, y: bottom, style: borderStyle)
        buffer.put(characters.bottomRight, x: right, y: bottom, style: borderStyle)
        for x in (rect.x + 1)..<right {
            buffer.put(characters.horizontal, x: x, y: rect.y, style: borderStyle)
            buffer.put(characters.horizontal, x: x, y: bottom, style: borderStyle)
        }
        for y in (rect.y + 1)..<bottom {
            buffer.put(characters.vertical, x: rect.x, y: y, style: borderStyle)
            buffer.put(characters.vertical, x: right, y: y, style: borderStyle)
        }

        let innerWidth = rect.width - 4
        var titleEnd = rect.x + 1
        if let title, !title.isEmpty, innerWidth > 0 {
            let text = " " + TextWidth.truncate(title, to: max(1, innerWidth - 2)) + " "
            titleEnd = rect.x + 2 + buffer.write(text, x: rect.x + 2, y: rect.y, style: titleStyle ?? borderStyle,
                                                 maxWidth: innerWidth)
        }
        if let label, !label.isEmpty {
            let available = right - 1 - titleEnd - 1
            if available > 2 {
                let text = " " + TextWidth.truncate(label, to: available - 2) + " "
                let start = right - 1 - TextWidth.of(text)
                buffer.write(text, x: start, y: rect.y, style: borderStyle)
            }
        }
        return rect.inset(by: 1)
    }
}
