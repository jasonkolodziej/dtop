import Testing
@testable import DtopUI

@Suite("Colors")
struct ColorTests {
    @Test("parses every supported notation", arguments: [
        ("#ff8000", Color.rgb(255, 128, 0)), ("#F80", .rgb(255, 136, 0)), ("rgb(1, 2, 3)", .rgb(1, 2, 3)),
        ("color(123)", .indexed(123)), ("256:42", .indexed(42)), ("ansi:9", .ansi(9)), ("red", .ansi(1)),
        ("Bright-Blue", .ansi(12)), ("bright_white", .ansi(15)), ("grey", .ansi(8)), ("default", .default),
        ("  none ", .default)
    ])
    func parse(text: String, expected: Color) {
        #expect(Color.parse(text) == expected)
    }

    @Test("rejects invalid notations", arguments: [
        "", "#ggg", "#12345", "rgb(1,2)", "rgb(256,0,0)", "color(256)", "ansi:16", "chartreuse", "256:x"
    ])
    func parseInvalid(text: String) {
        #expect(Color.parse(text) == nil)
    }

    @Test("descriptions parse back to the same color")
    func roundTrip() {
        for color in [Color.default, .ansi(3), .ansi(11), .indexed(200), .rgb(1, 171, 255)] {
            #expect(Color.parse(color.description) == color)
        }
    }

    @Test("SGR parameters for each color kind")
    func sgr() {
        #expect(Color.default.sgrParameters(foreground: true) == "39")
        #expect(Color.default.sgrParameters(foreground: false) == "49")
        #expect(Color.ansi(1).sgrParameters(foreground: true) == "31")
        #expect(Color.ansi(9).sgrParameters(foreground: true) == "91")
        #expect(Color.ansi(4).sgrParameters(foreground: false) == "44")
        #expect(Color.ansi(12).sgrParameters(foreground: false) == "104")
        #expect(Color.indexed(200).sgrParameters(foreground: true) == "38;5;200")
        #expect(Color.rgb(1, 2, 3).sgrParameters(foreground: false) == "48;2;1;2;3")
    }

    @Test("downsampling to 256 colors picks the cube or gray ramp")
    func to256() {
        #expect(Color.rgb(255, 0, 0).downsampled(to: .palette256) == .indexed(196))
        #expect(Color.rgb(0, 0, 0).downsampled(to: .palette256) == .indexed(16))
        #expect(Color.rgb(128, 128, 128).downsampled(to: .palette256) == .indexed(244))
        #expect(Color.rgb(95, 135, 175).downsampled(to: .palette256) == .indexed(67))
        #expect(Color.indexed(42).downsampled(to: .palette256) == .indexed(42))
        #expect(Color.ansi(3).downsampled(to: .palette256) == .ansi(3))
    }

    @Test("downsampling to 16 colors finds the nearest base color")
    func to16() {
        #expect(Color.rgb(250, 10, 10).downsampled(to: .basic16) == .ansi(9))
        #expect(Color.rgb(0, 190, 0).downsampled(to: .basic16) == .ansi(2))
        #expect(Color.rgb(10, 10, 10).downsampled(to: .basic16) == .ansi(0))
        #expect(Color.indexed(9).downsampled(to: .basic16) == .ansi(9))
        #expect(Color.indexed(231).downsampled(to: .basic16) == .ansi(15))
    }

    @Test("no-color mode and truecolor passthrough")
    func noneAndTrue() {
        #expect(Color.rgb(1, 2, 3).downsampled(to: .none) == .default)
        #expect(Color.ansi(1).downsampled(to: .none) == .default)
        #expect(Color.rgb(1, 2, 3).downsampled(to: .trueColor) == .rgb(1, 2, 3))
        #expect(Color.default.downsampled(to: .palette256) == .default)
    }

    @Test("palette entries convert to RGB")
    func rgbComponents() {
        #expect(Color.default.rgbComponents == nil)
        #expect(Color.indexed(16).rgbComponents! == (0, 0, 0))
        #expect(Color.indexed(231).rgbComponents! == (255, 255, 255))
        #expect(Color.indexed(232).rgbComponents! == (8, 8, 8))
        #expect(Color.indexed(255).rgbComponents! == (238, 238, 238))
        #expect(Color.indexed(1).rgbComponents! == (205, 0, 0))
    }

    @Test("blend interpolates RGB and snaps otherwise")
    func blend() {
        #expect(Color.blend(.rgb(0, 0, 0), .rgb(200, 100, 50), fraction: 0.5) == .rgb(100, 50, 25))
        #expect(Color.blend(.rgb(0, 0, 0), .rgb(200, 100, 50), fraction: -1) == .rgb(0, 0, 0))
        #expect(Color.blend(.rgb(0, 0, 0), .rgb(200, 100, 50), fraction: 9) == .rgb(200, 100, 50))
        #expect(Color.blend(.rgb(0, 0, 0), .rgb(200, 100, 50), fraction: .nan) == .rgb(0, 0, 0))
        #expect(Color.blend(.ansi(1), .ansi(2), fraction: 0.4) == .ansi(1))
        #expect(Color.blend(.ansi(1), .ansi(2), fraction: 0.6) == .ansi(2))
    }
}

@Suite("Color mode detection")
struct ColorModeTests {
    @Test("detects capabilities from the environment", arguments: [
        ([:] as [String: String], ColorMode.basic16),
        (["NO_COLOR": "1", "COLORTERM": "truecolor"], .none),
        (["NO_COLOR": ""], .basic16),
        (["TERM": "dumb"], .none),
        (["COLORTERM": "truecolor"], .trueColor),
        (["COLORTERM": "24bit"], .trueColor),
        (["WT_SESSION": "abc"], .trueColor),
        (["TERM_PROGRAM": "iTerm.app"], .trueColor),
        (["TERM": "xterm-direct"], .trueColor),
        (["TERM": "xterm-256color"], .palette256),
        (["TERM": "screen"], .basic16),
        (["ConEmuANSI": "ON"], .palette256)
    ])
    func detect(environment: [String: String], expected: ColorMode) {
        #expect(ColorMode.detect(environment: environment) == expected)
    }

    @Test("parses user-facing names")
    func parse() {
        #expect(ColorMode.parse("TrueColor") == .trueColor)
        #expect(ColorMode.parse("256") == .palette256)
        #expect(ColorMode.parse("16") == .basic16)
        #expect(ColorMode.parse("off") == ColorMode.none)
        #expect(ColorMode.parse("lots") == nil)
    }
}

@Suite("Styles")
struct StyleTests {
    @Test("SGR sequences start from a reset and are deterministic")
    func sgr() {
        let style = Style(foreground: .rgb(1, 2, 3), background: .ansi(4), attributes: [.underline, .bold])
        #expect(style.sgr(mode: .trueColor) == "\u{1B}[0;1;4;38;2;1;2;3;44m")
        #expect(style.sgr(mode: .none) == "\u{1B}[0;1;4m")
        #expect(Style.plain.sgr(mode: .trueColor) == "\u{1B}[0m")
        let all = Style(attributes: [.bold, .dim, .italic, .underline, .reverse, .strikethrough])
        #expect(all.sgr(mode: .basic16) == "\u{1B}[0;1;2;3;4;7;9m")
    }

    @Test("builders return modified copies")
    func builders() {
        let base = Style.plain
        #expect(base.foreground(.red).foreground == .red)
        #expect(base.background(.blue).background == .blue)
        #expect(base.bold.attributes == .bold)
        #expect(base.adding([.dim, .italic]).attributes == [.dim, .italic])
        #expect(base == Style.plain)
    }
}

@Suite("Text width")
struct TextWidthTests {
    @Test("character widths", arguments: [
        ("a", 1), ("é", 1), ("─", 1), ("⣿", 1), ("中", 2), ("日", 2), ("한", 2), ("Ａ", 2), ("😀", 2), ("✅", 2),
        ("\u{0301}", 0), ("\u{200B}", 0), ("\u{0007}", 0), ("👍🏽", 2), ("❤️", 2), ("👨‍👩‍👧", 2)
    ] as [(Character, Int)])
    func widths(character: Character, expected: Int) {
        #expect(TextWidth.of(character) == expected)
    }

    @Test("string widths sum per character")
    func strings() {
        #expect(TextWidth.of("abc") == 3)
        #expect(TextWidth.of("中文ab") == 6)
        #expect(TextWidth.of("") == 0)
        #expect(TextWidth.of("e\u{0301}") == 1)
    }

    @Test("truncate adds an ellipsis and never splits wide characters")
    func truncate() {
        #expect(TextWidth.truncate("hello", to: 10) == "hello")
        #expect(TextWidth.truncate("hello world", to: 8) == "hello w…")
        #expect(TextWidth.truncate("中文中文", to: 5) == "中文…")
        #expect(TextWidth.truncate("abc", to: 0) == "")
        #expect(TextWidth.truncate("abcdef", to: 1) == "…")
        #expect(TextWidth.truncate("abcdef", to: 2, ellipsis: "...") == "..")
        #expect(TextWidth.truncate("abcdef", to: 4, ellipsis: "") == "abcd")
    }

    @Test("fit pads to exact width with alignment")
    func fit() {
        #expect(TextWidth.fit("ab", to: 5) == "ab   ")
        #expect(TextWidth.fit("ab", to: 5, alignment: .right) == "   ab")
        #expect(TextWidth.fit("ab", to: 5, alignment: .center) == " ab  ")
        #expect(TextWidth.fit("abcdef", to: 4) == "abc…")
        #expect(TextWidth.fit("中", to: 3) == "中 ")
        #expect(TextWidth.fit("x", to: 0) == "")
    }

    @Test("word wrap breaks on spaces and hard-breaks long words")
    func wrap() {
        #expect(TextWidth.wrap("the quick brown fox", width: 10) == ["the quick", "brown fox"])
        #expect(TextWidth.wrap("abcdefghij", width: 4) == ["abcd", "efgh", "ij"])
        #expect(TextWidth.wrap("a\n\nb", width: 5) == ["a", "", "b"])
        #expect(TextWidth.wrap("", width: 5) == [""])
        #expect(TextWidth.wrap("x", width: 0) == [])
        #expect(TextWidth.wrap("中文中文", width: 3) == ["中", "文", "中", "文"])
        #expect(TextWidth.wrap("中", width: 1) == ["中"], "a glyph wider than the line still makes progress")
    }
}

@Suite("Screen buffer")
struct ScreenBufferTests {
    @Test("writes text and reports columns used")
    func write() {
        var buffer = ScreenBuffer(width: 10, height: 2)
        #expect(buffer.write("hello", x: 1, y: 0) == 5)
        #expect(buffer.text(row: 0) == " hello    ")
        #expect(buffer.write("overflowing", x: 6, y: 1) == 4)
        #expect(buffer.text(row: 1) == "      over")
        #expect(buffer.write("abc", x: 0, y: 1, maxWidth: 2) == 2)
        #expect(buffer.text(row: 1).hasPrefix("ab"))
    }

    @Test("out-of-bounds access is ignored")
    func bounds() {
        var buffer = ScreenBuffer(width: 3, height: 1)
        buffer.write("x", x: -5, y: 0)
        buffer.write("x", x: 0, y: 5)
        buffer[10, 10] = Cell("z")
        #expect(buffer[10, 10] == .blank)
        #expect(buffer.text(row: 0) == "   ")
        #expect(buffer.text(row: 3) == "")
        #expect(ScreenBuffer(width: -1, height: -1).cells.isEmpty)
    }

    @Test("wide characters occupy two cells and are repaired when overwritten")
    func wideCharacters() {
        var buffer = ScreenBuffer(width: 6, height: 1)
        buffer.write("中a", x: 0, y: 0)
        #expect(buffer[1, 0].isContinuation)
        #expect(buffer.text(row: 0) == "中a   ")
        buffer.put("x", x: 1, y: 0, style: .plain)
        #expect(buffer.text(row: 0) == " xa   ", "overwriting the right half blanks the left half")
        buffer.write("文", x: 3, y: 0)
        buffer.put("y", x: 3, y: 0, style: .plain)
        #expect(!buffer[4, 0].isContinuation)
        #expect(buffer.text(row: 0) == " xay  ")
    }

    @Test("a wide character at the right edge becomes a space")
    func wideAtEdge() {
        var buffer = ScreenBuffer(width: 3, height: 1)
        buffer.write("ab中", x: 0, y: 0)
        #expect(buffer.text(row: 0) == "ab ")
        var clipped = ScreenBuffer(width: 5, height: 1)
        clipped.put("中", x: 1, y: 0, style: .plain, clip: Rect(x: 0, y: 0, width: 2, height: 1))
        #expect(clipped.text(row: 0) == "     ")
    }

    @Test("zero-width characters are skipped")
    func zeroWidth() {
        var buffer = ScreenBuffer(width: 4, height: 1)
        #expect(buffer.write("a\u{200B}b", x: 0, y: 0) == 2)
        #expect(buffer.text(row: 0) == "ab  ")
    }

    @Test("fill and restyle affect only the clipped rect")
    func fillAndRestyle() {
        var buffer = ScreenBuffer(width: 4, height: 3)
        buffer.fill(Rect(x: 1, y: 1, width: 10, height: 10), character: "#", style: Style(foreground: .red))
        #expect(buffer.plainText == "\n ###\n ###")
        buffer.restyle(Rect(x: 0, y: 1, width: 2, height: 1)) { $0.background(.blue) }
        #expect(buffer[0, 1].style.background == .blue)
        #expect(buffer[1, 1].style == Style(foreground: .red, background: .blue))
        #expect(buffer[2, 1].style.background == .default)
        buffer.fill(Rect(x: 9, y: 9, width: 1, height: 1))
        buffer.restyle(.zero) { $0 }
    }

    @Test("plain text trims trailing spaces per line")
    func plainText() {
        var buffer = ScreenBuffer(width: 5, height: 2)
        buffer.write("hi", x: 0, y: 1)
        #expect(buffer.plainText == "\nhi")
        #expect(buffer.lines == ["     ", "hi   "])
        #expect(buffer.bounds == Rect(x: 0, y: 0, width: 5, height: 2))
    }
}

@Suite("Layout")
struct LayoutTests {
    @Test("rect geometry helpers")
    func geometry() {
        let rect = Rect(x: 2, y: 3, width: 10, height: 4)
        #expect(rect.maxX == 12)
        #expect(rect.maxY == 7)
        #expect(rect.contains(x: 2, y: 3))
        #expect(!rect.contains(x: 12, y: 3))
        #expect(rect.inset(by: 1) == Rect(x: 3, y: 4, width: 8, height: 2))
        #expect(rect.inset(by: 5).isEmpty)
        #expect(rect.inset(top: 1, left: 2) == Rect(x: 4, y: 4, width: 8, height: 3))
        #expect(rect.centered(width: 4, height: 2) == Rect(x: 5, y: 4, width: 4, height: 2))
        #expect(rect.centered(width: 40, height: 40) == rect)
        #expect(rect.row(1) == Rect(x: 2, y: 4, width: 10, height: 1))
        #expect(rect.row(9).isEmpty)
        #expect(Rect(x: 0, y: 0, width: -3, height: 2).width == 0)
        #expect(rect.description == "(2,3 10x4)")
    }

    @Test("intersection clips and handles disjoint rects")
    func intersection() {
        let a = Rect(x: 0, y: 0, width: 10, height: 10)
        #expect(a.intersection(Rect(x: 5, y: 5, width: 10, height: 10)) == Rect(x: 5, y: 5, width: 5, height: 5))
        #expect(a.intersection(Rect(x: 20, y: 20, width: 1, height: 1)).isEmpty)
    }

    @Test("solver honours fixed and percent, sharing the rest by weight")
    func solve() {
        #expect(LayoutConstraint.solve([.fixed(10), .fill(1)], length: 50) == [10, 40])
        #expect(LayoutConstraint.solve([.percent(50), .fill(1), .fill(1)], length: 101) == [50, 26, 25])
        #expect(LayoutConstraint.solve([.fill(1), .fill(2)], length: 10) == [4, 6])
        #expect(LayoutConstraint.solve([.fill(1), .fill(1), .fill(1)], length: 10) == [4, 3, 3])
        #expect(LayoutConstraint.solve([.fixed(5), .fixed(5)], length: 20) == [5, 5])
        #expect(LayoutConstraint.solve([], length: 20) == [])
    }

    @Test("overflowing fixed sizes shrink from the end")
    func overflow() {
        #expect(LayoutConstraint.solve([.fixed(10), .fixed(10), .fixed(10)], length: 15) == [10, 5, 0])
        #expect(LayoutConstraint.solve([.fixed(10), .fill(1)], length: 4) == [4, 0])
        #expect(LayoutConstraint.solve([.percent(150), .fixed(-3)], length: 10) == [10, 0])
    }

    @Test("spacing is subtracted between parts")
    func spacing() {
        let parts = Rect(x: 0, y: 0, width: 21, height: 5).split(.horizontal, [.fill(1), .fill(1)], spacing: 1)
        #expect(parts == [Rect(x: 0, y: 0, width: 10, height: 5), Rect(x: 11, y: 0, width: 10, height: 5)])
        let rows = Rect(x: 0, y: 2, width: 5, height: 10).split(.vertical, [.fixed(3), .fill(1)])
        #expect(rows == [Rect(x: 0, y: 2, width: 5, height: 3), Rect(x: 0, y: 5, width: 5, height: 7)])
        #expect(Rect(x: 0, y: 0, width: 5, height: 5).split(.vertical, []).isEmpty)
    }

    @Test("split sizes always sum to the available length", arguments: [0, 1, 7, 33, 80, 211])
    func conservation(length: Int) {
        let constraints: [LayoutConstraint] = [.percent(30), .fill(2), .fixed(5), .fill(1)]
        let sizes = LayoutConstraint.solve(constraints, length: length)
        #expect(sizes.reduce(0, +) == length)
        #expect(sizes.allSatisfy { $0 >= 0 })
    }
}

@Suite("Frame renderer")
struct FrameRendererTests {
    @Test("the first frame clears and paints everything")
    func firstFrame() {
        var buffer = ScreenBuffer(width: 3, height: 1)
        buffer.write("ab", x: 0, y: 0)
        var renderer = FrameRenderer(colorMode: .trueColor)
        let output = renderer.render(buffer)
        #expect(output == "\u{1B}[0m\u{1B}[2J\u{1B}[1;1H\u{1B}[0mab \u{1B}[0m")
    }

    @Test("identical frames produce no output")
    func identical() {
        var buffer = ScreenBuffer(width: 4, height: 2)
        buffer.write("same", x: 0, y: 0)
        var renderer = FrameRenderer(colorMode: .trueColor)
        _ = renderer.render(buffer)
        #expect(renderer.render(buffer) == "")
    }

    @Test("only changed cells are emitted, with one cursor move per run")
    func minimalDiff() {
        var first = ScreenBuffer(width: 10, height: 2)
        first.write("hello", x: 0, y: 0)
        var second = first
        second.write("J", x: 0, y: 1)
        second.write("XY", x: 3, y: 0)
        let output = FrameRenderer.diff(from: first, to: second, colorMode: .trueColor)
        #expect(output == "\u{1B}[1;4H\u{1B}[0mXY\u{1B}[2;1HJ\u{1B}[0m")
    }

    @Test("style changes are emitted only when the style differs")
    func styles() {
        let previous = ScreenBuffer(width: 3, height: 1)
        var next = previous
        let red = Style(foreground: .ansi(1))
        next.write("ab", x: 0, y: 0, style: red)
        next.write("c", x: 2, y: 0)
        let output = FrameRenderer.diff(from: previous, to: next, colorMode: .basic16)
        #expect(output == "\u{1B}[1;1H\u{1B}[0;31mab\u{1B}[0mc\u{1B}[0m")
        #expect(!FrameRenderer.diff(from: previous, to: next, colorMode: .none).contains("31"))
    }

    @Test("wide characters advance the cursor by two")
    func wide() {
        let previous = ScreenBuffer(width: 4, height: 1)
        var next = previous
        next.write("中x", x: 0, y: 0)
        let output = FrameRenderer.diff(from: previous, to: next, colorMode: .trueColor)
        #expect(output == "\u{1B}[1;1H\u{1B}[0m中x\u{1B}[0m")
    }

    @Test("a resize or invalidate forces a full repaint")
    func resize() {
        var renderer = FrameRenderer(colorMode: .trueColor)
        _ = renderer.render(ScreenBuffer(width: 2, height: 1))
        #expect(renderer.render(ScreenBuffer(width: 3, height: 1)).contains("\u{1B}[2J"))
        renderer.invalidate()
        #expect(renderer.render(ScreenBuffer(width: 3, height: 1)).contains("\u{1B}[2J"))
    }

    @Test("terminal control sequences")
    func sequences() {
        #expect(TerminalSequence.moveTo(x: 0, y: 0) == "\u{1B}[1;1H")
        #expect(TerminalSequence.moveTo(x: 9, y: 4) == "\u{1B}[5;10H")
        #expect(TerminalSequence.setTitle("dtop\u{07}\u{1B}x") == "\u{1B}]0;dtopx\u{07}")
        #expect(TerminalSequence.enableMouse.contains("1006"))
        #expect(TerminalSequence.disableMouse.contains("1006l"))
    }
}
