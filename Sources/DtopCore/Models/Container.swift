public enum ContainerState: Sendable, Hashable, Codable, CustomStringConvertible {
    case created
    case running
    case paused
    case restarting
    case removing
    case exited
    case dead
    case unknown(String)

    public init(rawValue: String) {
        switch rawValue.lowercased() {
        case "created": self = .created
        case "running": self = .running
        case "paused": self = .paused
        case "restarting": self = .restarting
        case "removing": self = .removing
        case "exited": self = .exited
        case "dead": self = .dead
        default: self = .unknown(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .created: return "created"
        case .running: return "running"
        case .paused: return "paused"
        case .restarting: return "restarting"
        case .removing: return "removing"
        case .exited: return "exited"
        case .dead: return "dead"
        case .unknown(let value): return value
        }
    }

    public var description: String { rawValue }

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        self.init(rawValue: (try? container.decode(String.self)) ?? "unknown")
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    /// Whether the container has live processes (stats and `top` are meaningful).
    public var isActive: Bool {
        self == .running || self == .restarting || self == .paused
    }

    /// Sort rank so that running containers float to the top of status-sorted lists.
    public var sortRank: Int {
        switch self {
        case .running: return 0
        case .restarting: return 1
        case .paused: return 2
        case .created: return 3
        case .removing: return 4
        case .exited: return 5
        case .dead: return 6
        case .unknown: return 7
        }
    }
}

public enum HealthStatus: String, Sendable, Equatable, Codable {
    case starting
    case healthy
    case unhealthy
    case none

    public init(rawValue: String) {
        switch rawValue.lowercased() {
        case "starting", "health: starting": self = .starting
        case "healthy": self = .healthy
        case "unhealthy": self = .unhealthy
        default: self = .none
        }
    }

    /// Extracts the health suffix from a status string such as `Up 3 minutes (healthy)`.
    public static func fromStatusText(_ status: String) -> HealthStatus? {
        guard let open = status.lastIndex(of: "("), let close = status.lastIndex(of: ")"), open < close else {
            return nil
        }
        let inner = String(status[status.index(after: open)..<close])
        let parsed = HealthStatus(rawValue: inner)
        return parsed == .none ? nil : parsed
    }
}

public struct PortBinding: Sendable, Hashable, Codable {
    public var ip: String?
    public var privatePort: Int
    public var publicPort: Int?
    public var type: String

    public init(ip: String? = nil, privatePort: Int, publicPort: Int? = nil, type: String = "tcp") {
        self.ip = ip
        self.privatePort = privatePort
        self.publicPort = publicPort
        self.type = type
    }

    enum CodingKeys: String, CodingKey {
        case ip = "IP"
        case privatePort = "PrivatePort"
        case publicPort = "PublicPort"
        case type = "Type"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ip = c.lenient(.ip)
        privatePort = Int(c.flexibleInt(.privatePort) ?? 0)
        publicPort = c.flexibleInt(.publicPort).map(Int.init)
        type = c.lenient(.type, default: "tcp")
    }

    /// Formatted like the Docker CLI: `0.0.0.0:8080->80/tcp` or `80/tcp`.
    public var display: String {
        guard let publicPort else { return "\(privatePort)/\(type)" }
        let host = ip.map { $0.contains(":") ? "[\($0)]" : $0 } ?? "0.0.0.0"
        return "\(host):\(publicPort)->\(privatePort)/\(type)"
    }
}

/// Well-known Docker Compose labels.
public enum ComposeLabel {
    public static let project = "com.docker.compose.project"
    public static let service = "com.docker.compose.service"
    public static let workingDir = "com.docker.compose.project.working_dir"
    public static let configFiles = "com.docker.compose.project.config_files"
    public static let containerNumber = "com.docker.compose.container-number"
    public static let oneOff = "com.docker.compose.oneoff"
    public static let dependsOn = "com.docker.compose.depends_on"
}

/// A row from `GET /containers/json`.
public struct Container: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var names: [String]
    public var image: String
    public var imageID: String
    public var command: String
    public var created: Int64
    public var ports: [PortBinding]
    public var labels: [String: String]
    public var state: ContainerState
    public var status: String

    public init(
        id: String,
        names: [String],
        image: String,
        imageID: String = "",
        command: String = "",
        created: Int64 = 0,
        ports: [PortBinding] = [],
        labels: [String: String] = [:],
        state: ContainerState,
        status: String = ""
    ) {
        self.id = id
        self.names = names
        self.image = image
        self.imageID = imageID
        self.command = command
        self.created = created
        self.ports = ports
        self.labels = labels
        self.state = state
        self.status = status
    }

    enum CodingKeys: String, CodingKey {
        case id = "Id"
        case names = "Names"
        case image = "Image"
        case imageID = "ImageID"
        case command = "Command"
        case created = "Created"
        case ports = "Ports"
        case labels = "Labels"
        case state = "State"
        case status = "Status"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        names = c.lenient(.names, default: [])
        image = c.lenient(.image, default: "")
        imageID = c.lenient(.imageID, default: "")
        command = c.lenient(.command, default: "")
        created = c.flexibleInt(.created) ?? 0
        ports = c.lenient(.ports, default: [])
        labels = c.lenient(.labels, default: [:])
        state = c.lenient(.state, default: .unknown("unknown"))
        status = c.lenient(.status, default: "")
    }

    public var shortID: String { String(id.prefix(12)) }

    /// Primary name without the leading slash Docker adds.
    public var name: String {
        guard let first = names.min(by: { $0.count < $1.count }) else { return shortID }
        return first.hasPrefix("/") ? String(first.dropFirst()) : first
    }

    public var health: HealthStatus? { HealthStatus.fromStatusText(status) }

    public var composeProject: String? { labels[ComposeLabel.project] }
    public var composeService: String? { labels[ComposeLabel.service] }
    public var composeWorkingDirectory: String? { labels[ComposeLabel.workingDir] }

    public var composeConfigFiles: [String] {
        guard let raw = labels[ComposeLabel.configFiles], !raw.isEmpty else { return [] }
        return raw.split(separator: ",").map { String($0).trimmingWhitespace() }.filter { !$0.isEmpty }
    }

    public var composeContainerNumber: Int? { labels[ComposeLabel.containerNumber].flatMap { Int($0) } }

    public var isComposeOneOff: Bool { labels[ComposeLabel.oneOff]?.lowercased() == "true" }

    /// Distinct, sorted published ports for compact display.
    public var portSummary: String {
        let unique = Array(Set(ports.map(\.display))).sorted()
        return unique.joined(separator: ", ")
    }
}
