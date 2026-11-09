import DtopCore

public enum ConnectionState: Sendable, Equatable {
    case connecting
    case connected
    case disconnected(message: String, retryAt: Double?)

    public var isConnected: Bool { self == .connected }
}

/// Where the followed log stream of the selected container stands.
public enum LogStreamState: Sendable, Equatable {
    /// Not streaming (logs were fetched once, or nothing was asked for).
    case idle
    /// `logs -f` is open and new lines are appended as they arrive.
    case live
    /// The stream ended: the container stopped, or the connection broke (with the reason).
    case ended(String?)
}

/// Data for the detail panel of one container. Loaded lazily for the selection only.
public struct ContainerDetailData: Sendable, Equatable {
    public var containerID: String
    public var details: ContainerDetails?
    public var processes: [ProcessTreeRow]
    public var logs: [LogLine]
    public var logState: LogStreamState = .idle
    public var error: String?
    public var loadedAt: Double

    /// Lines kept for a followed stream; older lines scroll out.
    public static let maximumLogLines = 5_000

    /// Appends streamed lines, keeping at most `maximumLogLines`. Returns how many lines were
    /// dropped from the front.
    @discardableResult
    public mutating func appendLogs(_ lines: [LogLine]) -> Int {
        logs += lines
        let overflow = logs.count - Self.maximumLogLines
        guard overflow > 0 else { return 0 }
        logs.removeFirst(overflow)
        return overflow
    }

    public init(
        containerID: String,
        details: ContainerDetails? = nil,
        processes: [ProcessTreeRow] = [],
        logs: [LogLine] = [],
        error: String? = nil,
        loadedAt: Double = 0
    ) {
        self.containerID = containerID
        self.details = details
        self.processes = processes
        self.logs = logs
        self.error = error
        self.loadedAt = loadedAt
    }
}

/// Everything the UI knows about the Docker host at one moment.
public struct DockerSnapshot: Sendable {
    public var endpoint: String
    public var connection: ConnectionState
    public var version: DockerVersion?
    public var info: SystemInfo?
    public var containers: [Container]
    public var images: [DockerImage]
    public var networks: [DockerNetwork]
    public var volumes: [DockerVolume]
    public var metrics: MetricsStore
    public var detail: ContainerDetailData?
    public var lastUpdated: Double?
    /// Errors from secondary lists (e.g. volumes) that should not mark the connection down.
    public var warnings: [String]

    public init(
        endpoint: String = "",
        connection: ConnectionState = .connecting,
        version: DockerVersion? = nil,
        info: SystemInfo? = nil,
        containers: [Container] = [],
        images: [DockerImage] = [],
        networks: [DockerNetwork] = [],
        volumes: [DockerVolume] = [],
        metrics: MetricsStore = MetricsStore(),
        detail: ContainerDetailData? = nil,
        lastUpdated: Double? = nil,
        warnings: [String] = []
    ) {
        self.endpoint = endpoint
        self.connection = connection
        self.version = version
        self.info = info
        self.containers = containers
        self.images = images
        self.networks = networks
        self.volumes = volumes
        self.metrics = metrics
        self.detail = detail
        self.lastUpdated = lastUpdated
        self.warnings = warnings
    }

    public func container(id: String) -> Container? {
        containers.first { $0.id == id }
    }

    public var runningCount: Int { containers.filter { $0.state == .running }.count }
    public var stoppedCount: Int { containers.filter { !$0.state.isActive }.count }
}

/// One line about a host for the header chips and the host list.
public struct HostSummary: Sendable, Equatable {
    public var name: String
    public var description: String
    public var connection: ConnectionState
    public var running: Int
    public var total: Int
    public var cpuPercent: Double?
    public var cpuCount: Int
    public var memoryUsed: UInt64?
    public var memoryTotal: UInt64

    public init(name: String, description: String, snapshot: DockerSnapshot) {
        self.name = name
        self.description = description
        connection = snapshot.connection
        running = snapshot.runningCount
        total = snapshot.containers.count
        let connected = snapshot.connection.isConnected && snapshot.lastUpdated != nil
        cpuPercent = connected ? snapshot.metrics.totalLatest.cpuPercent : nil
        cpuCount = snapshot.info?.cpuCount ?? 0
        memoryUsed = connected ? snapshot.metrics.totalLatest.memoryUsage : nil
        memoryTotal = UInt64(clamping: snapshot.info?.memoryTotal ?? 0)
    }
}
