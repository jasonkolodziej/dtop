/// Horizontal meter with eighth-block resolution: `█████▍    `.
public enum Gauge {
    static let partials: [Character] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"]

    /// The bar text for a fraction (clamped to 0...1) over `width` columns.
    public static func bar(fraction: Double, width: Int) -> String {
        guard width > 0 else { return "" }
        let clamped = fraction.isFinite ? max(0, min(1, fraction)) : 0
        let eighths = Int((clamped * Double(width * 8)).rounded())
        let full = eighths / 8
        let remainder = eighths % 8
        var text = String(repeating: "█", count: full)
        if full < width {
            text.append(partials[remainder])
            text += String(repeating: " ", count: width - full - 1)
        }
        return text
    }

    /// Draws a gauge with an optional right-aligned label inside a filled track.
    public static func draw(
        into buffer: inout ScreenBuffer,
        x: Int,
        y: Int,
        width: Int,
        fraction: Double,
        filled: Style,
        track: Style
    ) {
        guard width > 0 else { return }
        let text = bar(fraction: fraction, width: width)
        var column = x
        for character in text {
            buffer.put(character == " " ? "·" : character, x: column, y: y, style: character == " " ? track : filled)
            column += 1
        }
    }
}

/// btop-style graphs using Braille patterns: each cell holds a 2x4 dot grid, giving twice the
/// horizontal and four times the vertical resolution of block characters.
public enum BrailleGraph {
    // Dot bits for the left and right columns, bottom row first.
    static let leftDots: [UInt32] = [0x40, 0x04, 0x02, 0x01]
    static let rightDots: [UInt32] = [0x80, 0x20, 0x10, 0x08]

    /// Renders normalized values (0...1, oldest first) into `height` strings of `width` cells,
    /// top row first. The newest samples are right-aligned; missing history is left blank.
    public static func render(values: [Double], width: Int, height: Int) -> [String] {
        guard width > 0, height > 0 else { return [] }
        let capacity = width * 2
        let recent = Array(values.suffix(capacity))
        let padded = Array(repeating: Double.nan, count: capacity - recent.count) + recent
        let levelsPerCell = 4
        let totalLevels = height * levelsPerCell

        let levels = padded.map { value -> Int in
            guard value.isFinite else { return 0 }
            let clamped = max(0, min(1, value))
            let level = Int((clamped * Double(totalLevels)).rounded())
            // Any non-zero activity shows at least one dot so idle-but-alive is visible.
            return clamped > 0 ? max(1, level) : 0
        }

        var rows: [String] = []
        for row in 0..<height {
            let floor = (height - 1 - row) * levelsPerCell
            var line = ""
            for cell in 0..<width {
                var bits: UInt32 = 0
                let left = levels[cell * 2] - floor
                let right = levels[cell * 2 + 1] - floor
                for dot in 0..<levelsPerCell {
                    if left > dot { bits |= leftDots[dot] }
                    if right > dot { bits |= rightDots[dot] }
                }
                line.append(bits == 0 ? " " : Character(Unicode.Scalar(0x2800 + bits)!))
            }
            rows.append(line)
        }
        return rows
    }

    /// Draws a graph into a rect, coloring each row by its height (0 = bottom, 1 = top).
    public static func draw(
        into buffer: inout ScreenBuffer,
        rect: Rect,
        values: [Double],
        maximum: Double,
        style: (Double) -> Style
    ) {
        guard !rect.isEmpty else { return }
        let scale = maximum > 0 && maximum.isFinite ? maximum : 1
        let normalized = values.map { $0 / scale }
        let rows = render(values: normalized, width: rect.width, height: rect.height)
        for (offset, line) in rows.enumerated() {
            let heightFraction = rect.height <= 1 ? 1 : Double(rect.height - 1 - offset) / Double(rect.height - 1)
            buffer.write(line, x: rect.x, y: rect.y + offset, style: style(heightFraction), maxWidth: rect.width)
        }
    }
}

/// Single-row block sparkline: `▁▂▄▇█`.
public enum Sparkline {
    static let blocks: [Character] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"]

    public static func render(values: [Double], width: Int, maximum: Double? = nil) -> String {
        guard width > 0 else { return "" }
        let recent = Array(values.suffix(width))
        let peak = maximum ?? recent.filter(\.isFinite).max() ?? 0
        var line = String(repeating: " ", count: width - recent.count)
        for value in recent {
            guard value.isFinite, peak > 0, value > 0 else {
                line.append(" ")
                continue
            }
            let fraction = min(1, value / peak)
            let index = min(blocks.count - 1, Int((fraction * Double(blocks.count - 1)).rounded()))
            line.append(blocks[index])
        }
        return line
    }
}
