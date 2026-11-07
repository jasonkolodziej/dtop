/// Rolling metric series for one container (or for the whole host).
public struct MetricSeries: Sendable {
    public var cpu: RingBuffer<Double>
    public var memory: RingBuffer<Double>
    public var networkRx: RingBuffer<Double>
    public var networkTx: RingBuffer<Double>
    public var blockRead: RingBuffer<Double>
    public var blockWrite: RingBuffer<Double>

    public init(capacity: Int) {
        cpu = RingBuffer(capacity: capacity)
        memory = RingBuffer(capacity: capacity)
        networkRx = RingBuffer(capacity: capacity)
        networkTx = RingBuffer(capacity: capacity)
        blockRead = RingBuffer(capacity: capacity)
        blockWrite = RingBuffer(capacity: capacity)
    }

    public mutating func append(_ sample: ResourceSample) {
        cpu.append(sample.cpuPercent)
        memory.append(Double(sample.memoryUsage))
        networkRx.append(sample.networkRxRate)
        networkTx.append(sample.networkTxRate)
        blockRead.append(sample.blockReadRate)
        blockWrite.append(sample.blockWriteRate)
    }
}

/// Latest samples and history for all containers plus host-wide aggregates.
public struct MetricsStore: Sendable {
    public let capacity: Int
    public private(set) var latest: [String: ResourceSample] = [:]
    public private(set) var series: [String: MetricSeries] = [:]
    public private(set) var total: MetricSeries
    /// Most recent aggregate across containers.
    public private(set) var totalLatest = ResourceSample()

    public init(capacity: Int = 240) {
        self.capacity = capacity
        total = MetricSeries(capacity: capacity)
    }

    /// Records one polling round. Containers missing from `samples` lose their history so the
    /// store never grows with churned containers.
    public mutating func record(_ samples: [String: ResourceSample], timestamp: Double) {
        latest = samples
        series = series.filter { samples[$0.key] != nil }
        for (id, sample) in samples {
            series[id, default: MetricSeries(capacity: capacity)].append(sample)
        }
        var aggregate = ResourceSample(timestamp: timestamp)
        aggregate.cpuCount = samples.values.map(\.cpuCount).max() ?? 1
        for sample in samples.values {
            aggregate.cpuPercent += sample.cpuPercent
            aggregate.memoryUsage &+= sample.memoryUsage
            aggregate.networkRxRate += sample.networkRxRate
            aggregate.networkTxRate += sample.networkTxRate
            aggregate.blockReadRate += sample.blockReadRate
            aggregate.blockWriteRate += sample.blockWriteRate
            aggregate.pids &+= sample.pids
        }
        totalLatest = aggregate
        total.append(aggregate)
    }

    public mutating func reset() {
        latest = [:]
        series = [:]
        total = MetricSeries(capacity: capacity)
        totalLatest = ResourceSample()
    }
}
