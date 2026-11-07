/// One computed resource reading for a container.
public struct ResourceSample: Sendable, Equatable {
    public var timestamp: Double
    /// Percent of a single CPU (so 4 busy cores = 400%), like `docker stats`.
    public var cpuPercent: Double
    public var cpuCount: Int
    public var memoryUsage: UInt64
    public var memoryLimit: UInt64
    public var networkRxRate: Double
    public var networkTxRate: Double
    public var blockReadRate: Double
    public var blockWriteRate: Double
    public var networkRxTotal: UInt64
    public var networkTxTotal: UInt64
    public var blockReadTotal: UInt64
    public var blockWriteTotal: UInt64
    public var pids: UInt64

    public init(
        timestamp: Double = 0,
        cpuPercent: Double = 0,
        cpuCount: Int = 1,
        memoryUsage: UInt64 = 0,
        memoryLimit: UInt64 = 0,
        networkRxRate: Double = 0,
        networkTxRate: Double = 0,
        blockReadRate: Double = 0,
        blockWriteRate: Double = 0,
        networkRxTotal: UInt64 = 0,
        networkTxTotal: UInt64 = 0,
        blockReadTotal: UInt64 = 0,
        blockWriteTotal: UInt64 = 0,
        pids: UInt64 = 0
    ) {
        self.timestamp = timestamp
        self.cpuPercent = cpuPercent
        self.cpuCount = cpuCount
        self.memoryUsage = memoryUsage
        self.memoryLimit = memoryLimit
        self.networkRxRate = networkRxRate
        self.networkTxRate = networkTxRate
        self.blockReadRate = blockReadRate
        self.blockWriteRate = blockWriteRate
        self.networkRxTotal = networkRxTotal
        self.networkTxTotal = networkTxTotal
        self.blockReadTotal = blockReadTotal
        self.blockWriteTotal = blockWriteTotal
        self.pids = pids
    }

    public var memoryPercent: Double {
        guard memoryLimit > 0 else { return 0 }
        return Double(memoryUsage) / Double(memoryLimit) * 100
    }

    /// CPU normalized to the whole machine (0-100%), which is what graphs should plot.
    public var normalizedCPUPercent: Double {
        cpuPercent / Double(max(1, cpuCount))
    }
}

/// Converts raw cumulative Docker counters into rates and percentages.
public enum StatsCalculator {
    /// Computes a sample from the current payload and, when available, the previous one we
    /// fetched. With one-shot stats Docker leaves `precpu_stats` empty, so our own previous
    /// sample is the baseline; with streamed stats `precpu_stats` is used as a fallback.
    public static func sample(current: ContainerStats, previous: ContainerStats?, fallbackTimestamp: Double) -> ResourceSample {
        let timestamp = DockerTimestamp.parse(current.read) ?? fallbackTimestamp
        let network = current.totalNetwork
        let block = current.totalBlockIO

        var sample = ResourceSample(
            timestamp: timestamp,
            cpuPercent: 0,
            cpuCount: current.cpu.effectiveCPUCount,
            memoryUsage: current.memoryUsageExcludingCache,
            memoryLimit: current.memory.limit ?? 0,
            networkRxTotal: network.rx,
            networkTxTotal: network.tx,
            blockReadTotal: block.read,
            blockWriteTotal: block.write,
            pids: current.pidsCurrent ?? UInt64(max(0, current.numProcs))
        )
        if current.numProcs > 0, current.cpu.systemUsage == nil {
            // Windows reports processors through num_procs.
            sample.cpuCount = current.numProcs
        }

        let baselineCPU: ContainerStats.CPUStats
        let baselineTimestamp: Double?
        if let previous {
            baselineCPU = previous.cpu
            baselineTimestamp = DockerTimestamp.parse(previous.read)
        } else {
            baselineCPU = current.precpu
            baselineTimestamp = DockerTimestamp.parse(current.preread)
        }

        sample.cpuPercent = cpuPercent(current: current.cpu, baseline: baselineCPU,
                                       elapsedSeconds: baselineTimestamp.map { timestamp - $0 },
                                       numProcs: current.numProcs)

        if let previous, let baselineTimestamp {
            let elapsed = timestamp - baselineTimestamp
            if elapsed > 0 {
                let previousNetwork = previous.totalNetwork
                let previousBlock = previous.totalBlockIO
                sample.networkRxRate = rate(network.rx, previousNetwork.rx, elapsed)
                sample.networkTxRate = rate(network.tx, previousNetwork.tx, elapsed)
                sample.blockReadRate = rate(block.read, previousBlock.read, elapsed)
                sample.blockWriteRate = rate(block.write, previousBlock.write, elapsed)
            }
        }
        return sample
    }

    /// Linux: share of host CPU time scaled by CPU count. Windows (no system usage): 100ns ticks
    /// over wall-clock time and processor count.
    public static func cpuPercent(
        current: ContainerStats.CPUStats,
        baseline: ContainerStats.CPUStats,
        elapsedSeconds: Double?,
        numProcs: Int = 0
    ) -> Double {
        guard current.usage.totalUsage >= baseline.usage.totalUsage else { return 0 }
        let cpuDelta = Double(current.usage.totalUsage - baseline.usage.totalUsage)

        if let system = current.systemUsage, let previousSystem = baseline.systemUsage, previousSystem > 0 {
            guard system > previousSystem else { return 0 }
            let systemDelta = Double(system - previousSystem)
            let value = cpuDelta / systemDelta * Double(current.effectiveCPUCount) * 100
            return value.isFinite ? max(0, value) : 0
        }

        if current.systemUsage == nil, let elapsedSeconds, elapsedSeconds > 0, baseline.usage.totalUsage > 0 {
            // Windows counts 100ns ticks. The CLI divides by ticks * processors to get a
            // whole-machine percentage; we keep dtop's single-CPU convention (400% = 4 busy cores).
            let intervals = elapsedSeconds * 10_000_000
            let value = cpuDelta / intervals * 100
            return value.isFinite ? max(0, value) : 0
        }
        return 0
    }

    /// Bytes per second; counter resets (container restart) yield 0 rather than a huge spike.
    public static func rate(_ current: UInt64, _ previous: UInt64, _ elapsedSeconds: Double) -> Double {
        guard elapsedSeconds > 0, current >= previous else { return 0 }
        return Double(current - previous) / elapsedSeconds
    }
}

/// Keeps the previous raw payload per container so each new payload can be turned into rates.
public struct StatsSampler: Sendable {
    private var previous: [String: ContainerStats] = [:]

    public init() {}

    public mutating func ingest(id: String, stats: ContainerStats, now: Double) -> ResourceSample {
        let sample = StatsCalculator.sample(current: stats, previous: previous[id], fallbackTimestamp: now)
        previous[id] = stats
        return sample
    }

    /// Drops baselines for containers that are gone or stopped.
    public mutating func retain(ids: Set<String>) {
        previous = previous.filter { ids.contains($0.key) }
    }

    public var trackedIDs: Set<String> { Set(previous.keys) }
}
