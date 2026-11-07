import DtopCore
import DtopUI

enum HeaderView {
    static func draw(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let theme = context.theme
        let base = Style(foreground: theme[.headerForeground], background: theme[.headerBackground])
        buffer.fill(rect, style: base)
        let snapshot = context.snapshot

        var x = rect.x
        x += buffer.write(" dtop ", x: x, y: rect.y, style: base.foreground(theme[.accent]).bold)
        var facts: [String] = []
        if let version = snapshot.version {
            facts.append("docker \(version.version)")
        }
        if let info = snapshot.info {
            if !info.name.isEmpty { facts.append(info.name) }
            if info.cpuCount > 0 { facts.append("\(info.cpuCount) cpu") }
            if info.memoryTotal > 0 { facts.append(Format.bytes(info.memoryTotal)) }
        }
        if facts.isEmpty { facts.append(snapshot.endpoint) }

        // Right side: connection, counts, clock.
        var right: [(String, Style)] = []
        switch snapshot.connection {
        case .connected:
            right.append(("● \(snapshot.runningCount) up", base.foreground(theme[.statusRunning])))
            right.append(("◌ \(snapshot.stoppedCount) down", base.foreground(theme[.statusExited])))
        case .connecting:
            right.append(("○ connecting", base.foreground(theme[.warning])))
        case .disconnected(_, let retryAt):
            let wait = retryAt.map { max(0, Int(($0 - context.now).rounded(.up))) }
            right.append((wait.map { "✕ offline · retry \($0)s" } ?? "✕ offline", base.foreground(theme[.error])))
        }
        if context.state.isPaused {
            right.append(("⏸ paused", base.foreground(theme[.warning]).bold))
        }
        if !context.clock.isEmpty {
            right.append((context.clock, base.foreground(theme[.muted])))
        }
        let rightText = right.map(\.0).joined(separator: "  ")
        let rightWidth = TextWidth.of(rightText) + 1
        let available = rect.maxX - x - rightWidth - 1
        if available > 4 {
            buffer.write(TextWidth.truncate(facts.joined(separator: " · "), to: available), x: x, y: rect.y,
                         style: base.foreground(theme[.muted]))
        }
        var rightX = max(x, rect.maxX - rightWidth)
        for (index, part) in right.enumerated() {
            if index > 0 { rightX += buffer.write("  ", x: rightX, y: rect.y, style: base) }
            rightX += buffer.write(part.0, x: rightX, y: rect.y, style: part.1, maxWidth: rect.maxX - rightX)
        }
    }
}

enum MetricsView {
    static func draw(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let wide = rect.width >= 90
        let parts = rect.split(.horizontal, wide ? [.fill(2), .fill(2), .fill(2)] : [.fill(1), .fill(1)])
        drawCPU(into: &buffer, rect: parts[0], context: context)
        drawMemory(into: &buffer, rect: parts[1], context: context)
        if wide {
            drawNetwork(into: &buffer, rect: parts[2], context: context)
        }
    }

    static func hostCPUCount(_ context: RenderContext) -> Int {
        max(1, context.snapshot.info?.cpuCount ?? context.snapshot.metrics.totalLatest.cpuCount)
    }

    static func drawCPU(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let theme = context.theme
        let cpus = hostCPUCount(context)
        let total = context.snapshot.metrics.totalLatest.cpuPercent
        let normalized = total / Double(cpus)
        let inner = Box.draw(into: &buffer, rect: rect, title: "cpu", label: "\(Format.percent(normalized)) of \(cpus) cpu",
                             borderStyle: theme.style(.border), titleStyle: theme.style(.cpu).bold, characters: theme.border)
        let history = context.snapshot.metrics.total.cpu.elements.map { $0 / Double(cpus * 100) }
        BrailleGraph.draw(into: &buffer, rect: inner, values: history, maximum: 1) { fraction in
            Style(foreground: theme.gradient(fraction), background: theme[.background])
        }
    }

    static func drawMemory(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let theme = context.theme
        let used = context.snapshot.metrics.totalLatest.memoryUsage
        let capacity = context.snapshot.info?.memoryTotal ?? 0
        let label = capacity > 0 ? "\(Format.bytes(used)) / \(Format.bytes(capacity))" : Format.bytes(used)
        let inner = Box.draw(into: &buffer, rect: rect, title: "mem", label: label, borderStyle: theme.style(.border),
                             titleStyle: theme.style(.memory).bold, characters: theme.border)
        let history = context.snapshot.metrics.total.memory.elements
        let maximum = capacity > 0 ? Double(capacity) : max(1, history.max() ?? 1)
        BrailleGraph.draw(into: &buffer, rect: inner, values: history, maximum: maximum) { fraction in
            Style(foreground: Color.blend(theme[.memory], theme[.graphHigh], fraction: fraction * 0.6),
                  background: theme[.background])
        }
    }

    static func drawNetwork(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let theme = context.theme
        let latest = context.snapshot.metrics.totalLatest
        let label = "↓\(Format.rate(latest.networkRxRate)) ↑\(Format.rate(latest.networkTxRate))"
        let inner = Box.draw(into: &buffer, rect: rect, title: "net", label: label, borderStyle: theme.style(.border),
                             titleStyle: theme.style(.networkRx).bold, characters: theme.border)
        guard !inner.isEmpty else { return }
        let halves = inner.split(.vertical, [.fill(1), .fill(1)])
        let rx = context.snapshot.metrics.total.networkRx.elements
        let tx = context.snapshot.metrics.total.networkTx.elements
        // Shared scale so the two halves are visually comparable.
        let peak = max(1024, rx.max() ?? 0, tx.max() ?? 0)
        BrailleGraph.draw(into: &buffer, rect: halves[0], values: rx, maximum: peak) { _ in theme.style(.networkRx) }
        BrailleGraph.draw(into: &buffer, rect: halves[1], values: tx, maximum: peak) { _ in theme.style(.networkTx) }
    }
}

enum TabBarView {
    static func draw(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) -> [TabHit] {
        let theme = context.theme
        let snapshot = context.snapshot
        buffer.fill(rect, style: theme.base)
        var hits: [TabHit] = []
        var x = rect.x + 1
        for tab in Tab.allCases {
            let count: Int
            switch tab {
            case .containers: count = snapshot.containers.count
            case .compose: count = ComposeProject.group(snapshot.containers).projects.count
            case .images: count = snapshot.images.count
            case .networks: count = snapshot.networks.count
            case .volumes: count = snapshot.volumes.count
            }
            let label = " \(tab.shortcut) \(tab.title) \(count) "
            let width = TextWidth.of(label)
            guard x + width <= rect.maxX else { break }
            let active = tab == context.state.tab
            var style = active
                ? Style(foreground: theme[.background] == .default ? theme[.selectionForeground] : theme[.background],
                        background: theme[.tabActive], attributes: .bold)
                : theme.style(.tabInactive)
            if active && theme.usesAttributesForEmphasis { style = style.adding(.reverse) }
            buffer.write(label, x: x, y: rect.y, style: style)
            hits.append(TabHit(tab: tab, rect: Rect(x: x, y: rect.y, width: width, height: 1)))
            x += width + 1
        }

        var status: [String] = []
        if context.state.tab == .containers || context.state.tab == .images {
            status.append("sort \(context.state.sortKey.rawValue)\(context.state.sortDescending ? "↓" : "↑")")
        }
        if context.state.tab == .containers || context.state.tab == .compose {
            status.append(context.state.statusFilter.rawValue)
        }
        if !context.state.filterText.isEmpty {
            status.append("filter \"\(context.state.filterText)\"")
        }
        let text = status.joined(separator: " · ")
        let width = TextWidth.of(text)
        if !text.isEmpty, rect.maxX - 1 - width > x {
            buffer.write(text, x: rect.maxX - 1 - width, y: rect.y, style: theme.style(.muted))
        }
        return hits
    }
}

enum FooterView {
    static func hints(for context: RenderContext) -> [(String, String)] {
        let common: [(String, String)] = [("q", "quit"), ("?", "help"), ("/", "filter")]
        switch context.state.tab {
        case .containers:
            return common + [("s", "sort"), ("a", "show"), ("u", "start"), ("x", "stop"), ("r", "restart"),
                             ("p", "pause"), ("K", "kill"), ("D", "remove"), ("v", "pane"), ("t", "theme")]
        case .compose:
            return common + [("⏎", "fold"), ("u", "start"), ("U", "up"), ("x", "stop"), ("r", "restart"),
                             ("D", "down"), ("v", "pane"), ("t", "theme")]
        case .images, .networks, .volumes:
            return common + [("D", "remove"), ("X", "prune"), ("d", "details"), ("t", "theme")]
        }
    }

    static func draw(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let theme = context.theme
        let base = Style(foreground: theme[.headerForeground], background: theme[.headerBackground])
        buffer.fill(rect, style: base)

        if context.state.mode == .filter {
            var x = rect.x + 1
            x += buffer.write("/", x: x, y: rect.y, style: base.foreground(theme[.accent]).bold)
            x += buffer.write(context.state.filterText, x: x, y: rect.y, style: base, maxWidth: rect.maxX - x - 28)
            buffer.write("▏", x: x, y: rect.y, style: base.foreground(theme[.accent]))
            let hint = "⏎ apply · esc clear"
            buffer.write(hint, x: rect.maxX - TextWidth.of(hint) - 1, y: rect.y, style: base.foreground(theme[.muted]))
            return
        }

        if let notice = context.state.notice {
            let role: ThemeRole
            switch notice.level {
            case .info: role = .accent
            case .success: role = .success
            case .warning: role = .warning
            case .error: role = .error
            }
            buffer.write(" " + notice.text, x: rect.x, y: rect.y, style: base.foreground(theme[role]),
                         maxWidth: rect.width)
            return
        }

        if case .disconnected(let message, _) = context.snapshot.connection {
            buffer.write(" " + message, x: rect.x, y: rect.y, style: base.foreground(theme[.error]), maxWidth: rect.width)
            return
        }

        var x = rect.x + 1
        for (key, action) in hints(for: context) {
            let needed = TextWidth.of(key) + TextWidth.of(action) + 3
            guard x + needed <= rect.maxX else { break }
            x += buffer.write(key, x: x, y: rect.y, style: base.foreground(theme[.accent]).bold)
            x += buffer.write(" " + action + "  ", x: x, y: rect.y, style: base.foreground(theme[.muted]))
        }
    }
}
