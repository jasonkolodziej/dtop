/// Escape sequences dtop uses to manage the terminal.
public enum TerminalSequence {
    public static let escape = "\u{1B}["
    public static let enterAlternateScreen = "\u{1B}[?1049h"
    public static let exitAlternateScreen = "\u{1B}[?1049l"
    public static let hideCursor = "\u{1B}[?25l"
    public static let showCursor = "\u{1B}[?25h"
    public static let clearScreen = "\u{1B}[2J"
    public static let home = "\u{1B}[H"
    public static let reset = "\u{1B}[0m"
    /// Button events, drag tracking and SGR extended coordinates (no 223-column limit).
    public static let enableMouse = "\u{1B}[?1000h\u{1B}[?1002h\u{1B}[?1006h"
    public static let disableMouse = "\u{1B}[?1006l\u{1B}[?1002l\u{1B}[?1000l"
    public static let enableBracketedPaste = "\u{1B}[?2004h"
    public static let disableBracketedPaste = "\u{1B}[?2004l"
    public static let disableLineWrap = "\u{1B}[?7l"
    public static let enableLineWrap = "\u{1B}[?7h"

    public static func moveTo(x: Int, y: Int) -> String {
        "\u{1B}[\(y + 1);\(x + 1)H"
    }

    public static func setTitle(_ title: String) -> String {
        let safe = String(title.unicodeScalars.filter { $0.value >= 0x20 && $0.value != 0x7F }.map(Character.init))
        return "\u{1B}]0;\(safe)\u{07}"
    }
}

/// Turns screen buffers into terminal output, emitting only cells that changed since the last
/// frame. Cursor moves and style changes are coalesced, which keeps dtop's own CPU and the
/// terminal's work low at high refresh rates.
public struct FrameRenderer: Sendable {
    public var colorMode: ColorMode
    private var previous: ScreenBuffer?

    public init(colorMode: ColorMode) {
        self.colorMode = colorMode
    }

    /// Forces the next frame to repaint everything (after resize or external output).
    public mutating func invalidate() {
        previous = nil
    }

    public mutating func render(_ next: ScreenBuffer) -> String {
        let output = Self.diff(from: previous, to: next, colorMode: colorMode)
        previous = next
        return output
    }

    public static func diff(from previous: ScreenBuffer?, to next: ScreenBuffer, colorMode: ColorMode) -> String {
        let fullRedraw = previous == nil || previous?.width != next.width || previous?.height != next.height
        var output = ""
        if fullRedraw {
            output += TerminalSequence.reset + TerminalSequence.clearScreen
        }

        var cursor: (x: Int, y: Int)?
        var currentStyle: Style?
        for y in 0..<next.height {
            var x = 0
            while x < next.width {
                let cell = next[x, y]
                let changed = fullRedraw || previous?[x, y] != cell
                guard changed else {
                    x += 1
                    continue
                }
                // A changed continuation cell means its wide character must be redrawn.
                var drawX = x
                var drawCell = cell
                if cell.isContinuation, x > 0 {
                    drawX = x - 1
                    drawCell = next[drawX, y]
                }
                if drawCell.isContinuation {
                    x += 1
                    continue
                }
                if cursor == nil || cursor! != (drawX, y) {
                    output += TerminalSequence.moveTo(x: drawX, y: y)
                }
                if currentStyle != drawCell.style {
                    output += drawCell.style.sgr(mode: colorMode)
                    currentStyle = drawCell.style
                }
                output.append(drawCell.character)
                let advance = max(1, TextWidth.of(drawCell.character))
                cursor = (drawX + advance, y)
                x = drawX + advance
            }
        }
        if currentStyle != nil {
            output += TerminalSequence.reset
        }
        return output
    }
}
