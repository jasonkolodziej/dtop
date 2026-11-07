import DtopCore

public enum ConnectionState: Sendable, Equatable {
    case connecting
    case connected
    case disconnected(message: String, retryAt: Double?)

    public var isConnected: Bool { self == .connected }
}

/// Data for the detail panel of one container. Loaded lazily for the selection only.
public struct ContainerDetailData: Sendable, Equatable {
    public var containerID: String
    public var details: ContainerDetails?
    public var processes: [ProcessTreeRow]
    public var logs: [LogLine]
    public var error: String?
    public var loadedAt: Double

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
