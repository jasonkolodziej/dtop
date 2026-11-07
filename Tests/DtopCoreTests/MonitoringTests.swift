import Testing
@testable import DtopCore

@Suite("Stats calculator")
struct StatsCalculatorTests {
    func stats(_ read: String, total: UInt64, system: UInt64, rx: UInt64 = 0, tx: UInt64 = 0,
               blockRead: UInt64 = 0, blockWrite: UInt64 = 0) throws -> ContainerStats {
        try Fixtures.decode(ContainerStats.self, Fixtures.linuxStats(read: read, total: total, system: system, rx: rx, tx: tx,
                                                                      blockRead: blockRead, blockWrite: blockWrite))
    }

    @Test("CPU% follows docker stats: delta / system delta * cpus * 100")
    func cpuFormula() throws {
        let first = try stats("2024-05-01T10:00:00Z", total: 1_000_000, system: 100_000_000)
        let second = try stats("2024-05-01T10:00:01Z", total: 3_000_000, system: 104_000_000)
        let sample = StatsCalculator.sample(current: second, previous: first, fallbackTimestamp: 0)
        // 2e6 / 4e6 * 4 cpus * 100 = 200%
        #expect(abs(sample.cpuPercent - 200) < 1e-9)
        #expect(sample.cpuCount == 4)
        #expect(abs(sample.normalizedCPUPercent - 50) < 1e-9)
    }

    @Test("rates use the elapsed time between reads")
    func rates() throws {
        let first = try stats("2024-05-01T10:00:00Z", total: 0, system: 1, rx: 1_000, tx: 2_000, blockRead: 0, blockWrite: 0)
        let second = try stats("2024-05-01T10:00:02Z", total: 0, system: 2, rx: 5_000, tx: 2_000, blockRead: 8_192, blockWrite: 4_096)
        let sample = StatsCalculator.sample(current: second, previous: first, fallbackTimestamp: 0)
        #expect(sample.networkRxRate == 2_000)
        #expect(sample.networkTxRate == 0)
        #expect(sample.blockReadRate == 4_096)
        #expect(sample.blockWriteRate == 2_048)
        #expect(sample.networkRxTotal == 5_000)
        #expect(sample.pids == 7)
    }

    @Test("without a previous sample, one-shot stats yield 0% CPU and no rates")
    func firstSample() throws {
        let sample = StatsCalculator.sample(current: try stats("2024-05-01T10:00:00Z", total: 9, system: 99), previous: nil,
                                            fallbackTimestamp: 0)
        #expect(sample.cpuPercent == 0)
        #expect(sample.networkRxRate == 0)
        #expect(sample.memoryUsage == 104_857_600 - 4_194_304)
        #expect(sample.memoryLimit == 1_073_741_824)
        #expect(abs(sample.memoryPercent - 9.375) < 1e-9)
    }

    @Test("streamed stats fall back to precpu_stats for the baseline")
    func precpuFallback() {
        let current = ContainerStats(
            read: "2024-05-01T10:00:01Z", preread: "2024-05-01T10:00:00Z",
            cpu: .init(usage: .init(totalUsage: 2_000), systemUsage: 20_000, onlineCPUs: 2),
            precpu: .init(usage: .init(totalUsage: 1_000), systemUsage: 10_000, onlineCPUs: 2))
        let sample = StatsCalculator.sample(current: current, previous: nil, fallbackTimestamp: 0)
        #expect(abs(sample.cpuPercent - 20) < 1e-9)
    }

    @Test("counter resets after a restart produce zero, not huge spikes")
    func counterReset() throws {
        let before = try stats("2024-05-01T10:00:00Z", total: 9_000_000, system: 100, rx: 10_000)
        let after = try stats("2024-05-01T10:00:01Z", total: 100, system: 200, rx: 10)
        let sample = StatsCalculator.sample(current: after, previous: before, fallbackTimestamp: 0)
        #expect(sample.cpuPercent == 0)
        #expect(sample.networkRxRate == 0)
        #expect(StatsCalculator.rate(5, 10, 1) == 0)
        #expect(StatsCalculator.rate(10, 5, 0) == 0)
    }

    @Test("system counter going backwards or staying flat yields 0")
    func flatSystem() {
        let cpu = ContainerStats.CPUStats(usage: .init(totalUsage: 200), systemUsage: 1_000, onlineCPUs: 2)
        let same = ContainerStats.CPUStats(usage: .init(totalUsage: 100), systemUsage: 1_000, onlineCPUs: 2)
        #expect(StatsCalculator.cpuPercent(current: cpu, baseline: same, elapsedSeconds: 1) == 0)
    }

    @Test("Windows CPU uses 100ns ticks over wall time")
    func windowsCPU() throws {
        let stats = try Fixtures.decode(ContainerStats.self, Fixtures.windowsStats)
        let sample = StatsCalculator.sample(current: stats, previous: nil, fallbackTimestamp: 0)
        // 5,000,000 ticks (0.5s of CPU) over 1s wall time = 50% of one CPU.
        #expect(abs(sample.cpuPercent - 50) < 1e-9)
        #expect(sample.cpuCount == 2)
        #expect(abs(sample.normalizedCPUPercent - 25) < 1e-9)
        #expect(sample.memoryUsage == 30_000_000)
    }

    @Test("an unparseable read timestamp uses the fallback clock")
    func fallbackTimestamp() {
        let stats = ContainerStats(read: "garbage", cpu: .init(usage: .init(totalUsage: 0)))
        #expect(StatsCalculator.sample(current: stats, previous: nil, fallbackTimestamp: 42).timestamp == 42)
    }

    @Test("memory percent is 0 without a limit")
    func noLimit() {
        #expect(ResourceSample(memoryUsage: 100, memoryLimit: 0).memoryPercent == 0)
        #expect(ResourceSample(cpuPercent: 100, cpuCount: 0).normalizedCPUPercent == 100)
    }
}

@Suite("Stats sampler")
struct StatsSamplerTests {
    @Test("keeps per-container baselines and forgets removed containers")
    func baselines() throws {
        var sampler = StatsSampler()
        let a1 = try Fixtures.decode(ContainerStats.self, Fixtures.linuxStats(read: "2024-05-01T10:00:00Z", total: 0, system: 1_000))
        let a2 = try Fixtures.decode(ContainerStats.self, Fixtures.linuxStats(read: "2024-05-01T10:00:01Z", total: 100, system: 1_400))
        #expect(sampler.ingest(id: "a", stats: a1, now: 0).cpuPercent == 0)
        #expect(sampler.ingest(id: "a", stats: a2, now: 1).cpuPercent == 100)
        _ = sampler.ingest(id: "b", stats: a1, now: 1)
        #expect(sampler.trackedIDs == ["a", "b"])
        sampler.retain(ids: ["b"])
        #expect(sampler.trackedIDs == ["b"])
        #expect(sampler.ingest(id: "a", stats: a2, now: 2).cpuPercent == 0, "baseline was dropped")
    }
}

@Suite("Ring buffer")
struct RingBufferTests {
    @Test("keeps the newest elements in order once full")
    func overwrite() {
        var buffer = RingBuffer<Int>(capacity: 3)
        #expect(buffer.isEmpty)
        #expect(buffer.first == nil)
        #expect(buffer.last == nil)
        for value in 1...5 { buffer.append(value) }
        #expect(buffer.isFull)
        #expect(buffer.count == 3)
        #expect(buffer.elements == [3, 4, 5])
        #expect(buffer[0] == 3)
        #expect(buffer.first == 3)
        #expect(buffer.last == 5)
        #expect(Array(buffer) == [3, 4, 5])
    }

    @Test("suffix returns the newest n, clamped")
    func suffix() {
        var buffer = RingBuffer<Int>(capacity: 4)
        for value in 1...6 { buffer.append(value) }
        #expect(buffer.suffix(2) == [5, 6])
        #expect(buffer.suffix(10) == [3, 4, 5, 6])
        #expect(buffer.suffix(0) == [])
        #expect(buffer.suffix(-1) == [])
    }

    @Test("removeAll resets state")
    func reset() {
        var buffer = RingBuffer<String>(capacity: 2)
        buffer.append("a")
        buffer.append("b")
        buffer.append("c")
        buffer.removeAll()
        #expect(buffer.isEmpty)
        buffer.append("d")
        #expect(buffer.elements == ["d"])
    }

    @Test("capacity one always holds the latest value")
    func capacityOne() {
        var buffer = RingBuffer<Int>(capacity: 1)
        for value in 0..<10 { buffer.append(value) }
        #expect(buffer.elements == [9])
    }
}

@Suite("Metrics store")
struct MetricsStoreTests {
    @Test("records samples, aggregates totals and drops history of vanished containers")
    func record() {
        var store = MetricsStore(capacity: 5)
        store.record([
            "a": ResourceSample(cpuPercent: 50, cpuCount: 4, memoryUsage: 100, networkRxRate: 10, pids: 2),
            "b": ResourceSample(cpuPercent: 25, cpuCount: 4, memoryUsage: 300, networkTxRate: 5, pids: 3)
        ], timestamp: 1)
        #expect(store.totalLatest.cpuPercent == 75)
        #expect(store.totalLatest.memoryUsage == 400)
        #expect(store.totalLatest.networkRxRate == 10)
        #expect(store.totalLatest.networkTxRate == 5)
        #expect(store.totalLatest.pids == 5)
        #expect(store.totalLatest.cpuCount == 4)
        #expect(store.total.cpu.elements == [75])
        #expect(store.series["a"]?.cpu.elements == [50])

        store.record(["a": ResourceSample(cpuPercent: 10, memoryUsage: 1)], timestamp: 2)
        #expect(store.series["b"] == nil)
        #expect(store.series["a"]?.cpu.elements == [50, 10])
        #expect(store.total.cpu.elements == [75, 10])

        for tick in 0..<10 { store.record([:], timestamp: Double(tick)) }
        #expect(store.total.cpu.count == 5)
        #expect(store.series.isEmpty)
        #expect(store.totalLatest.cpuCount == 1)

        store.reset()
        #expect(store.total.cpu.isEmpty)
        #expect(store.latest.isEmpty)
    }
}

@Suite("Backoff")
struct BackoffTests {
    @Test("grows exponentially up to the cap without jitter")
    func exponential() {
        var backoff = Backoff(initialDelay: 0.5, maximumDelay: 4, multiplier: 2, jitter: 0)
        let delays = (0..<6).map { _ in backoff.nextDelay() }
        #expect(delays == [0.5, 1, 2, 4, 4, 4])
        #expect(backoff.attempt == 6)
        backoff.reset()
        #expect(backoff.nextDelay() == 0.5)
    }

    @Test("jitter stays within bounds and never exceeds the cap")
    func jitter() {
        var low = Backoff(initialDelay: 1, maximumDelay: 10, multiplier: 2, jitter: 0.2)
        #expect(abs(low.nextDelay(random: { 0 }) - 0.8) < 1e-9)
        var high = Backoff(initialDelay: 1, maximumDelay: 10, multiplier: 2, jitter: 0.2)
        #expect(abs(high.nextDelay(random: { 0.999999 }) - 1.2) < 1e-4)
        var capped = Backoff(initialDelay: 10, maximumDelay: 10, multiplier: 2, jitter: 0.5)
        #expect(capped.nextDelay(random: { 0.999 }) == 10)
    }

    @Test("jitter is clamped to 0...1")
    func clampJitter() {
        #expect(Backoff(jitter: 5).jitter == 1)
        #expect(Backoff(jitter: -1).jitter == 0)
    }

    @Test("large attempt counts do not overflow")
    func manyAttempts() {
        var backoff = Backoff(initialDelay: 1, maximumDelay: 30, multiplier: 2, jitter: 0)
        for _ in 0..<10_000 { _ = backoff.nextDelay() }
        #expect(backoff.baseDelay == 30)
    }
}

@Suite("Process tree")
struct ProcessTreeTests {
    @Test("parses ps -ef columns by title")
    func parse() throws {
        let processes = ProcessTree.parse(try Fixtures.decode(ContainerTop.self, Fixtures.top))
        #expect(processes.map(\.pid) == [100, 120, 121, 130])
        #expect(processes[0].parentPID == 50)
        #expect(processes[0].user == "root")
        #expect(processes[1].cpu == "1")
        #expect(processes[0].command == "nginx: master process nginx -g daemon off;")
    }

    @Test("joins commands that ps split across trailing cells and skips bad rows")
    func parseQuirks() {
        let top = ContainerTop(titles: ["PID", "PPID", "CMD"], processes: [
            ["1", "0", "sleep", "100"], ["oops", "1", "x"], ["2"]
        ])
        let processes = ProcessTree.parse(top)
        #expect(processes.count == 2)
        #expect(processes[0].command == "sleep 100")
        #expect(processes[1].command == "2", "falls back to the last cell when CMD is missing")
    }

    @Test("Windows-style top output without PPID still parses")
    func windowsColumns() {
        let top = ContainerTop(titles: ["Name", "PID", "CPU", "Private Working Set"],
                               processes: [["smss.exe", "100", "00:00:00.031", "245.8kB"]])
        let processes = ProcessTree.parse(top)
        #expect(processes.count == 1)
        #expect(processes[0].parentPID == nil)
        #expect(processes[0].command == "smss.exe")
        #expect(processes[0].cpu == "00:00:00.031")
    }

    @Test("missing PID column yields nothing")
    func noPID() {
        #expect(ProcessTree.parse(ContainerTop(titles: ["CMD"], processes: [["x"]])).isEmpty)
    }

    @Test("builds a depth-first tree with box-drawing prefixes")
    func build() throws {
        let rows = ProcessTree.build(ProcessTree.parse(try Fixtures.decode(ContainerTop.self, Fixtures.top)))
        #expect(rows.map(\.process.pid) == [100, 120, 121, 130])
        #expect(rows.map(\.depth) == [0, 1, 1, 2])
        #expect(rows.map(\.prefix) == ["", "├─ ", "└─ ", "   └─ "])
    }

    @Test("guides continue for non-last ancestors")
    func guides() {
        let processes = [
            ContainerProcess(pid: 1, parentPID: 0, command: "init"),
            ContainerProcess(pid: 2, parentPID: 1, command: "a"),
            ContainerProcess(pid: 3, parentPID: 2, command: "a1"),
            ContainerProcess(pid: 4, parentPID: 1, command: "b")
        ]
        let rows = ProcessTree.build(processes)
        #expect(rows.map(\.prefix) == ["", "├─ ", "│  └─ ", "└─ "])
    }

    @Test("orphans become roots and cycles do not hang or drop processes")
    func orphansAndCycles() {
        let orphans = ProcessTree.build([
            ContainerProcess(pid: 10, parentPID: 999, command: "x"),
            ContainerProcess(pid: 5, parentPID: nil, command: "y")
        ])
        #expect(orphans.map(\.process.pid) == [5, 10])
        #expect(orphans.allSatisfy { $0.depth == 0 })

        let cycle = ProcessTree.build([
            ContainerProcess(pid: 1, parentPID: 2, command: "a"),
            ContainerProcess(pid: 2, parentPID: 1, command: "b"),
            ContainerProcess(pid: 3, parentPID: 3, command: "self")
        ])
        #expect(Set(cycle.map(\.process.pid)) == [1, 2, 3])
        #expect(cycle.count == 3)
    }
}

@Suite("Formatting")
struct FormattingTests {
    @Test("binary byte units", arguments: [
        (0.0, "0B"), (-5.0, "0B"), (Double.nan, "0B"), (512.0, "512B"), (1023.4, "1023B"), (1023.6, "1.0KiB"),
        (1024.0, "1.0KiB"), (1536.0, "1.5KiB"), (10_240.0, "10KiB"), (1_048_576.0, "1.0MiB"),
        (5.5 * 1_073_741_824, "5.5GiB"), (2.0 * 1_099_511_627_776, "2.0TiB")
    ])
    func bytes(value: Double, expected: String) {
        #expect(Format.bytes(value) == expected)
    }

    @Test("integer overload and rates")
    func integerAndRate() {
        #expect(Format.bytes(Int64(2048)) == "2.0KiB")
        #expect(Format.bytes(UInt64(0)) == "0B")
        #expect(Format.rate(1536) == "1.5KiB/s")
    }

    @Test("compact bytes for dense columns", arguments: [
        (0.0, "0"), (0.5, "0"), (999.0, "999B"), (1536.0, "1.5K"), (20 * 1_048_576.0, "20M"), (3.25 * 1_073_741_824, "3.3G")
    ])
    func compact(value: Double, expected: String) {
        #expect(Format.compactBytes(value) == expected)
    }

    @Test("fixed-point formatting is locale independent and rounds half away from zero")
    func fixed() {
        #expect(Format.fixed(3.14159, decimals: 2) == "3.14")
        #expect(Format.fixed(2.5, decimals: 0) == "3")
        #expect(Format.fixed(0.05, decimals: 1) == "0.1")
        #expect(Format.fixed(-1.25, decimals: 1) == "-1.3")
        #expect(Format.fixed(-0.04, decimals: 1) == "0.0")
        #expect(Format.fixed(9.999, decimals: 2) == "10.00")
        #expect(Format.fixed(.infinity, decimals: 1) == "0")
        #expect(Format.fixed(1, decimals: 3) == "1.000")
        #expect(Format.percent(12.345) == "12.3%")
        #expect(Format.percent(.nan) == "0%")
    }

    @Test("durations show the two most significant units", arguments: [
        (0.0, "0s"), (59.0, "59s"), (60.0, "1m"), (61.0, "1m 1s"), (3600.0, "1h"), (3725.0, "1h 2m"),
        (86_400.0, "1d"), (90_000.0, "1d 1h"), (-1.0, "0s")
    ])
    func durations(seconds: Double, expected: String) {
        #expect(Format.duration(seconds) == expected)
    }

    @Test("ages are relative to an explicit clock")
    func ages() {
        #expect(Format.age(since: 100, now: 100.5) == "just now")
        #expect(Format.age(since: 100, now: 50) == "just now")
        #expect(Format.age(since: 0, now: 7200) == "2h ago")
    }
}
