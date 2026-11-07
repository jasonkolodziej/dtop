import DtopCore
import DtopUI

/// Plain-text snapshot for `dtop --once`: no escape codes, stable column layout, safe to pipe.
public enum OnceReport {
    public static func render(snapshot: DockerSnapshot, width: Int = 120) -> String {
        var lines: [String] = []
        if case .disconnected(let message, _) = snapshot.connection {
            return "dtop: \(message)\n"
        }
        var header = "dtop \(AppProfile.current.version) · \(snapshot.endpoint)"
        if let version = snapshot.version { header += " · docker \(version.version) (api \(version.apiVersion))" }
        if let info = snapshot.info {
            header += " · \(info.cpuCount) cpu · \(Format.bytes(info.memoryTotal))"
        }
        lines.append(header)
        let total = snapshot.metrics.totalLatest
        lines.append("containers: \(snapshot.runningCount) running, \(snapshot.stoppedCount) stopped · "
            + "cpu \(Format.percent(total.cpuPercent)) · mem \(Format.bytes(total.memoryUsage)) · "
            + "net ↓\(Format.rate(total.networkRxRate)) ↑\(Format.rate(total.networkTxRate))")
        let gpu = snapshot.metrics.gpu
        for device in gpu.devices {
            var line = "gpu: \(device.name)"
            if let utilization = device.utilization { line += " · \(Format.percent(utilization))" }
            if let used = device.memoryUsed {
                line += " · \(Format.bytes(used))" + (device.memoryTotal.map { " / \(Format.bytes($0))" } ?? "")
            }
            lines.append(line)
        }
        for issue in gpu.issues { lines.append("gpu: \(issue)") }
        lines.append("")

        let showGPU = !gpu.devices.isEmpty
        var columns: [(String, Int, TextAlignment)] = [
            ("NAME", 28, .left), ("STATE", 10, .left), ("CPU%", 7, .right), ("MEM", 9, .right),
            ("MEM%", 6, .right), ("PROJECT", 16, .left), ("IMAGE", 30, .left)
        ]
        if showGPU { columns.insert(("GPU%", 6, .right), at: 5) }
        func format(_ cells: [String]) -> String {
            let text = zip(columns, cells).map { column, cell in
                TextWidth.fit(cell, to: column.1, alignment: column.2)
            }.joined(separator: " ")
            var trimmed = Substring(TextWidth.prefix(text, width: width))
            while trimmed.last == " " { trimmed = trimmed.dropLast() }
            return String(trimmed)
        }
        lines.append(format(columns.map(\.0)))
        let sorted = ListModel.sortContainers(snapshot.containers, snapshot: snapshot, key: .cpu, descending: true)
        for container in sorted {
            let sample = snapshot.metrics.latest[container.id]
            let active = container.state.isActive && sample != nil
            var cells = [
                container.name,
                container.state.rawValue,
                active ? Format.fixed(sample!.cpuPercent, decimals: 1) : "-",
                active ? Format.bytes(sample!.memoryUsage) : "-",
                active && sample!.memoryLimit > 0 ? Format.fixed(sample!.memoryPercent, decimals: 1) : "-",
                container.composeProject ?? "",
                container.image
            ]
            if showGPU {
                cells.insert(active ? (sample!.gpuPercent.map { Format.fixed($0, decimals: 1) } ?? "-") : "-", at: 5)
            }
            lines.append(format(cells))
        }
        if snapshot.containers.isEmpty { lines.append("(no containers)") }
        return lines.joined(separator: "\n") + "\n"
    }
}
