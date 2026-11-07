import DtopCore
import DtopUI

enum ListView {
    static func draw(into buffer: inout ScreenBuffer, rect: Rect, items: [ListItem], selected: Int?,
                     context: RenderContext) -> TableLayout {
        let theme = context.theme
        let tab = context.state.tab
        let count = tab == .compose ? items.filter { $0.composeProject != nil }.count : items.count
        let title = "\(tab.title.lowercased()) \(count)"
        let focused = context.state.mode == .normal
        let inner = Box.draw(into: &buffer, rect: rect, title: title, label: nil,
                             borderStyle: theme.style(focused ? .borderFocused : .border),
                             titleStyle: theme.style(.title).bold, characters: theme.border)
        guard !inner.isEmpty else {
            return TableLayout(headerRect: .zero, rowRects: [], columnWidths: [], scrollOffset: 0)
        }

        if items.isEmpty {
            drawEmptyState(into: &buffer, rect: inner, context: context)
            return TableLayout(headerRect: .zero, rowRects: [], columnWidths: [], scrollOffset: 0)
        }

        let showGPU = !context.snapshot.metrics.gpu.devices.isEmpty
        let columns = Self.columns(for: tab, showGPU: showGPU)
        let rows = items.map { row(for: $0, context: context, showGPU: showGPU) }
        let styles = TableStyles(
            header: theme.style(.tableHeader).bold,
            row: theme.base,
            selected: theme.selection.adding(.bold),
            scrollIndicator: theme.style(.muted)
        )
        return Table.draw(into: &buffer, rect: inner, columns: columns, rows: rows, selected: selected,
                          scrollOffset: context.state.scrollOffsets[tab] ?? 0, styles: styles)
    }

    static func drawEmptyState(into buffer: inout ScreenBuffer, rect: Rect, context: RenderContext) {
        let theme = context.theme
        var lines: [String]
        var role = ThemeRole.muted
        switch context.snapshot.connection {
        case .connecting:
            lines = ["Connecting to Docker at \(context.snapshot.endpoint)…"]
        case .disconnected(let message, let retryAt):
            role = .error
            lines = TextWidth.wrap(message, width: max(10, rect.width - 4))
            if let retryAt {
                lines.append("")
                lines.append("Retrying in \(max(0, Int((retryAt - context.now).rounded(.up))))s — press R to retry now")
            }
        case .connected:
            if !context.state.filterText.isEmpty {
                lines = ["Nothing matches \"\(context.state.filterText)\" — press Esc to clear the filter"]
            } else {
                switch context.state.tab {
                case .containers:
                    lines = context.state.statusFilter == .all
                        ? ["No containers yet. Try: docker run -d nginx"]
                        : ["No \(context.state.statusFilter.rawValue) containers — press a to change"]
                case .compose: lines = ["No Compose projects. Start one with: docker compose up -d"]
                case .images: lines = ["No images"]
                case .networks: lines = ["No networks"]
                case .volumes: lines = ["No volumes"]
                }
            }
        }
        let top = rect.y + max(0, (rect.height - lines.count) / 2)
        for (offset, line) in lines.enumerated() where top + offset < rect.maxY {
            let text = TextWidth.truncate(line, to: rect.width)
            let x = rect.x + max(0, (rect.width - TextWidth.of(text)) / 2)
            buffer.write(text, x: x, y: top + offset, style: theme.style(role))
        }
    }

    static func columns(for tab: Tab, showGPU: Bool = false) -> [TableColumn] {
        switch tab {
        case .containers, .compose:
            return Self.withGPU(showGPU, [
                TableColumn("", width: .fixed(1)),
                TableColumn("NAME", width: .flexible(min: 14, weight: 3)),
                TableColumn("STATE", width: .fixed(10), priority: 1),
                TableColumn("CPU%", width: .fixed(6), alignment: .right),
                TableColumn("MEM", width: .fixed(7), alignment: .right),
                TableColumn("MEM%", width: .fixed(5), alignment: .right, priority: 3),
                TableColumn("NET ↓↑/s", width: .fixed(11), alignment: .right, priority: 4),
                TableColumn(tab == .compose ? "SERVICE" : "IMAGE", width: .flexible(min: 12, weight: 2), priority: 2),
                TableColumn("PORTS", width: .flexible(min: 12, weight: 1), priority: 6),
                TableColumn("STATUS", width: .flexible(min: 14, weight: 2), priority: 5)
            ], gpuColumn: TableColumn("GPU%", width: .fixed(5), alignment: .right, priority: 3))
        case .images:
            return [
                TableColumn("REPOSITORY:TAG", width: .flexible(min: 20, weight: 3)),
                TableColumn("ID", width: .fixed(12), priority: 2),
                TableColumn("SIZE", width: .fixed(8), alignment: .right),
                TableColumn("CREATED", width: .fixed(10), alignment: .right, priority: 1),
                TableColumn("USED", width: .fixed(4), alignment: .right, priority: 3)
            ]
        case .networks:
            return [
                TableColumn("NAME", width: .flexible(min: 16, weight: 2)),
                TableColumn("ID", width: .fixed(12), priority: 3),
                TableColumn("DRIVER", width: .fixed(8)),
                TableColumn("SCOPE", width: .fixed(6), priority: 4),
                TableColumn("SUBNET", width: .flexible(min: 18, weight: 1), priority: 1),
                TableColumn("CTRS", width: .fixed(4), alignment: .right, priority: 2)
            ]
        case .volumes:
            return [
                TableColumn("NAME", width: .flexible(min: 18, weight: 3)),
                TableColumn("DRIVER", width: .fixed(8), priority: 2),
                TableColumn("PROJECT", width: .flexible(min: 10, weight: 1), priority: 1),
                TableColumn("MOUNTPOINT", width: .flexible(min: 20, weight: 2), priority: 3)
            ]
        }
    }

    /// Inserts the GPU column after MEM% (index 6) only when the host has a GPU, so machines
    /// without one keep the narrower table.
    static func withGPU<T>(_ show: Bool, _ items: [T], gpuColumn: T) -> [T] {
        guard show else { return items }
        var result = items
        result.insert(gpuColumn, at: min(6, result.count))
        return result
    }

    static func gpuCell(_ sample: ResourceSample?, active: Bool, theme: Theme) -> TableCell {
        guard active, let percent = sample?.gpuPercent else { return TableCell("-", style: theme.style(.muted)) }
        return TableCell(percentCell(percent), style: Style(foreground: theme.gradient(percent / 100)))
    }

    static func row(for item: ListItem, context: RenderContext, showGPU: Bool = false) -> [TableCell] {
        let theme = context.theme
        let metrics = context.snapshot.metrics
        switch item {
        case .container(let container, let depth, let isLast):
            let stateStyle = theme.style(AppView.stateRole(container.state, health: container.health))
            let sample = metrics.latest[container.id]
            let active = container.state.isActive && sample != nil
            let name = depth > 0 ? (isLast ? "└─ " : "├─ ") + container.name : container.name
            let cpuFraction = (sample?.normalizedCPUPercent ?? 0) / 100
            let memoryFraction = (sample?.memoryPercent ?? 0) / 100
            let stateText: String
            switch container.health {
            case .healthy?: stateText = container.state.rawValue + " ✓"
            case .unhealthy?: stateText = "unhealthy"
            case .starting?: stateText = "starting"
            default: stateText = container.state.rawValue
            }
            return Self.withGPU(showGPU, [
                TableCell(AppView.stateSymbol(container.state), style: stateStyle),
                TableCell(name, style: depth > 0 ? nil : Style(attributes: .bold)),
                TableCell(stateText, style: stateStyle),
                TableCell(active ? Format.fixed(sample!.cpuPercent, decimals: 1) : "-",
                          style: active ? Style(foreground: theme.gradient(cpuFraction)) : theme.style(.muted)),
                TableCell(active ? Format.bytes(sample!.memoryUsage) : "-", style: active ? nil : theme.style(.muted)),
                TableCell(active && sample!.memoryLimit > 0 ? percentCell(sample!.memoryPercent) : "-",
                          style: active ? Style(foreground: theme.gradient(memoryFraction)) : theme.style(.muted)),
                TableCell(active ? networkCell(sample!.networkRxRate, sample!.networkTxRate) : "-",
                          style: theme.style(.muted)),
                TableCell(depth > 0 ? (container.composeService ?? container.image) : container.image,
                          style: theme.style(.muted)),
                TableCell(container.portSummary, style: theme.style(.muted)),
                TableCell(container.status, style: theme.style(.muted))
            ], gpuColumn: gpuCell(sample, active: active, theme: theme))

        case .composeProject(let project, let collapsed):
            let role = AppView.composeRole(project.aggregateStatus)
            var cpu = 0.0
            var memory: UInt64 = 0
            var rx = 0.0
            var tx = 0.0
            var gpu: Double?
            for container in project.containers {
                guard let sample = metrics.latest[container.id] else { continue }
                cpu += sample.cpuPercent
                memory &+= sample.memoryUsage
                rx += sample.networkRxRate
                tx += sample.networkTxRate
                if let share = sample.gpuPercent { gpu = (gpu ?? 0) + share }
            }
            let services = project.services.map(\.name).joined(separator: ", ")
            return Self.withGPU(showGPU, [
                TableCell(collapsed ? "▸" : "▾", style: theme.style(.accent)),
                TableCell(project.name, style: theme.style(.accent).bold),
                TableCell("\(project.runningCount)/\(project.containers.count) up", style: theme.style(role)),
                TableCell(Format.fixed(cpu, decimals: 1)),
                TableCell(Format.bytes(memory)),
                TableCell(""),
                TableCell(networkCell(rx, tx), style: theme.style(.muted)),
                TableCell(services, style: theme.style(.muted)),
                TableCell(""),
                TableCell(project.aggregateStatus.rawValue, style: theme.style(role))
            ], gpuColumn: TableCell(gpu.map { percentCell(min(100, $0)) } ?? "", style: theme.style(.gpu)))

        case .image(let image):
            return [
                TableCell(image.displayName, style: image.isDangling ? theme.style(.muted) : nil),
                TableCell(image.shortID, style: theme.style(.muted)),
                TableCell(Format.bytes(image.size)),
                TableCell(Format.age(since: Double(image.created), now: context.now), style: theme.style(.muted)),
                TableCell(image.containers >= 0 ? String(image.containers) : "-", style: theme.style(.muted))
            ]

        case .network(let network):
            let subnets = network.ipam.map(\.subnet).filter { !$0.isEmpty }.joined(separator: ", ")
            return [
                TableCell(network.name, style: network.isBuiltIn ? theme.style(.muted) : nil),
                TableCell(network.shortID, style: theme.style(.muted)),
                TableCell(network.driver),
                TableCell(network.scope, style: theme.style(.muted)),
                TableCell(subnets, style: theme.style(.muted)),
                TableCell(String(network.containerIDs.count))
            ]

        case .volume(let volume):
            return [
                TableCell(volume.isAnonymous ? String(volume.name.prefix(12)) + "… (anonymous)" : volume.name,
                          style: volume.isAnonymous ? theme.style(.muted) : nil),
                TableCell(volume.driver, style: theme.style(.muted)),
                TableCell(volume.composeProject ?? "", style: theme.style(.accent)),
                TableCell(volume.mountpoint, style: theme.style(.muted))
            ]
        }
    }

    static func networkCell(_ rx: Double, _ tx: Double) -> String {
        "\(Format.compactBytes(rx)) \(Format.compactBytes(tx))"
    }

    /// One decimal below 10% so small-but-nonzero usage does not read as 0.
    static func percentCell(_ value: Double) -> String {
        Format.fixed(value, decimals: value < 10 ? 1 : 0)
    }
}
