import Testing
@testable import DtopUI

@Suite("Box")
struct BoxTests {
    @Test("draws borders, title and label and returns the inner rect")
    func box() {
        var buffer = ScreenBuffer(width: 20, height: 4)
        let inner = Box.draw(into: &buffer, rect: buffer.bounds, title: "cpu", label: "42%", borderStyle: .plain)
        #expect(inner == Rect(x: 1, y: 1, width: 18, height: 2))
        #expect(buffer.lines == [
            "╭─ cpu ────── 42% ─╮",
            "│                  │",
            "│                  │",
            "╰──────────────────╯"
        ])
    }

    @Test("long titles are truncated and labels dropped when there is no room")
    func crowded() {
        var buffer = ScreenBuffer(width: 12, height: 3)
        Box.draw(into: &buffer, rect: buffer.bounds, title: "a very long title", label: "label", borderStyle: .plain)
        #expect(buffer.text(row: 0) == "╭─ a ver… ─╮")
    }

    @Test("ascii borders and degenerate sizes")
    func asciiAndTiny() {
        var buffer = ScreenBuffer(width: 4, height: 2)
        #expect(Box.draw(into: &buffer, rect: buffer.bounds, borderStyle: .plain, characters: .ascii) == Rect(x: 1, y: 1, width: 2, height: 0))
        #expect(buffer.lines == ["+--+", "+--+"])
        var tiny = ScreenBuffer(width: 1, height: 1)
        #expect(Box.draw(into: &tiny, rect: tiny.bounds, borderStyle: .plain) == .zero)
    }

    @Test("border styles resolve by name")
    func names() {
        #expect(BorderCharacters.named("Rounded") == .rounded)
        #expect(BorderCharacters.named("square") == .single)
        #expect(BorderCharacters.named("double") == .double)
        #expect(BorderCharacters.named("thick") == .heavy)
        #expect(BorderCharacters.named("ascii") == .ascii)
        #expect(BorderCharacters.named("wavy") == nil)
    }
}

@Suite("Table")
struct TableTests {
    let columns = [
        TableColumn("NAME", width: .flexible(min: 6, weight: 2)),
        TableColumn("CPU", width: .fixed(5), alignment: .right),
        TableColumn("IMAGE", width: .flexible(min: 6, weight: 1), priority: 2),
        TableColumn("PORTS", width: .flexible(min: 8, weight: 1), priority: 3)
    ]
    let styles = TableStyles(header: Style(attributes: .bold), row: .plain,
                             selected: Style(background: .blue), scrollIndicator: .plain)

    @Test("distributes spare width by weight")
    func widths() {
        // min total = 6+5+6+8 + 3 spaces = 28; 12 spare split 2:1:1.
        #expect(Table.columnWidths(columns, totalWidth: 40) == [12, 5, 9, 11])
        #expect(Table.columnWidths(columns, totalWidth: 28) == [6, 5, 6, 8])
    }

    @Test("hides the least important columns first when narrow")
    func hiding() {
        #expect(Table.columnWidths(columns, totalWidth: 25) == [10, 5, 8, nil])
        #expect(Table.columnWidths(columns, totalWidth: 12) == [6, 5, nil, nil])
        #expect(Table.columnWidths(columns, totalWidth: 3) == [3, nil, nil, nil])
        #expect(Table.columnWidths([TableColumn("X", width: .fixed(5))], totalWidth: 3) == [3])
        #expect(Table.columnWidths([TableColumn("X", width: .fixed(5))], totalWidth: 30) == [5])
    }

    @Test("scroll offset keeps the selection visible", arguments: [
        (nil as Int?, 0, 5, 20, 0), (0, 0, 5, 20, 0), (7, 0, 5, 20, 3), (2, 10, 5, 20, 2), (19, 0, 5, 20, 15),
        (50, 0, 5, 20, 15), (3, 18, 5, 20, 3), (nil, 99, 5, 20, 15), (0, 0, 0, 20, 0), (0, 4, 5, 0, 0)
    ])
    func scroll(selected: Int?, current: Int, visible: Int, count: Int, expected: Int) {
        #expect(Table.scrollOffset(selected: selected, current: current, visibleRows: visible, rowCount: count) == expected)
    }

    @Test("draws header and rows, highlights the selection and records hit rects")
    func draw() {
        var buffer = ScreenBuffer(width: 28, height: 4)
        let rows = (0..<5).map { [TableCell("row\($0)"), TableCell("\($0).0"), TableCell("img"), TableCell("80/tcp")] }
        let layout = Table.draw(into: &buffer, rect: buffer.bounds, columns: columns, rows: rows, selected: 3,
                                scrollOffset: 0, styles: styles)
        #expect(buffer.text(row: 0) == "NAME     CPU IMAGE  PORTS   ")
        #expect(layout.scrollOffset == 1)
        #expect(layout.rowRects.map(\.index) == [1, 2, 3])
        #expect(buffer.text(row: 1).hasPrefix("row1     1.0 img"))
        #expect(buffer[0, 3].style.background == .blue)
        #expect(buffer[0, 2].style.background == .default)
        #expect(buffer[27, 1].character == "▲")
        #expect(buffer[27, 3].character == "▼")
        #expect(layout.rowIndex(atX: 4, y: 2) == 2)
        #expect(layout.rowIndex(atX: 4, y: 0) == nil)
        #expect(layout.headerRect == Rect(x: 0, y: 0, width: 28, height: 1))
    }

    @Test("cell styles layer over row styles")
    func merge() {
        let base = Style(foreground: .white, background: .blue, attributes: .bold)
        #expect(Table.merge(base, nil) == base)
        #expect(Table.merge(base, Style(foreground: .red)) == Style(foreground: .red, background: .blue, attributes: .bold))
        #expect(Table.merge(base, Style(background: .green, attributes: .dim))
            == Style(foreground: .white, background: .green, attributes: [.bold, .dim]))
    }

    @Test("works without a header and with missing cells")
    func headless() {
        var buffer = ScreenBuffer(width: 28, height: 2)
        let layout = Table.draw(into: &buffer, rect: buffer.bounds, columns: columns, rows: [[TableCell("only")]],
                                selected: nil, scrollOffset: 0, styles: styles, showHeader: false)
        #expect(layout.rowRects.first?.rect.y == 0)
        #expect(buffer.text(row: 0).hasPrefix("only"))
        #expect(layout.headerRect == .zero)
        #expect(TableLayout.empty.rowRects.isEmpty)
    }
}

@Suite("Charts")
struct ChartTests {
    @Test("gauge bars use eighth blocks", arguments: [
        (0.0, 4, "    "), (1.0, 4, "████"), (0.5, 4, "██  "), (0.53, 4, "██▏ "), (2.0, 3, "███"), (-1.0, 3, "   "),
        (Double.nan, 2, "  "), (0.5, 0, "")
    ])
    func gauge(fraction: Double, width: Int, expected: String) {
        #expect(Gauge.bar(fraction: fraction, width: width) == expected)
    }

    @Test("gauge drawing uses track dots for the empty part")
    func gaugeDraw() {
        var buffer = ScreenBuffer(width: 4, height: 1)
        Gauge.draw(into: &buffer, x: 0, y: 0, width: 4, fraction: 0.5, filled: Style(foreground: .green), track: .plain)
        #expect(buffer.text(row: 0) == "██··")
        #expect(buffer[0, 0].style.foreground == .green)
    }

    @Test("braille graph maps values to dot heights, newest on the right")
    func braille() {
        // Two cells wide, one row: four samples.
        #expect(BrailleGraph.render(values: [0, 0.25, 0.5, 1], width: 2, height: 1) == ["⢀⣼"])
        #expect(BrailleGraph.render(values: [1, 1, 1, 1], width: 2, height: 1) == ["⣿⣿"])
        #expect(BrailleGraph.render(values: [0, 0, 0, 0], width: 2, height: 1) == ["  "])
    }

    @Test("braille graph spans multiple rows and pads missing history")
    func brailleRows() {
        let rows = BrailleGraph.render(values: [1, 0.5], width: 2, height: 2)
        #expect(rows == ["  ⡇", "  ⣿"].map { String($0.dropFirst(1)) })
        #expect(BrailleGraph.render(values: [], width: 3, height: 2) == ["   ", "   "])
        #expect(BrailleGraph.render(values: [1], width: 0, height: 2).isEmpty)
    }

    @Test("tiny non-zero values still show one dot; values are clamped")
    func brailleClamp() {
        #expect(BrailleGraph.render(values: [0.001, 7], width: 1, height: 1) == ["⣸"])
        #expect(BrailleGraph.render(values: [-3, .nan], width: 1, height: 1) == [" "])
    }

    @Test("graph drawing scales by maximum and colors rows by height")
    func graphDraw() {
        var buffer = ScreenBuffer(width: 1, height: 2)
        BrailleGraph.draw(into: &buffer, rect: buffer.bounds, values: [50, 100], maximum: 100) { fraction in
            Style(foreground: fraction > 0.5 ? .red : .green)
        }
        #expect(buffer.lines == ["⢸", "⣿"])
        #expect(buffer[0, 0].style.foreground == .red)
        #expect(buffer[0, 1].style.foreground == .green)
        var empty = ScreenBuffer(width: 0, height: 0)
        BrailleGraph.draw(into: &empty, rect: .zero, values: [1], maximum: 0) { _ in .plain }
    }

    @Test("sparkline scales to the peak or an explicit maximum")
    func sparkline() {
        #expect(Sparkline.render(values: [0, 1, 2, 4], width: 4) == " ▃▅█")
        #expect(Sparkline.render(values: [1, 2], width: 4) == "  ▅█")
        #expect(Sparkline.render(values: [5, 5, 5], width: 2, maximum: 10) == "▅▅")
        #expect(Sparkline.render(values: [0, 0], width: 2) == "  ")
        #expect(Sparkline.render(values: [.nan, 1], width: 2) == " █")
        #expect(Sparkline.render(values: [1], width: 0) == "")
    }
}
