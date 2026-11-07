public enum ColumnWidth: Sendable, Equatable {
    case fixed(Int)
    /// Grows to share leftover space by weight, never shrinking below `min`.
    case flexible(min: Int, weight: Int)

    var minimum: Int {
        switch self {
        case .fixed(let value): return value
        case .flexible(let min, _): return min
        }
    }
}

public struct TableColumn: Sendable, Equatable {
    public var title: String
    public var width: ColumnWidth
    public var alignment: TextAlignment
    /// Lower is more important; the highest-numbered columns are hidden first when space runs out.
    public var priority: Int

    public init(_ title: String, width: ColumnWidth, alignment: TextAlignment = .left, priority: Int = 0) {
        self.title = title
        self.width = width
        self.alignment = alignment
        self.priority = priority
    }
}

public struct TableCell: Sendable, Equatable {
    public var text: String
    public var style: Style?

    public init(_ text: String, style: Style? = nil) {
        self.text = text
        self.style = style
    }
}

public struct TableStyles: Sendable, Equatable {
    public var header: Style
    public var row: Style
    public var selected: Style
    public var scrollIndicator: Style

    public init(header: Style, row: Style, selected: Style, scrollIndicator: Style) {
        self.header = header
        self.row = row
        self.selected = selected
        self.scrollIndicator = scrollIndicator
    }
}

/// Where each visible row landed, for mouse hit testing.
public struct TableLayout: Sendable, Equatable {
    public var headerRect: Rect
    public var rowRects: [(index: Int, rect: Rect)]
    public var columnWidths: [Int?]
    public var scrollOffset: Int

    public init(headerRect: Rect, rowRects: [(index: Int, rect: Rect)], columnWidths: [Int?], scrollOffset: Int) {
        self.headerRect = headerRect
        self.rowRects = rowRects
        self.columnWidths = columnWidths
        self.scrollOffset = scrollOffset
    }

    public static let empty = TableLayout(headerRect: .zero, rowRects: [], columnWidths: [], scrollOffset: 0)

    public static func ==(lhs: TableLayout, rhs: TableLayout) -> Bool {
        lhs.headerRect == rhs.headerRect && lhs.columnWidths == rhs.columnWidths && lhs.scrollOffset == rhs.scrollOffset
            && lhs.rowRects.count == rhs.rowRects.count
            && zip(lhs.rowRects, rhs.rowRects).allSatisfy { $0.index == $1.index && $0.rect == $1.rect }
    }

    public func rowIndex(atX x: Int, y: Int) -> Int? {
        rowRects.first { $0.rect.contains(x: x, y: y) }?.index
    }
}

public enum Table {
    /// Resolves column widths for the available space. Hidden columns come back as `nil`.
    public static func columnWidths(_ columns: [TableColumn], totalWidth: Int, spacing: Int = 1) -> [Int?] {
        var visible = Array(columns.indices)
        func required(_ indices: [Int]) -> Int {
            indices.map { columns[$0].width.minimum }.reduce(0, +) + max(0, indices.count - 1) * spacing
        }
        while visible.count > 1, required(visible) > totalWidth {
            // Drop the least important column (ties: the rightmost).
            let victim = visible.max { lhs, rhs in
                columns[lhs].priority == columns[rhs].priority ? lhs < rhs : columns[lhs].priority < columns[rhs].priority
            }!
            visible.removeAll { $0 == victim }
        }

        var widths: [Int?] = Array(repeating: nil, count: columns.count)
        for index in visible { widths[index] = columns[index].width.minimum }
        var leftover = totalWidth - required(visible)
        if visible.count == 1, let only = visible.first {
            if case .fixed(let fixed) = columns[only].width {
                widths[only] = max(0, min(fixed, totalWidth))
            } else {
                widths[only] = max(0, totalWidth)
            }
            return widths
        }
        let flexible = visible.compactMap { index -> (Int, Int)? in
            if case .flexible(_, let weight) = columns[index].width, weight > 0 { return (index, weight) }
            return nil
        }
        let totalWeight = flexible.map(\.1).reduce(0, +)
        if leftover > 0, totalWeight > 0 {
            var given = 0
            for (index, weight) in flexible {
                let share = leftover * weight / totalWeight
                widths[index]! += share
                given += share
            }
            leftover -= given
            for (index, _) in flexible where leftover > 0 {
                widths[index]! += 1
                leftover -= 1
            }
        }
        return widths
    }

    /// Adjusts a scroll offset so `selected` stays visible.
    public static func scrollOffset(selected: Int?, current: Int, visibleRows: Int, rowCount: Int) -> Int {
        guard visibleRows > 0, rowCount > 0 else { return 0 }
        let maxOffset = max(0, rowCount - visibleRows)
        var offset = min(max(0, current), maxOffset)
        if let selected {
            let clamped = min(max(0, selected), rowCount - 1)
            if clamped < offset { offset = clamped }
            if clamped >= offset + visibleRows { offset = clamped - visibleRows + 1 }
        }
        return min(max(0, offset), maxOffset)
    }

    @discardableResult
    public static func draw(
        into buffer: inout ScreenBuffer,
        rect: Rect,
        columns: [TableColumn],
        rows: [[TableCell]],
        selected: Int?,
        scrollOffset: Int,
        styles: TableStyles,
        spacing: Int = 1,
        showHeader: Bool = true
    ) -> TableLayout {
        let widths = columnWidths(columns, totalWidth: rect.width, spacing: spacing)
        var y = rect.y
        let headerRect = showHeader ? Rect(x: rect.x, y: rect.y, width: rect.width, height: rect.height > 0 ? 1 : 0) : .zero
        if showHeader, rect.height > 0 {
            buffer.fill(headerRect, style: styles.header)
            drawRow(into: &buffer, x: rect.x, y: y, maxX: rect.maxX, cells: columns.map { TableCell($0.title) },
                    columns: columns, widths: widths, spacing: spacing, baseStyle: styles.header)
            y += 1
        }
        let visibleRows = max(0, rect.maxY - y)
        let offset = Self.scrollOffset(selected: selected, current: scrollOffset, visibleRows: visibleRows, rowCount: rows.count)
        var rowRects: [(index: Int, rect: Rect)] = []
        for (line, index) in (offset..<min(rows.count, offset + visibleRows)).enumerated() {
            let rowY = y + line
            let isSelected = index == selected
            let base = isSelected ? styles.selected : styles.row
            let rowRect = Rect(x: rect.x, y: rowY, width: rect.width, height: 1)
            buffer.fill(rowRect, style: base)
            drawRow(into: &buffer, x: rect.x, y: rowY, maxX: rect.maxX, cells: rows[index], columns: columns,
                    widths: widths, spacing: spacing, baseStyle: base)
            if isSelected {
                // Keep per-cell foreground accents but force the selection background.
                buffer.restyle(rowRect) { $0.background(styles.selected.background).adding(styles.selected.attributes) }
            }
            rowRects.append((index, rowRect))
        }
        // Scroll indicators in the last column when content overflows.
        if rows.count > visibleRows, visibleRows > 0, rect.width > 0 {
            if offset > 0 { buffer.put("▲", x: rect.maxX - 1, y: y, style: styles.scrollIndicator) }
            if offset + visibleRows < rows.count {
                buffer.put("▼", x: rect.maxX - 1, y: y + visibleRows - 1, style: styles.scrollIndicator)
            }
        }
        return TableLayout(headerRect: headerRect, rowRects: rowRects, columnWidths: widths, scrollOffset: offset)
    }

    private static func drawRow(
        into buffer: inout ScreenBuffer,
        x: Int,
        y: Int,
        maxX: Int,
        cells: [TableCell],
        columns: [TableColumn],
        widths: [Int?],
        spacing: Int,
        baseStyle: Style
    ) {
        var column = x
        var first = true
        for (index, width) in widths.enumerated() {
            guard let width else { continue }
            if !first { column += spacing }
            first = false
            guard column < maxX else { break }
            let cell = index < cells.count ? cells[index] : TableCell("")
            let style = merge(baseStyle, cell.style)
            let text = TextWidth.fit(cell.text, to: min(width, maxX - column), alignment: columns[index].alignment)
            buffer.write(text, x: column, y: y, style: style, maxWidth: maxX - column)
            column += width
        }
    }

    /// Layers a cell's style over the row style; `.default` colors inherit from the row.
    static func merge(_ base: Style, _ overlay: Style?) -> Style {
        guard let overlay else { return base }
        return Style(
            foreground: overlay.foreground == .default ? base.foreground : overlay.foreground,
            background: overlay.background == .default ? base.background : overlay.background,
            attributes: base.attributes.union(overlay.attributes)
        )
    }
}
