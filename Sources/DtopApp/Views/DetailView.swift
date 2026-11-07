import DtopCore
import DtopUI

enum DetailView {
    typealias Line = (key: String, value: String, role: ThemeRole?)

    static func draw(into buffer: inout ScreenBuffer, rect: Rect, item: ListItem?, context: RenderContext) -> [PaneHit] {
        let theme = context.theme
        guard let item else {
            let inner = Box.draw(into: &buffer, rect: rect, title: "details", borderStyle: theme.style(.border),
                                 titleStyle: theme.style(.title), characters: theme.border)
            if !inner.isEmpty {
                buffer.write("Select an item to see details", x: inner.x + 1, y: inner.y, style: theme.style(.muted),
                             maxWidth: inner.width - 1)
            }
            return []
        }

        switch item {
        case .container(let container, _, _):
            return drawContainer(into: &buffer, rect: rect, container: container, context: context)
        case .composeProject(let project, _):
            drawLines(into: &buffer, rect: rect, title: project.name, lines: projectLines(project, context: context),
                      context: context)
        case .image(let image):
            drawLines(into: &buffer, rect: rect, title: image.displayName, lines: imageLines(image, context: context),
                      context: context)
        case .network(let network):
            drawLines(into: &buffer, rect: rect, title: network.name, lines: networkLines(network, context: context),
                      context: context)
        case .volume(let volume):
            drawLines(into: &buffer, rect: rect, title: volume.name, lines: volumeLines(volume), context: context)
        }
        return []
    }

    // MARK: - Containers

    static func drawContainer(into buffer: inout ScreenBuffer, rect: Rect, container: Container,
                              context: RenderContext) -> [PaneHit] {
        let theme = context.theme
        let inner = Box.draw(into: &buffer, rect: rect, title: container.name, borderStyle: theme.style(.border),
                             titleStyle: theme.style(AppView.stateRole(container.state, health: container.health)).bold,
                             characters: theme.border)
        guard inner.height >= 2 else { return [] }

        // Pane selector on the first inner row.
        var hits: [PaneHit] = []
        var x = inner.x
        for pane in DetailPane.allCases {
            let label = " \(pane.title) "
            let width = TextWidth.of(label)
            guard x + width <= inner.maxX else { break }
            let active = pane == context.state.detailPane
            let style = active ? theme.selection.adding(.bold) : theme.style(.muted)
            buffer.write(label, x: x, y: inner.y, style: style)
            hits.append(PaneHit(pane: pane, rect: Rect(x: x, y: inner.y, width: width, height: 1)))
            x += width + 1
        }
        let detail = context.snapshot.detail?.containerID == container.id ? context.snapshot.detail : nil
        if context.state.detailPane == .logs, let (text, role) = logStatus(detail, state: context.state) {
            let width = TextWidth.of(text)
            if inner.maxX - width > x {
                buffer.write(text, x: inner.maxX - width, y: inner.y, style: theme.style(role))
            }
        }
        let content = inner.inset(top: 1)
        guard !content.isEmpty else { return hits }

        if let error = detail?.error {
            buffer.write(TextWidth.truncate(error, to: content.width), x: content.x, y: content.y,
                         style: theme.style(.error))
            return hits
        }
        switch context.state.detailPane {
        case .overview:
            drawOverview(into: &buffer, rect: content, container: container, detail: detail, context: context)
        case .processes:
            drawProcesses(into: &buffer, rect: content, container: container, detail: detail, context: context)
        case .logs:
            drawLogs(into: &buffer, rect: content, detail: detail, context: context)
        }
        return hits
    }

    static func containerLines(_ container: Container, details: ContainerDetails?, sample: ResourceSample?) -> [Line] {
        var lines: [Line] = [
            ("id", container.shortID, nil),
            ("image", container.image, nil),
            ("state", container.status.isEmpty ? container.state.rawValue : container.status,
             AppView.stateRole(container.state, health: container.health))
        ]
        if let project = container.composeProject {
            lines.append(("compose", "\(project) / \(container.composeService ?? "?")", .accent))
        }
        if let sample, container.state.isActive {
            lines.append(("cpu", Format.percent(sample.cpuPercent), nil))
            let limit = sample.memoryLimit > 0 ? " / \(Format.bytes(sample.memoryLimit))" : ""
            lines.append(("memory", Format.bytes(sample.memoryUsage) + limit, nil))
            lines.append(("net", "↓\(Format.rate(sample.networkRxRate)) ↑\(Format.rate(sample.networkTxRate))", nil))
            lines.append(("disk", "r \(Format.rate(sample.blockReadRate)) w \(Format.rate(sample.blockWriteRate))", nil))
            lines.append(("pids", String(sample.pids), nil))
            if let gpu = sample.gpuPercent {
                let memory = sample.gpuMemory.map { " · " + Format.bytes($0) } ?? ""
                lines.append(("gpu", Format.percent(gpu) + memory, .gpu))
            }
        }
        if let details {
            if !details.commandLine.isEmpty { lines.append(("command", details.commandLine, nil)) }
            for access in details.gpuAccess { lines.append(("gpu access", access, .gpu)) }
            if details.restartCount > 0 || !details.restartPolicy.isEmpty {
                let policy = details.restartPolicy.isEmpty ? "no" : details.restartPolicy
                lines.append(("restarts", "\(details.restartCount) (policy \(policy))",
                              details.restartCount > 0 ? .warning : nil))
            }
            if details.state.oomKilled { lines.append(("oom", "killed by the OOM killer", .error)) }
            if !details.state.running, details.state.exitCode != 0 {
                lines.append(("exit code", String(details.state.exitCode), .error))
            }
            if !details.state.error.isEmpty { lines.append(("error", details.state.error, .error)) }
            if let health = details.state.health, health != .none {
                lines.append(("health", health.rawValue + (details.state.failingStreak > 0
                    ? " (\(details.state.failingStreak) failing)" : ""), health == .healthy ? .healthy : .unhealthy))
            }
            for network in details.networks {
                lines.append(("network", network.ipAddress.isEmpty ? network.name : "\(network.name) \(network.ipAddress)", nil))
            }
            for (port, bindings) in details.portBindings.sorted(by: { $0.key < $1.key }) where !bindings.isEmpty {
                lines.append(("port", "\(bindings.joined(separator: ", ")) → \(port)", nil))
            }
            for mount in details.mounts {
                let source = mount.type == "volume" ? (mount.name ?? mount.source) : mount.source
                lines.append(("mount", "\(source) → \(mount.destination)\(mount.readWrite ? "" : " (ro)")", nil))
            }
        } else if !container.ports.isEmpty {
            lines.append(("ports", container.portSummary, nil))
        }
        return lines
    }

    static func drawOverview(into buffer: inout ScreenBuffer, rect: Rect, container: Container,
                             detail: ContainerDetailData?, context: RenderContext) {
        let theme = context.theme
        var area = rect
        // Mini CPU/MEM sparklines when there is room.
        if container.state.isActive, let series = context.snapshot.metrics.series[container.id], area.height >= 8 {
            let width = max(1, area.width - 5)
            buffer.write("cpu ", x: area.x, y: area.y, style: theme.style(.muted))
            buffer.write(Sparkline.render(values: series.cpu.elements, width: width), x: area.x + 4, y: area.y,
                         style: theme.style(.cpu))
            buffer.write("mem ", x: area.x, y: area.y + 1, style: theme.style(.muted))
            buffer.write(Sparkline.render(values: series.memory.elements, width: width), x: area.x + 4, y: area.y + 1,
                         style: theme.style(.memory))
            area = area.inset(top: 3)
            if context.snapshot.metrics.latest[container.id]?.gpuPercent != nil, area.height >= 6 {
                buffer.write("gpu ", x: area.x, y: area.y - 1, style: theme.style(.muted))
                buffer.write(Sparkline.render(values: series.gpu.elements, width: width, maximum: 100), x: area.x + 4,
                             y: area.y - 1, style: theme.style(.gpu))
                area = area.inset(top: 1)
            }
        }
        let lines = containerLines(container, details: detail?.details, sample: context.snapshot.metrics.latest[container.id])
        renderKeyValues(into: &buffer, rect: area, lines: lines, scroll: context.state.detailScroll, theme: theme)
    }

    static func drawProcesses(into buffer: inout ScreenBuffer, rect: Rect, container: Container,
                              detail: ContainerDetailData?, context: RenderContext) {
        let theme = context.theme
        guard container.state.isActive else {
            buffer.write("Container is not running", x: rect.x, y: rect.y, style: theme.style(.muted))
            return
        }
        guard let detail, !detail.processes.isEmpty else {
            buffer.write("Loading processes…", x: rect.x, y: rect.y, style: theme.style(.muted))
            return
        }
        buffer.write(TextWidth.fit("PID      CPU  COMMAND", to: rect.width), x: rect.x, y: rect.y,
                     style: theme.style(.tableHeader).bold)
        let body = rect.inset(top: 1)
        let start = min(context.state.detailScroll, max(0, detail.processes.count - body.height))
        for (offset, row) in detail.processes.dropFirst(start).prefix(body.height).enumerated() {
            let y = body.y + offset
            var x = body.x
            x += buffer.write(TextWidth.fit(String(row.process.pid), to: 8), x: x, y: y, style: theme.style(.accent))
            x += buffer.write(TextWidth.fit(row.process.cpu, to: 5), x: x, y: y, style: theme.style(.muted))
            x += buffer.write(row.prefix, x: x, y: y, style: theme.style(.border))
            buffer.write(row.process.command, x: x, y: y, style: theme.base, maxWidth: body.maxX - x)
        }
    }

    /// The stream indicator next to the pane tabs: live, paused (scrolled back) or ended.
    static func logStatus(_ detail: ContainerDetailData?, state: AppState) -> (String, ThemeRole)? {
        guard let detail else { return nil }
        switch detail.logState {
        case .idle:
            return nil
        case .live:
            return state.logFollow ? ("● live ", .success) : ("⏸ paused · f follow ", .warning)
        case .ended(nil):
            return ("■ ended ", .muted)
        case .ended(.some):
            return ("✕ stream lost ", .error)
        }
    }

    static func drawLogs(into buffer: inout ScreenBuffer, rect: Rect, detail: ContainerDetailData?, context: RenderContext) {
        let theme = context.theme
        guard let detail else {
            buffer.write("Loading logs…", x: rect.x, y: rect.y, style: theme.style(.muted))
            return
        }
        if case .ended(let error?) = detail.logState {
            // Keep what was received, and say why it stopped on the last line.
            let note = "log stream lost: \(error) — reconnecting if the container is running"
            buffer.write(TextWidth.truncate(note, to: rect.width), x: rect.x, y: rect.maxY - 1, style: theme.style(.error))
            return drawLogLines(into: &buffer, rect: rect.inset(bottom: 1), detail: detail, context: context)
        }
        guard !detail.logs.isEmpty else {
            let message = detail.logState == .live ? "Waiting for log output…" : "No log output"
            buffer.write(message, x: rect.x, y: rect.y, style: theme.style(.muted))
            return
        }
        drawLogLines(into: &buffer, rect: rect, detail: detail, context: context)
    }

    static func drawLogLines(into buffer: inout ScreenBuffer, rect: Rect, detail: ContainerDetailData, context: RenderContext) {
        let theme = context.theme
        guard rect.height > 0 else { return }
        // Newest at the bottom; scrolling moves back in time.
        let scroll = min(context.state.detailScroll, max(0, detail.logs.count - rect.height))
        let end = detail.logs.count - scroll
        let start = max(0, end - rect.height)
        for (offset, line) in detail.logs[start..<end].enumerated() {
            let style = line.stream == .stderr ? theme.style(.warning) : theme.base
            buffer.write(line.text, x: rect.x, y: rect.y + offset, style: style, maxWidth: rect.width)
        }
    }

    // MARK: - Other resources

    static func projectLines(_ project: ComposeProject, context: RenderContext) -> [Line] {
        var lines: [Line] = [
            ("status", "\(project.aggregateStatus.rawValue) · \(project.runningCount)/\(project.containers.count) running",
             AppView.composeRole(project.aggregateStatus))
        ]
        if let directory = project.workingDirectory { lines.append(("directory", directory, nil)) }
        for file in project.configFiles { lines.append(("file", file, nil)) }
        for service in project.services {
            lines.append(("service", "\(service.name) \(service.runningCount)/\(service.containers.count)",
                          AppView.composeRole(service.aggregateStatus)))
        }
        lines.append(("", "", nil))
        lines.append(("keys", "u start · U up · x stop · r restart · p pause · D down", .muted))
        return lines
    }

    static func imageLines(_ image: DockerImage, context: RenderContext) -> [Line] {
        var lines: [Line] = [("id", image.shortID, nil), ("size", Format.bytes(image.size), nil)]
        if image.created > 0 { lines.append(("created", Format.age(since: Double(image.created), now: context.now), nil)) }
        for tag in image.repoTags { lines.append(("tag", tag, nil)) }
        for digest in image.repoDigests { lines.append(("digest", digest, .muted)) }
        let users = context.snapshot.containers.filter { $0.imageID == image.id }
        for container in users {
            lines.append(("used by", container.name, AppView.stateRole(container.state, health: container.health)))
        }
        if users.isEmpty, image.isDangling { lines.append(("note", "dangling — removed by prune (X)", .warning)) }
        return lines
    }

    static func networkLines(_ network: DockerNetwork, context: RenderContext) -> [Line] {
        var lines: [Line] = [
            ("id", network.shortID, nil), ("driver", network.driver, nil), ("scope", network.scope, nil)
        ]
        if network.isInternal { lines.append(("internal", "yes", .warning)) }
        if network.enableIPv6 { lines.append(("ipv6", "enabled", nil)) }
        for config in network.ipam {
            lines.append(("subnet", config.gateway.isEmpty ? config.subnet : "\(config.subnet) via \(config.gateway)", nil))
        }
        for id in network.containerIDs {
            let name = context.snapshot.container(id: id)?.name ?? String(id.prefix(12))
            lines.append(("container", name, nil))
        }
        if network.isBuiltIn { lines.append(("note", "built-in network (cannot be removed)", .muted)) }
        return lines
    }

    static func volumeLines(_ volume: DockerVolume) -> [Line] {
        var lines: [Line] = [("driver", volume.driver, nil), ("scope", volume.scope, nil)]
        if !volume.mountpoint.isEmpty { lines.append(("mountpoint", volume.mountpoint, nil)) }
        if !volume.createdAt.isEmpty { lines.append(("created", volume.createdAt, nil)) }
        if let project = volume.composeProject { lines.append(("compose", project, .accent)) }
        if let size = volume.sizeBytes { lines.append(("size", Format.bytes(size), nil)) }
        for (key, value) in volume.labels.sorted(by: { $0.key < $1.key }) where !key.hasPrefix("com.docker.compose") {
            lines.append(("label", "\(key)=\(value)", .muted))
        }
        return lines
    }

    static func drawLines(into buffer: inout ScreenBuffer, rect: Rect, title: String, lines: [Line], context: RenderContext) {
        let theme = context.theme
        let inner = Box.draw(into: &buffer, rect: rect, title: title, borderStyle: theme.style(.border),
                             titleStyle: theme.style(.title).bold, characters: theme.border)
        renderKeyValues(into: &buffer, rect: inner, lines: lines, scroll: context.state.detailScroll, theme: theme)
    }

    static func renderKeyValues(into buffer: inout ScreenBuffer, rect: Rect, lines: [Line], scroll: Int, theme: Theme) {
        guard !rect.isEmpty else { return }
        let keyWidth = min(11, max(4, (lines.map { TextWidth.of($0.key) }.max() ?? 4) + 1))
        let start = min(scroll, max(0, lines.count - rect.height))
        for (offset, line) in lines.dropFirst(start).prefix(rect.height).enumerated() {
            let y = rect.y + offset
            buffer.write(TextWidth.fit(line.key, to: keyWidth), x: rect.x, y: y, style: theme.style(.muted))
            buffer.write(line.value, x: rect.x + keyWidth, y: y, style: line.role.map { theme.style($0) } ?? theme.base,
                         maxWidth: rect.width - keyWidth)
        }
    }
}
