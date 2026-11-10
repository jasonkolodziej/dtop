import DtopUI

/// dtop's logo mark drawn in terminal cells: a prompt chevron beside three rising metric bars
/// (see `docs/brand/`). It takes its colors from the theme's accent, success, memory and gpu
/// roles. In the default "harbor" theme those are close to the brand palette; other themes recolor
/// the mark to fit.
enum BrandMark {
    /// A cell-run of the mark: text and the theme role that colors it.
    typealias Run = (text: String, role: ThemeRole)

    /// The one-row mark used in the header. The gap echoes the space between chevron and bars in the logo.
    static let compact: [Run] = [(">", .accent), (" ", .muted), ("▃", .success), ("▅", .memory), ("▇", .gpu)]

    /// The five-row mark used on the connecting and offline screens. Every row is `largeWidth`
    /// cells wide. The chevron is a two-pixel stroke drawn with half blocks (a cell is two pixels
    /// tall), and the bar heights follow the logo's 30 : 48 : 68 proportions.
    static let large: [[Run]] = [
        [("▀█▄", .accent), ("          ", .muted), ("██", .gpu)],
        [("  ▀█▄", .accent), ("     ", .muted), ("▇▇", .memory), (" ", .muted), ("██", .gpu)],
        [("  ▄█▀", .accent), ("  ", .muted), ("▆▆", .success), (" ", .muted), ("██", .memory), (" ", .muted), ("██", .gpu)],
        [("▄█▀", .accent), ("    ", .muted), ("██", .success), (" ", .muted), ("██", .memory), (" ", .muted), ("██", .gpu)],
        [("▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔", .muted)]
    ]

    static let largeWidth = 15

    /// Draws the compact mark at (x, y) on `base` and returns the number of cells written.
    @discardableResult
    static func drawCompact(into buffer: inout ScreenBuffer, x: Int, y: Int, base: Style, theme: Theme,
                            maxWidth: Int? = nil) -> Int {
        draw(compact, into: &buffer, x: x, y: y, base: base, theme: theme, maxWidth: maxWidth)
    }

    /// Draws the large mark with its top-left corner at (x, y), clipped to `bounds`.
    static func drawLarge(into buffer: inout ScreenBuffer, x: Int, y: Int, bounds: Rect, base: Style, theme: Theme) {
        for (offset, row) in large.enumerated() {
            let rowY = y + offset
            guard rowY >= bounds.y, rowY < bounds.maxY, x < bounds.maxX else { continue }
            draw(row, into: &buffer, x: x, y: rowY, base: base, theme: theme, maxWidth: bounds.maxX - x)
        }
    }

    @discardableResult
    private static func draw(_ runs: [Run], into buffer: inout ScreenBuffer, x: Int, y: Int, base: Style,
                             theme: Theme, maxWidth: Int?) -> Int {
        var cursor = x
        for run in runs {
            let remaining = maxWidth.map { x + $0 - cursor }
            if let remaining, remaining <= 0 { break }
            // The chevron is bold, like the logo's heavy stroke.
            var style = base.foreground(theme[run.role])
            if run.role == .accent { style = style.bold }
            cursor += buffer.write(run.text, x: cursor, y: y, style: style, maxWidth: remaining)
        }
        return cursor - x
    }
}
