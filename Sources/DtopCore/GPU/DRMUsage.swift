/// One DRM client as described by `/proc/<pid>/fdinfo/<fd>`, following the kernel's
/// "DRM client usage stats" specification (Documentation/gpu/drm-usage-stats.rst).
/// amdgpu and i915 report `drm-engine-<name>` busy nanoseconds; xe reports
/// `drm-cycles-<name>` / `drm-total-cycles-<name>`.
public struct DRMClient: Sendable, Equatable {
    public var driver: String
    public var clientID: String
    public var pdev: String
    public var engineNanoseconds: [String: UInt64]
    public var engineCapacity: [String: UInt64]
    public var cycles: [String: UInt64]
    public var totalCycles: [String: UInt64]
    /// Bytes resident per memory region (`drm-resident-*`, or amdgpu's legacy `drm-memory-*`).
    public var residentMemory: [String: UInt64]

    /// Clients are unique per device: the spec allows `drm-client-id` to be per-device, so the
    /// PCI address is part of the key, and several fds of one client must not be double counted.
    public var key: String { pdev + "#" + clientID }

    /// Memory in device-local regions (VRAM / Intel "local"), which is what "GPU memory" means
    /// on a discrete card.
    public var deviceMemory: UInt64 {
        residentMemory.filter { region, _ in region.hasPrefix("vram") || region.hasPrefix("local") }
            .values.reduce(0, &+)
    }

    public static func parse(_ text: String) -> DRMClient? {
        var fields: [String: String] = [:]
        for line in text.split(separator: "\n") {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let key = String(line[..<colon]).trimmingWhitespace()
            guard key.hasPrefix("drm-") else { continue }
            fields[key] = String(line[line.index(after: colon)...]).trimmingWhitespace()
        }
        guard let driver = fields["drm-driver"], let clientID = fields["drm-client-id"] else { return nil }
        var client = DRMClient(driver: driver, clientID: clientID, pdev: fields["drm-pdev"] ?? "",
                               engineNanoseconds: [:], engineCapacity: [:], cycles: [:], totalCycles: [:],
                               residentMemory: [:])
        var legacyMemory: [String: UInt64] = [:]
        for (key, value) in fields {
            if let engine = key.dropPrefix("drm-engine-capacity-") {
                client.engineCapacity[engine] = UInt64(value)
            } else if let engine = key.dropPrefix("drm-engine-") {
                client.engineNanoseconds[engine] = parseNanoseconds(value)
            } else if let engine = key.dropPrefix("drm-total-cycles-") {
                client.totalCycles[engine] = UInt64(value)
            } else if let engine = key.dropPrefix("drm-cycles-") {
                client.cycles[engine] = UInt64(value)
            } else if let region = key.dropPrefix("drm-resident-") {
                client.residentMemory[region] = parseBytes(value)
            } else if let region = key.dropPrefix("drm-memory-") {
                legacyMemory[region] = parseBytes(value)
            }
        }
        // drm-memory-<region> is a deprecated amdgpu alias for drm-resident-<region>.
        for (region, bytes) in legacyMemory where client.residentMemory[region] == nil {
            client.residentMemory[region] = bytes
        }
        return client
    }

    static func parseNanoseconds(_ value: String) -> UInt64? {
        let number = value.split(separator: " ").first.map(String.init) ?? value
        return UInt64(number)
    }

    /// `23992 KiB`, `16 MiB` or plain bytes.
    static func parseBytes(_ value: String) -> UInt64? {
        let parts = value.split(separator: " ")
        guard let number = parts.first.flatMap({ UInt64($0) }) else { return nil }
        switch parts.count > 1 ? String(parts[1]) : "" {
        case "KiB": return number * 1024
        case "MiB": return number * 1_048_576
        case "GiB": return number * 1_073_741_824
        default: return number
        }
    }
}

/// Per-engine utilization (0-100) of one DRM client over the last polling interval.
public struct DRMClientUsage: Sendable, Equatable {
    public var engines: [String: Double]

    public init(engines: [String: Double]) {
        self.engines = engines
    }

    /// The busiest engine, which is what "GPU %" means for a single process.
    public var busiest: Double { engines.values.max() ?? 0 }
}

/// Turns cumulative DRM client counters into utilization percentages between two polls.
public struct DRMUsageTracker: Sendable {
    struct Previous: Sendable {
        var timestamp: Double
        var engineNanoseconds: [String: UInt64]
        var cycles: [String: UInt64]
        var totalCycles: [String: UInt64]
    }

    private var previous: [String: Previous] = [:]

    public init() {}

    /// Usage per client key since the previous call. Clients seen for the first time have no
    /// baseline and are absent from the result. Per the spec, a counter that goes backwards keeps
    /// its previous larger value until it catches up. Clients that disappear are forgotten.
    public mutating func update(_ clients: [DRMClient], now: Double) -> [String: DRMClientUsage] {
        var result: [String: DRMClientUsage] = [:]
        var next: [String: Previous] = [:]
        for client in clients where next[client.key] == nil {
            let old = previous[client.key]
            var current = Previous(timestamp: now, engineNanoseconds: client.engineNanoseconds,
                                   cycles: client.cycles, totalCycles: client.totalCycles)
            if let old {
                for (engine, value) in current.engineNanoseconds where value < (old.engineNanoseconds[engine] ?? 0) {
                    current.engineNanoseconds[engine] = old.engineNanoseconds[engine]
                }
                for (engine, value) in current.cycles where value < (old.cycles[engine] ?? 0) {
                    current.cycles[engine] = old.cycles[engine]
                }
            }
            next[client.key] = current
            guard let old else { continue }

            var engines: [String: Double] = [:]
            let elapsedNanoseconds = (now - old.timestamp) * 1_000_000_000
            if elapsedNanoseconds > 0 {
                for (engine, value) in current.engineNanoseconds {
                    let delta = Double(value - min(value, old.engineNanoseconds[engine] ?? value))
                    let capacity = Double(max(1, client.engineCapacity[engine] ?? 1))
                    engines[engine] = delta / (elapsedNanoseconds * capacity) * 100
                }
            }
            for (engine, value) in current.cycles {
                guard let total = current.totalCycles[engine], let oldTotal = old.totalCycles[engine], total > oldTotal else {
                    continue
                }
                let delta = Double(value - min(value, old.cycles[engine] ?? value))
                let capacity = Double(max(1, client.engineCapacity[engine] ?? 1))
                engines[engine] = max(engines[engine] ?? 0, delta / (Double(total - oldTotal) * capacity) * 100)
            }
            result[client.key] = DRMClientUsage(engines: engines.mapValues { min(100, max(0, $0)) })
        }
        previous = next
        return result
    }

    /// Device utilization: per engine, sum every client's share (capped at 100%), then take the
    /// busiest engine.
    public static func deviceUtilization(_ usages: [DRMClientUsage]) -> Double? {
        guard !usages.isEmpty else { return nil }
        var perEngine: [String: Double] = [:]
        for usage in usages {
            for (engine, value) in usage.engines { perEngine[engine, default: 0] += value }
        }
        return min(100, perEngine.values.max() ?? 0)
    }
}
