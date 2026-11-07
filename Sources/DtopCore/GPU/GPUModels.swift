public enum GPUVendor: String, Sendable, Equatable, CaseIterable {
    case nvidia
    case amd
    case intel
    case apple
    case unknown

    public var displayName: String {
        switch self {
        case .nvidia: return "NVIDIA"
        case .amd: return "AMD"
        case .intel: return "Intel"
        case .apple: return "Apple"
        case .unknown: return "GPU"
        }
    }

    /// PCI vendor IDs as found in `/sys/class/drm/cardN/device/vendor`.
    public init(pciVendorID: String) {
        switch pciVendorID.trimmingWhitespace().lowercased() {
        case "0x10de": self = .nvidia
        case "0x1002": self = .amd
        case "0x8086": self = .intel
        default: self = .unknown
        }
    }
}

/// One GPU and its current readings. Readings are optional because each vendor exposes a
/// different subset (Apple has no dedicated VRAM, Intel has no busy-percent counter, …).
public struct GPUDevice: Sendable, Equatable, Identifiable {
    public var id: String
    public var vendor: GPUVendor
    public var name: String
    /// Busy percentage, 0-100.
    public var utilization: Double?
    public var memoryUsed: UInt64?
    public var memoryTotal: UInt64?
    public var temperature: Double?

    public init(
        id: String,
        vendor: GPUVendor,
        name: String,
        utilization: Double? = nil,
        memoryUsed: UInt64? = nil,
        memoryTotal: UInt64? = nil,
        temperature: Double? = nil
    ) {
        self.id = id
        self.vendor = vendor
        self.name = name
        self.utilization = utilization
        self.memoryUsed = memoryUsed
        self.memoryTotal = memoryTotal
        self.temperature = temperature
    }
}

/// GPU usage attributed to one host process.
public struct GPUProcessUsage: Sendable, Equatable {
    public var pid: Int
    public var deviceID: String
    /// Share of the device this process kept busy, 0-100.
    public var utilization: Double?
    public var memoryBytes: UInt64?
    /// Docker container the process belongs to (from its cgroup), when it runs in one.
    public var containerID: String?

    public init(pid: Int, deviceID: String, utilization: Double? = nil, memoryBytes: UInt64? = nil, containerID: String? = nil) {
        self.pid = pid
        self.deviceID = deviceID
        self.utilization = utilization
        self.memoryBytes = memoryBytes
        self.containerID = containerID
    }
}

/// One GPU polling round.
public struct GPUSample: Sendable, Equatable {
    public var devices: [GPUDevice]
    public var processes: [GPUProcessUsage]
    /// True when every GPU process could be inspected, so containers without entries really
    /// use no GPU (rather than "unknown").
    public var processAttributionComplete: Bool
    /// Human-readable limitations, e.g. missing permissions for per-process data.
    public var issues: [String]

    public init(devices: [GPUDevice] = [], processes: [GPUProcessUsage] = [], processAttributionComplete: Bool = false,
                issues: [String] = []) {
        self.devices = devices
        self.processes = processes
        self.processAttributionComplete = processAttributionComplete
        self.issues = issues
    }

    public static let empty = GPUSample()

    /// Mean utilization across devices that report one.
    public var averageUtilization: Double? {
        let values = devices.compactMap(\.utilization)
        guard !values.isEmpty else { return nil }
        return values.reduce(0, +) / Double(values.count)
    }

    public var totalMemoryUsed: UInt64? {
        let values = devices.compactMap(\.memoryUsed)
        return values.isEmpty ? nil : values.reduce(0, &+)
    }

    public var totalMemory: UInt64? {
        let values = devices.compactMap(\.memoryTotal)
        return values.isEmpty ? nil : values.reduce(0, &+)
    }

    /// GPU usage per container ID: utilization summed per device then averaged across all
    /// devices (so one container saturating one of two GPUs reads 50%), memory summed.
    public func usageByContainer() -> [String: (utilization: Double, memory: UInt64)] {
        let deviceCount = Double(max(1, devices.count))
        var result: [String: (utilization: Double, memory: UInt64)] = [:]
        for process in processes {
            guard let container = process.containerID else { continue }
            var entry = result[container] ?? (0, 0)
            entry.utilization += (process.utilization ?? 0) / deviceCount
            entry.memory &+= process.memoryBytes ?? 0
            result[container] = entry
        }
        for (key, value) in result {
            result[key] = (min(100, value.utilization), value.memory)
        }
        return result
    }
}

/// Reads GPU state from the host. Implementations keep state between calls to turn cumulative
/// counters into rates, so callers should keep one instance for the life of the app.
public protocol GPUCollecting: Sendable {
    func sample(now: Double) async -> GPUSample
}

/// Finds the Docker container a host process belongs to from `/proc/<pid>/cgroup`.
/// Handles cgroup v1 (`/docker/<id>`), cgroup v2 with systemd (`docker-<id>.scope`) and other
/// runtimes that embed the 64-hex container ID in the path.
public enum CgroupContainerID {
    public static func parse(_ cgroupFile: String) -> String? {
        for line in cgroupFile.split(separator: "\n") {
            guard let path = line.split(separator: ":", maxSplits: 2).last else { continue }
            if let id = firstContainerID(in: Substring(path)) { return id }
        }
        return nil
    }

    static func firstContainerID(in path: Substring) -> String? {
        var run: [Character] = []
        var best: String?
        for character in path {
            if character.isHexDigit && character.isASCII && !character.isUppercase {
                run.append(character)
                continue
            }
            if run.count == 64 { best = String(run) }
            run.removeAll()
        }
        if run.count == 64 { best = String(run) }
        return best
    }
}
