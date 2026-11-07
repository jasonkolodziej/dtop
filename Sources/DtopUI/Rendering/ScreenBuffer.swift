public struct Cell: Sendable, Hashable {
    public var character: Character
    public var style: Style
    /// The right half of a double-width character; renderers skip these.
    public var isContinuation: Bool

    public init(_ character: Character = " ", style: Style = .plain, isContinuation: Bool = false) {
        self.character = character
        self.style = style
        self.isContinuation = isContinuation
    }

    public static let blank = Cell()
}

/// An in-memory grid of styled cells. Views draw into it; the renderer diffs consecutive
/// buffers to produce minimal terminal output. Out-of-bounds writes are clipped silently.
public struct ScreenBuffer: Sendable, Equatable {
    public let width: Int
    public let height: Int
    public private(set) var cells: [Cell]

    public init(width: Int, height: Int, fill: Cell = .blank) {
        self.width = max(0, width)
        self.height = max(0, height)
        cells = Array(repeating: fill, count: self.width * self.height)
    }

    public var bounds: Rect { Rect(x: 0, y: 0, width: width, height: height) }

    public func contains(x: Int, y: Int) -> Bool {
        x >= 0 && y >= 0 && x < width && y < height
    }

    public subscript(x: Int, y: Int) -> Cell {
        get {
            guard contains(x: x, y: y) else { return .blank }
            return cells[y * width + x]
        }
        set {
            guard contains(x: x, y: y) else { return }
            cells[y * width + x] = newValue
        }
    }

    /// Places one character, keeping wide characters and their continuation cells consistent.
    public mutating func put(_ character: Character, x: Int, y: Int, style: Style, clip: Rect? = nil) {
        let area = clip.map { $0.intersection(bounds) } ?? bounds
        guard area.contains(x: x, y: y) else { return }
        let characterWidth = TextWidth.of(character)
        guard characterWidth > 0 else { return }
        if characterWidth == 2 && !area.contains(x: x + 1, y: y) {
            // Not enough room for the wide glyph; draw a placeholder instead of half a character.
            put(" ", x: x, y: y, style: style, clip: clip)
            return
        }
        breakWideCharacter(at: x, y: y)
        if characterWidth == 2 { breakWideCharacter(at: x + 1, y: y) }
        self[x, y] = Cell(character, style: style)
        if characterWidth == 2 {
            self[x + 1, y] = Cell(" ", style: style, isContinuation: true)
        }
    }

    /// If (x, y) is part of a wide character, blank the other half so no orphan remains.
    private mutating func breakWideCharacter(at x: Int, y: Int) {
        guard contains(x: x, y: y) else { return }
        let cell = self[x, y]
        if cell.isContinuation, contains(x: x - 1, y: y) {
            self[x - 1, y] = Cell(" ", style: self[x - 1, y].style)
        } else if TextWidth.of(cell.character) == 2, contains(x: x + 1, y: y), self[x + 1, y].isContinuation {
            self[x + 1, y] = Cell(" ", style: self[x + 1, y].style)
        }
    }

    /// Writes text starting at (x, y), returning how many columns were used.
    @discardableResult
    public mutating func write(_ text: String, x: Int, y: Int, style: Style = .plain, maxWidth: Int? = nil, clip: Rect? = nil) -> Int {
        var column = x
        // Stop at the requested width or the visible edge, whichever comes first, so the
        // returned width is what actually landed on screen.
        let area = clip.map { $0.intersection(bounds) } ?? bounds
        let limit = min(maxWidth.map { x + max(0, $0) } ?? Int.max, max(x, area.maxX))
        for character in text {
            let characterWidth = TextWidth.of(character)
            if characterWidth == 0 { continue }
            if column + characterWidth > limit { break }
            put(character, x: column, y: y, style: style, clip: clip)
            column += characterWidth
        }
        return column - x
    }

    public mutating func fill(_ rect: Rect, character: Character = " ", style: Style = .plain) {
        let area = rect.intersection(bounds)
        guard !area.isEmpty else { return }
        for y in area.y..<area.maxY {
            for x in area.x..<area.maxX {
                self[x, y] = Cell(character, style: style)
            }
        }
    }

    /// Re-styles cells in a rect (e.g. to paint a selection highlight over already-drawn text).
    public mutating func restyle(_ rect: Rect, _ transform: (Style) -> Style) {
        let area = rect.intersection(bounds)
        guard !area.isEmpty else { return }
        for y in area.y..<area.maxY {
            for x in area.x..<area.maxX {
                var cell = self[x, y]
                cell.style = transform(cell.style)
                self[x, y] = cell
            }
        }
    }

    /// Plain-text content of one row (wide characters appear once). Handy for tests.
    public func text(row y: Int) -> String {
        guard y >= 0 && y < height else { return "" }
        var line = ""
        for x in 0..<width where !self[x, y].isContinuation {
            line.append(self[x, y].character)
        }
        return line
    }

    public var lines: [String] { (0..<height).map { text(row: $0) } }

    /// The whole buffer as text with trailing spaces trimmed per line.
    public var plainText: String {
        lines.map { line in
            var trimmed = Substring(line)
            while trimmed.last == " " { trimmed = trimmed.dropLast() }
            return String(trimmed)
        }.joined(separator: "\n")
    }
}
