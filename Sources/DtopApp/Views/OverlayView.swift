import DtopCore
import DtopUI

enum OverlayView {
    static let helpSections: [(String, [(String, String)])] = [
        ("Navigate", [
            ("↑↓ j k", "move selection"), ("PgUp PgDn", "page"), ("g G Home End", "first / last"),
            ("← → Tab h l", "switch view"), ("1-5", "jump to view"), ("mouse", "click, wheel"),
            ("Enter", "details / fold project"), ("v", "cycle detail pane"), ("d", "toggle details"),
            ("[ ]", "previous / next host"), ("H", "host list")
        ]),
        ("View", [
            ("/", "filter (Esc clears)"), ("s / S", "sort key / direction"), ("a", "all · running · stopped"),
            ("T F2", "theme menu (live, save/revert)"), ("t", "cycle theme (not saved)"),
            ("f", "follow / pause the log stream"),
            ("space", "pause updates"), ("R F5", "refresh now")
        ]),
        ("Manage", [
            ("u", "start / unpause"), ("x", "stop"), ("r", "restart"), ("p", "pause / unpause"), ("K", "kill"),
            ("D", "remove / compose down"), ("U", "compose up"), ("X", "prune this view"), ("q Ctrl-C", "quit")
        ])
    ]

    /// The host list: every monitored host with its status and load. Returns row hits.
    static func drawHostPicker(into buffer: inout ScreenBuffer, selected: Int, context: RenderContext) -> [HostHit] {
        let theme = context.theme
        let hosts = context.hosts
        guard !hosts.isEmpty else { return [] }
        let width = min(buffer.width - 4, 96)
        let height = min(buffer.height - 2, hosts.count + 5)
        guard width > 20, height > 4 else { return [] }
        let rect = buffer.bounds.centered(width: width, height: height)
        let dialog = Style(foreground: theme[.foreground], background: theme[.dialogBackground])
        buffer.fill(rect, style: dialog)
        let inner = Box.draw(into: &buffer, rect: rect, title: "hosts (\(hosts.count))", label: "⏎ switch · esc close",
                             borderStyle: dialog.foreground(theme[.dialogBorder]),
                             titleStyle: dialog.foreground(theme[.title]).bold, characters: theme.border)
        let columns = [
            TableColumn("", width: .fixed(2)),
            TableColumn("HOST", width: .flexible(min: 10, weight: 1)),
            TableColumn("STATUS", width: .fixed(10)),
            TableColumn("UP", width: .fixed(7), alignment: .right),
            TableColumn("CPU%", width: .fixed(6), alignment: .right, priority: 2),
            TableColumn("MEM", width: .fixed(8), alignment: .right, priority: 3),
            TableColumn("ENDPOINT", width: .flexible(min: 12, weight: 2), priority: 1)
        ]
        let rows: [[TableCell]] = hosts.enumerated().map { index, host in
            let status: (String, ThemeRole)
            switch host.connection {
            case .connected: status = ("connected", .statusRunning)
            case .connecting: status = ("connecting", .warning)
            case .disconnected: status = ("offline", .error)
            }
            return [
                TableCell(index < 9 ? String(index + 1) : "", style: theme.style(.muted)),
                TableCell((index == context.state.activeHost ? "▸ " : "  ") + host.name, style: theme.style(.title)),
                TableCell(HeaderView.connectionSymbol(host.connection) + " " + status.0, style: theme.style(status.1)),
                TableCell(host.total > 0 || host.connection.isConnected ? "\(host.running)/\(host.total)" : "-"),
                TableCell(host.cpuPercent.map { Format.percent($0, decimals: 1) } ?? "-"),
                TableCell(host.memoryUsed.map { Format.compactBytes(Double($0)) } ?? "-"),
                TableCell(host.description, style: theme.style(.muted))
            ]
        }
        let styles = TableStyles(header: dialog.foreground(theme[.tableHeader]).bold, row: dialog,
                                 selected: theme.selection.adding(.bold), scrollIndicator: theme.style(.muted))
        let layout = Table.draw(into: &buffer, rect: inner, columns: columns, rows: rows, selected: selected,
                                scrollOffset: 0, styles: styles)
        if case .disconnected(let message, _) = hosts[min(selected, hosts.count - 1)].connection, inner.height > hosts.count + 2 {
            buffer.write(message, x: inner.x + 1, y: inner.maxY - 1, style: dialog.foreground(theme[.error]),
                         maxWidth: inner.width - 2)
        }
        return layout.rowRects.map { HostHit(index: $0.index, rect: $0.rect) }
    }

    static func drawHelp(into buffer: inout ScreenBuffer, context: RenderContext) {
        let theme = context.theme
        let rowCount = helpSections.reduce(0) { $0 + $1.1.count + 2 }
        let height = min(buffer.height - 2, rowCount + 3)
        let width = min(buffer.width - 4, 56)
        let rect = buffer.bounds.centered(width: width, height: height)
        let dialog = Style(foreground: theme[.foreground], background: theme[.dialogBackground])
        buffer.fill(rect, style: dialog)
        let inner = Box.draw(into: &buffer, rect: rect, title: "dtop \(AppProfile.current.version) — keys",
                             label: "any key closes", borderStyle: dialog.foreground(theme[.dialogBorder]),
                             titleStyle: dialog.foreground(theme[.title]).bold, characters: theme.border)
        var y = inner.y
        for (title, bindings) in helpSections {
            guard y < inner.maxY else { break }
            buffer.write(title, x: inner.x + 1, y: y, style: dialog.foreground(theme[.accent]).bold)
            y += 1
            for (keys, action) in bindings where y < inner.maxY {
                buffer.write(TextWidth.fit(keys, to: 15), x: inner.x + 2, y: y, style: dialog.foreground(theme[.title]))
                buffer.write(action, x: inner.x + 18, y: y, style: dialog.foreground(theme[.muted]), maxWidth: inner.width - 18)
                y += 1
            }
            y += 1
        }
    }

    static func drawConfirmation(into buffer: inout ScreenBuffer, confirmation: Confirmation,
                                 context: RenderContext) -> (confirm: Rect?, cancel: Rect?) {
        let theme = context.theme
        let width = min(buffer.width - 4, 64)
        let messageLines = TextWidth.wrap(confirmation.operation.confirmationMessage, width: max(10, width - 4))
        let height = min(buffer.height - 2, messageLines.count + 6)
        let rect = buffer.bounds.centered(width: width, height: height)
        let dialog = Style(foreground: theme[.foreground], background: theme[.dialogBackground])
        buffer.fill(rect, style: dialog)
        let inner = Box.draw(into: &buffer, rect: rect, title: confirmation.operation.confirmationTitle,
                             borderStyle: dialog.foreground(theme[.warning]),
                             titleStyle: dialog.foreground(theme[.warning]).bold, characters: theme.border)
        guard inner.height >= 3 else { return (nil, nil) }
        for (offset, line) in messageLines.enumerated() where inner.y + 1 + offset < inner.maxY - 2 {
            buffer.write(line, x: inner.x + 1, y: inner.y + 1 + offset, style: dialog)
        }
        let yes = "  Yes (y)  "
        let no = "  No (n)  "
        let buttonsWidth = TextWidth.of(yes) + TextWidth.of(no) + 3
        let y = inner.maxY - 1
        let start = inner.x + max(0, (inner.width - buttonsWidth) / 2)
        let focused = theme.selection.adding(.bold)
        let unfocused = dialog.foreground(theme[.muted])
        let yesStyle = confirmation.confirmFocused ? focused.background(theme[.error]) : unfocused
        let noStyle = confirmation.confirmFocused ? unfocused : focused
        buffer.write(yes, x: start, y: y, style: yesStyle)
        let noX = start + TextWidth.of(yes) + 3
        buffer.write(no, x: noX, y: y, style: noStyle)
        return (Rect(x: start, y: y, width: TextWidth.of(yes), height: 1),
                Rect(x: noX, y: y, width: TextWidth.of(no), height: 1))
    }
}
