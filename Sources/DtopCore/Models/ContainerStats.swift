/// Raw payload from `GET /containers/{id}/stats`. Field names follow the API (snake_case).
/// Linux daemons populate cgroup counters; Windows daemons populate the 100ns-tick fields.
public struct ContainerStats: Sendable, Equatable, Decodable {
    public struct CPUUsage: Sendable, Equatable {
        public var totalUsage: UInt64
        public var perCPUUsage: [UInt64]
        public var kernelMode: UInt64
        public var userMode: UInt64

        public init(totalUsage: UInt64, perCPUUsage: [UInt64] = [], kernelMode: UInt64 = 0, userMode: UInt64 = 0) {
            self.totalUsage = totalUsage
            self.perCPUUsage = perCPUUsage
            self.kernelMode = kernelMode
            self.userMode = userMode
        }
    }

    public struct CPUStats: Sendable, Equatable {
        public var usage: CPUUsage
        public var systemUsage: UInt64?
        public var onlineCPUs: Int?

        public init(usage: CPUUsage, systemUsage: UInt64? = nil, onlineCPUs: Int? = nil) {
            self.usage = usage
            self.systemUsage = systemUsage
            self.onlineCPUs = onlineCPUs
        }

        /// CPUs to scale by: `online_cpus`, else the per-CPU array length (older kernels), else 1.
        public var effectiveCPUCount: Int {
            if let onlineCPUs, onlineCPUs > 0 { return onlineCPUs }
            let fromArray = usage.perCPUUsage.filter { $0 > 0 }.count
            return max(1, fromArray)
        }
    }

    public struct MemoryStats: Sendable, Equatable {
        public var usage: UInt64?
        public var limit: UInt64?
        public var maxUsage: UInt64?
        public var stats: [String: UInt64]
        /// Windows: private working set.
        public var privateWorkingSet: UInt64?
        public var commitBytes: UInt64?

        public init(
            usage: UInt64? = nil,
            limit: UInt64? = nil,
            maxUsage: UInt64? = nil,
            stats: [String: UInt64] = [:],
            privateWorkingSet: UInt64? = nil,
            commitBytes: UInt64? = nil
        ) {
            self.usage = usage
            self.limit = limit
            self.maxUsage = maxUsage
            self.stats = stats
            self.privateWorkingSet = privateWorkingSet
            self.commitBytes = commitBytes
        }
    }

    public struct NetworkStats: Sendable, Equatable {
        public var rxBytes: UInt64
        public var txBytes: UInt64
        public var rxPackets: UInt64
        public var txPackets: UInt64
        public var rxErrors: UInt64
        public var txErrors: UInt64

        public init(
            rxBytes: UInt64,
            txBytes: UInt64,
            rxPackets: UInt64 = 0,
            txPackets: UInt64 = 0,
            rxErrors: UInt64 = 0,
            txErrors: UInt64 = 0
        ) {
            self.rxBytes = rxBytes
            self.txBytes = txBytes
            self.rxPackets = rxPackets
            self.txPackets = txPackets
            self.rxErrors = rxErrors
            self.txErrors = txErrors
        }
    }

    public struct BlockIOEntry: Sendable, Equatable {
        public var major: UInt64
        public var minor: UInt64
        public var op: String
        public var value: UInt64

        public init(major: UInt64 = 0, minor: UInt64 = 0, op: String, value: UInt64) {
            self.major = major
            self.minor = minor
            self.op = op
            self.value = value
        }
    }

    public var read: String
    public var preread: String
    public var numProcs: Int
    public var cpu: CPUStats
    public var precpu: CPUStats
    public var memory: MemoryStats
    public var networks: [String: NetworkStats]
    public var blockIO: [BlockIOEntry]
    public var pidsCurrent: UInt64?
    public var pidsLimit: UInt64?
    /// Windows storage counters.
    public var storageReadBytes: UInt64?
    public var storageWriteBytes: UInt64?

    public init(
        read: String = "",
        preread: String = "",
        numProcs: Int = 0,
        cpu: CPUStats,
        precpu: CPUStats = CPUStats(usage: CPUUsage(totalUsage: 0)),
        memory: MemoryStats = MemoryStats(),
        networks: [String: NetworkStats] = [:],
        blockIO: [BlockIOEntry] = [],
        pidsCurrent: UInt64? = nil,
        pidsLimit: UInt64? = nil,
        storageReadBytes: UInt64? = nil,
        storageWriteBytes: UInt64? = nil
    ) {
        self.read = read
        self.preread = preread
        self.numProcs = numProcs
        self.cpu = cpu
        self.precpu = precpu
        self.memory = memory
        self.networks = networks
        self.blockIO = blockIO
        self.pidsCurrent = pidsCurrent
        self.pidsLimit = pidsLimit
        self.storageReadBytes = storageReadBytes
        self.storageWriteBytes = storageWriteBytes
    }

    enum CodingKeys: String, CodingKey {
        case read, preread, numProcs = "num_procs", cpuStats = "cpu_stats", precpuStats = "precpu_stats"
        case memoryStats = "memory_stats", networks, blkioStats = "blkio_stats", pidsStats = "pids_stats"
        case storageStats = "storage_stats"
    }

    enum CPUStatsKeys: String, CodingKey {
        case cpuUsage = "cpu_usage", systemCPUUsage = "system_cpu_usage", onlineCPUs = "online_cpus"
    }

    enum CPUUsageKeys: String, CodingKey {
        case totalUsage = "total_usage", perCPUUsage = "percpu_usage"
        case kernelMode = "usage_in_kernelmode", userMode = "usage_in_usermode"
    }

    enum MemoryKeys: String, CodingKey {
        case usage, limit, maxUsage = "max_usage", stats, privateWorkingSet = "privateworkingset"
        case commitBytes = "commitbytes"
    }

    enum NetworkKeys: String, CodingKey {
        case rxBytes = "rx_bytes", txBytes = "tx_bytes", rxPackets = "rx_packets", txPackets = "tx_packets"
        case rxErrors = "rx_errors", txErrors = "tx_errors"
    }

    enum BlkioKeys: String, CodingKey {
        case ioServiceBytesRecursive = "io_service_bytes_recursive"
    }

    enum BlkioEntryKeys: String, CodingKey {
        case major, minor, op, value
    }

    enum PidsKeys: String, CodingKey {
        case current, limit
    }

    enum StorageKeys: String, CodingKey {
        case readSizeBytes = "read_size_bytes", writeSizeBytes = "write_size_bytes"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        read = c.lenient(.read, default: "")
        preread = c.lenient(.preread, default: "")
        numProcs = Int(c.flexibleInt(.numProcs) ?? 0)
        cpu = Self.decodeCPU(c, key: .cpuStats)
        precpu = Self.decodeCPU(c, key: .precpuStats)

        if let m = try? c.nestedContainer(keyedBy: MemoryKeys.self, forKey: .memoryStats) {
            var stats: [String: UInt64] = [:]
            if let statsContainer = try? m.nestedContainer(keyedBy: AnyCodingKey.self, forKey: .stats) {
                for key in statsContainer.allKeys {
                    if let value = statsContainer.flexibleUInt(key) { stats[key.stringValue] = value }
                }
            }
            memory = MemoryStats(
                usage: m.flexibleUInt(.usage),
                limit: m.flexibleUInt(.limit),
                maxUsage: m.flexibleUInt(.maxUsage),
                stats: stats,
                privateWorkingSet: m.flexibleUInt(.privateWorkingSet),
                commitBytes: m.flexibleUInt(.commitBytes)
            )
        } else {
            memory = MemoryStats()
        }

        var networks: [String: NetworkStats] = [:]
        if let n = try? c.nestedContainer(keyedBy: AnyCodingKey.self, forKey: .networks) {
            for key in n.allKeys {
                guard let iface = try? n.nestedContainer(keyedBy: NetworkKeys.self, forKey: key) else { continue }
                networks[key.stringValue] = NetworkStats(
                    rxBytes: iface.flexibleUInt(.rxBytes) ?? 0,
                    txBytes: iface.flexibleUInt(.txBytes) ?? 0,
                    rxPackets: iface.flexibleUInt(.rxPackets) ?? 0,
                    txPackets: iface.flexibleUInt(.txPackets) ?? 0,
                    rxErrors: iface.flexibleUInt(.rxErrors) ?? 0,
                    txErrors: iface.flexibleUInt(.txErrors) ?? 0
                )
            }
        }
        self.networks = networks

        var entries: [BlockIOEntry] = []
        if let b = try? c.nestedContainer(keyedBy: BlkioKeys.self, forKey: .blkioStats),
           var list = try? b.nestedUnkeyedContainer(forKey: .ioServiceBytesRecursive) {
            while !list.isAtEnd {
                guard let entry = try? list.nestedContainer(keyedBy: BlkioEntryKeys.self) else { break }
                entries.append(BlockIOEntry(
                    major: entry.flexibleUInt(.major) ?? 0,
                    minor: entry.flexibleUInt(.minor) ?? 0,
                    op: entry.lenient(.op, default: ""),
                    value: entry.flexibleUInt(.value) ?? 0
                ))
            }
        }
        blockIO = entries

        if let p = try? c.nestedContainer(keyedBy: PidsKeys.self, forKey: .pidsStats) {
            pidsCurrent = p.flexibleUInt(.current)
            pidsLimit = p.flexibleUInt(.limit)
        }
        if let s = try? c.nestedContainer(keyedBy: StorageKeys.self, forKey: .storageStats) {
            storageReadBytes = s.flexibleUInt(.readSizeBytes)
            storageWriteBytes = s.flexibleUInt(.writeSizeBytes)
        }
    }

    private static func decodeCPU(_ c: KeyedDecodingContainer<CodingKeys>, key: CodingKeys) -> CPUStats {
        guard let cpu = try? c.nestedContainer(keyedBy: CPUStatsKeys.self, forKey: key) else {
            return CPUStats(usage: CPUUsage(totalUsage: 0))
        }
        var usage = CPUUsage(totalUsage: 0)
        if let u = try? cpu.nestedContainer(keyedBy: CPUUsageKeys.self, forKey: .cpuUsage) {
            usage = CPUUsage(
                totalUsage: u.flexibleUInt(.totalUsage) ?? 0,
                perCPUUsage: u.lenient(.perCPUUsage, default: []),
                kernelMode: u.flexibleUInt(.kernelMode) ?? 0,
                userMode: u.flexibleUInt(.userMode) ?? 0
            )
        }
        return CPUStats(
            usage: usage,
            systemUsage: cpu.flexibleUInt(.systemCPUUsage),
            onlineCPUs: cpu.flexibleInt(.onlineCPUs).map { Int($0) }
        )
    }

    /// Cumulative network bytes across every interface.
    public var totalNetwork: (rx: UInt64, tx: UInt64) {
        networks.values.reduce((rx: UInt64(0), tx: UInt64(0))) { partial, iface in
            (rx: partial.rx &+ iface.rxBytes, tx: partial.tx &+ iface.txBytes)
        }
    }

    /// Cumulative block I/O. cgroup v1 reports ops capitalized ("Read"), v2 lower-case ("read").
    public var totalBlockIO: (read: UInt64, write: UInt64) {
        if blockIO.isEmpty, storageReadBytes != nil || storageWriteBytes != nil {
            return (read: storageReadBytes ?? 0, write: storageWriteBytes ?? 0)
        }
        var read: UInt64 = 0
        var write: UInt64 = 0
        for entry in blockIO {
            switch entry.op.lowercased() {
            case "read": read &+= entry.value
            case "write": write &+= entry.value
            default: break
            }
        }
        return (read: read, write: write)
    }

    /// Memory in use excluding page cache, matching `docker stats`:
    /// cgroup v2 subtracts `inactive_file`, cgroup v1 subtracts `total_inactive_file`/`cache`.
    public var memoryUsageExcludingCache: UInt64 {
        if let privateWorkingSet = memory.privateWorkingSet, memory.usage == nil {
            return privateWorkingSet
        }
        guard let usage = memory.usage else { return 0 }
        let cache: UInt64
        // cgroup v1 reports both `inactive_file` and the hierarchical `total_inactive_file`;
        // the CLI uses the hierarchical value when it exists.
        if let inactive = memory.stats["total_inactive_file"] {
            cache = inactive
        } else if let inactive = memory.stats["inactive_file"] {
            cache = inactive
        } else if let legacy = memory.stats["cache"] {
            cache = legacy
        } else {
            cache = 0
        }
        return cache < usage ? usage - cache : usage
    }
}
