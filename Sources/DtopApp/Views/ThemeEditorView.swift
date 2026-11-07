import DtopCore
import DtopUI

/// The theme menu: a panel docked on the right while the rest of the UI keeps rendering live
/// with the draft theme, so every adjustment is visible on real data immediately.
enum ThemeEditorView {
    static let preferredWidth = 58

    static func panelRect(for size: Rect) -> Rect {
        let width = min(preferredWidth, size.width - 2)
        return Rect(x: size.maxX - width, y: 1, width: width, height: max(0, size.height - 2))
    }

    /// Draws the panel and returns hit rects for its rows.
    static func draw(into buffer: inout ScreenBuffer, editor: ThemeEditor) -> [RowHit] {
        let theme = editor.draft
        let rect = panelRect(for: buffer.bounds)
        guard rect.width >= 30, rect.height >= 12 else { return [] }
        let panel = Style(foreground: theme[.foreground], background: theme[.dialogBackground])
        buffer.fill(rect, style: panel)
        let inner = Box.draw(into: &buffer, rect: rect, title: "theme · \(editor.draft.name)",
                             label: editor.isDirty ? "unsaved" : "saved",
                             borderStyle: panel.foreground(theme[.dialogBorder]),
                             titleStyle: panel.foreground(theme[.title]).bold, characters: theme.border)
        var y = inner.y

        // Live preview strip.
        var x = inner.x + 1
        for (label, role) in [("● running", ThemeRole.statusRunning), ("◐ paused", .statusPaused),
                              ("◌ exited", .statusExited), ("↻ restart", .statusRestarting), ("✕ dead", .statusDead)] {
            x += buffer.write(label + "  ", x: x, y: y, style: panel.foreground(theme[role]), maxWidth: inner.maxX - x)
        }
        y += 1
        let barWidth = max(4, inner.width - 2)
        for column in 0..<barWidth {
            let fraction = Double(column) / Double(max(1, barWidth - 1))
            buffer.put("█", x: inner.x + 1 + column, y: y, style: panel.foreground(theme.gradient(fraction)))
        }
        y += 1
        x = inner.x + 1
        x += buffer.write(" selected row ", x: x, y: y, style: theme.selection.adding(.bold))
        x += buffer.write("  ", x: x, y: y, style: panel)
        for (label, role) in [("accent", ThemeRole.accent), ("muted", .muted), ("warn", .warning), ("error", .error)] {
            x += buffer.write(label + " ", x: x, y: y, style: panel.foreground(theme[role]), maxWidth: inner.maxX - x)
        }
        y += 1
        buffer.write(String(repeating: "─", count: inner.width), x: inner.x, y: y, style: panel.foreground(theme[.border]))
        y += 1

        // Rows, scrolled to keep the selection visible; the bottom four lines are reserved.
        let listHeight = max(1, inner.maxY - y - 4)
        let rows = ThemeEditor.rows
        let offset = Table.scrollOffset(selected: editor.selected, current: max(0, editor.selected - listHeight / 2),
                                        visibleRows: listHeight, rowCount: rows.count)
        var hits: [RowHit] = []
        for (line, index) in (offset..<min(rows.count, offset + listHeight)).enumerated() {
            let row = rows[index]
            let rowY = y + line
            let isSelected = index == editor.selected
            let rowRect = Rect(x: inner.x, y: rowY, width: inner.width, height: 1)
            let base = isSelected ? theme.selection : panel
            buffer.fill(rowRect, style: base)
            hits.append(RowHit(index: index, rect: rowRect))
            var column = inner.x + 1
            column += buffer.write(editor.isModified(row) ? "•" : " ", x: column, y: rowY,
                                   style: base.foreground(theme[.warning]))
            column += buffer.write(TextWidth.fit(row.label, to: 19) + " ", x: column, y: rowY,
                                   style: isSelected ? base.bold : base)
            if case .role(let role) = row {
                column += buffer.write("████", x: column, y: rowY, style: base.foreground(
                    editor.draft[role] == .default ? theme[.foreground] : editor.draft[role]))
                column += buffer.write(" ", x: column, y: rowY, style: base)
                column += buffer.write(TextWidth.fit(editor.valueText(for: row), to: 12) + " ", x: column, y: rowY,
                                       style: base.foreground(isSelected ? theme[.selectionForeground] : theme[.muted]))
                if isSelected, let components = editor.draft[role].rgbComponents {
                    let values = [components.0, components.1, components.2]
                    for channel in ThemeEditor.Channel.allCases where column < inner.maxX - 4 {
                        let text = "\(channel.label)\(TextWidth.fit(String(values[channel.rawValue]), to: 3, alignment: .right)) "
                        let style = channel == editor.channel ? base.adding([.reverse, .bold]) : base
                        column += buffer.write(text, x: column, y: rowY, style: style, maxWidth: inner.maxX - column)
                    }
                }
            } else {
                buffer.write(editor.valueText(for: row), x: column, y: rowY,
                             style: base.foreground(isSelected ? theme[.selectionForeground] : theme[.accent]),
                             maxWidth: inner.maxX - column)
            }
        }

        // Input / message line and key hints.
        let statusY = inner.maxY - 3
        buffer.write(String(repeating: "─", count: inner.width), x: inner.x, y: statusY, style: panel.foreground(theme[.border]))
        let fieldY = statusY + 1
        switch editor.field {
        case .none:
            if let message = editor.message {
                buffer.write(" " + message, x: inner.x, y: fieldY, style: panel.foreground(theme[.warning]), maxWidth: inner.width)
            } else if case .role = editor.selectedRow {
                buffer.write(" ←→ change \(editor.channel.label) · tab channel · ⏎ type a color", x: inner.x, y: fieldY,
                             style: panel.foreground(theme[.muted]), maxWidth: inner.width)
            } else {
                buffer.write(" ←→ or ⏎ to change", x: inner.x, y: fieldY, style: panel.foreground(theme[.muted]),
                             maxWidth: inner.width)
            }
        case .colorInput(let text):
            var column = inner.x
            column += buffer.write(" color: ", x: column, y: fieldY, style: panel.foreground(theme[.accent]).bold)
            column += buffer.write(text, x: column, y: fieldY, style: panel, maxWidth: inner.maxX - column - 1)
            buffer.write("▏", x: column, y: fieldY, style: panel.foreground(theme[.accent]))
            if let message = editor.message {
                buffer.write(message, x: inner.x + 1, y: fieldY + 1, style: panel.foreground(theme[.error]), maxWidth: inner.width - 1)
                return hits
            }
        case .saveName(let text):
            var column = inner.x
            column += buffer.write(" save as: ", x: column, y: fieldY, style: panel.foreground(theme[.accent]).bold)
            column += buffer.write(text, x: column, y: fieldY, style: panel, maxWidth: inner.maxX - column - 1)
            buffer.write("▏", x: column, y: fieldY, style: panel.foreground(theme[.accent]))
            if let message = editor.message {
                buffer.write(message, x: inner.x + 1, y: fieldY + 1, style: panel.foreground(theme[.error]), maxWidth: inner.width - 1)
                return hits
            }
            buffer.write(" ⏎ save · esc back", x: inner.x, y: fieldY + 1, style: panel.foreground(theme[.muted]))
            return hits
        case .confirmDiscard:
            buffer.write(" Discard unsaved theme changes? y discard · n keep editing · s save", x: inner.x, y: fieldY,
                         style: panel.foreground(theme[.warning]).bold, maxWidth: inner.width)
            return hits
        }
        buffer.write(" ↑↓ select · d reset · r revert all · s save · esc close", x: inner.x, y: fieldY + 1,
                     style: panel.foreground(theme[.muted]), maxWidth: inner.width)
        return hits
    }
}
