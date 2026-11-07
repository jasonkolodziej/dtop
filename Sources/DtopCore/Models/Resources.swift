/// A row from `GET /images/json`.
public struct DockerImage: Sendable, Hashable, Decodable, Identifiable {
    public var id: String
    public var parentID: String
    public var repoTags: [String]
    public var repoDigests: [String]
    public var created: Int64
    public var size: Int64
    public var sharedSize: Int64
    public var containers: Int
    public var labels: [String: String]

    public init(
        id: String,
        parentID: String = "",
        repoTags: [String] = [],
        repoDigests: [String] = [],
        created: Int64 = 0,
        size: Int64 = 0,
        sharedSize: Int64 = -1,
        containers: Int = -1,
        labels: [String: String] = [:]
    ) {
        self.id = id
        self.parentID = parentID
        self.repoTags = repoTags
        self.repoDigests = repoDigests
        self.created = created
        self.size = size
        self.sharedSize = sharedSize
        self.containers = containers
        self.labels = labels
    }

    enum CodingKeys: String, CodingKey {
        case id = "Id", parentID = "ParentId", repoTags = "RepoTags", repoDigests = "RepoDigests"
        case created = "Created", size = "Size", sharedSize = "SharedSize", containers = "Containers"
        case labels = "Labels"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        parentID = c.lenient(.parentID, default: "")
        repoTags = c.lenient(.repoTags, default: []).filter { $0 != "<none>:<none>" }
        repoDigests = c.lenient(.repoDigests, default: []).filter { $0 != "<none>@<none>" }
        created = c.flexibleInt(.created) ?? 0
        size = c.flexibleInt(.size) ?? 0
        sharedSize = c.flexibleInt(.sharedSize) ?? -1
        containers = Int(c.flexibleInt(.containers) ?? -1)
        labels = c.lenient(.labels, default: [:])
    }

    /// `sha256:abcdef...` -> `abcdef123456`.
    public var shortID: String {
        let raw = id.hasPrefix("sha256:") ? String(id.dropFirst(7)) : id
        return String(raw.prefix(12))
    }

    public var isDangling: Bool { repoTags.isEmpty }

    /// Human-readable reference, falling back to the digest-pinned name or `<none>`.
    public var displayName: String {
        if let tag = repoTags.sorted().first { return tag }
        if let digest = repoDigests.first, let at = digest.firstIndex(of: "@") {
            return String(digest[..<at]) + "@<none>"
        }
        return "<none>"
    }
}

/// A row from `GET /networks`.
public struct DockerNetwork: Sendable, Hashable, Decodable, Identifiable {
    public struct IPAMConfig: Sendable, Hashable, Decodable {
        public var subnet: String
        public var gateway: String

        public init(subnet: String, gateway: String = "") {
            self.subnet = subnet
            self.gateway = gateway
        }

        enum CodingKeys: String, CodingKey { case subnet = "Subnet", gateway = "Gateway" }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            subnet = c.lenient(.subnet, default: "")
            gateway = c.lenient(.gateway, default: "")
        }
    }

    public var id: String
    public var name: String
    public var driver: String
    public var scope: String
    public var created: String
    public var isInternal: Bool
    public var attachable: Bool
    public var enableIPv6: Bool
    public var ipam: [IPAMConfig]
    public var containerIDs: [String]
    public var labels: [String: String]

    public init(
        id: String,
        name: String,
        driver: String = "bridge",
        scope: String = "local",
        created: String = "",
        isInternal: Bool = false,
        attachable: Bool = false,
        enableIPv6: Bool = false,
        ipam: [IPAMConfig] = [],
        containerIDs: [String] = [],
        labels: [String: String] = [:]
    ) {
        self.id = id
        self.name = name
        self.driver = driver
        self.scope = scope
        self.created = created
        self.isInternal = isInternal
        self.attachable = attachable
        self.enableIPv6 = enableIPv6
        self.ipam = ipam
        self.containerIDs = containerIDs
        self.labels = labels
    }

    enum CodingKeys: String, CodingKey {
        case id = "Id", name = "Name", driver = "Driver", scope = "Scope", created = "Created"
        case isInternal = "Internal", attachable = "Attachable", enableIPv6 = "EnableIPv6", ipam = "IPAM"
        case containers = "Containers", labels = "Labels"
    }

    enum IPAMKeys: String, CodingKey { case config = "Config" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenient(.name, default: "")
        driver = c.lenient(.driver, default: "")
        scope = c.lenient(.scope, default: "")
        created = c.lenient(.created, default: "")
        isInternal = c.lenient(.isInternal, default: false)
        attachable = c.lenient(.attachable, default: false)
        enableIPv6 = c.lenient(.enableIPv6, default: false)
        if let ipamContainer = try? c.nestedContainer(keyedBy: IPAMKeys.self, forKey: .ipam) {
            ipam = ipamContainer.lenient(.config, default: [])
        } else {
            ipam = []
        }
        if let containers = try? c.nestedContainer(keyedBy: AnyCodingKey.self, forKey: .containers) {
            containerIDs = containers.allKeys.map(\.stringValue).sorted()
        } else {
            containerIDs = []
        }
        labels = c.lenient(.labels, default: [:])
    }

    public var shortID: String { String(id.prefix(12)) }

    /// Networks Docker creates itself and refuses to delete.
    public var isBuiltIn: Bool { ["bridge", "host", "none", "nat"].contains(name) }

    public var composeProject: String? { labels[ComposeLabel.project] }
}

/// An entry from `GET /volumes`.
public struct DockerVolume: Sendable, Hashable, Decodable, Identifiable {
    public var name: String
    public var driver: String
    public var mountpoint: String
    public var createdAt: String
    public var scope: String
    public var labels: [String: String]
    public var sizeBytes: Int64?
    public var refCount: Int?

    public var id: String { name }

    public init(
        name: String,
        driver: String = "local",
        mountpoint: String = "",
        createdAt: String = "",
        scope: String = "local",
        labels: [String: String] = [:],
        sizeBytes: Int64? = nil,
        refCount: Int? = nil
    ) {
        self.name = name
        self.driver = driver
        self.mountpoint = mountpoint
        self.createdAt = createdAt
        self.scope = scope
        self.labels = labels
        self.sizeBytes = sizeBytes
        self.refCount = refCount
    }

    enum CodingKeys: String, CodingKey {
        case name = "Name", driver = "Driver", mountpoint = "Mountpoint", createdAt = "CreatedAt"
        case scope = "Scope", labels = "Labels", usageData = "UsageData"
    }

    enum UsageKeys: String, CodingKey { case size = "Size", refCount = "RefCount" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        driver = c.lenient(.driver, default: "")
        mountpoint = c.lenient(.mountpoint, default: "")
        createdAt = c.lenient(.createdAt, default: "")
        scope = c.lenient(.scope, default: "")
        labels = c.lenient(.labels, default: [:])
        if let usage = try? c.nestedContainer(keyedBy: UsageKeys.self, forKey: .usageData) {
            // -1 means "not computed" in the API.
            sizeBytes = usage.flexibleInt(.size).flatMap { $0 < 0 ? nil : $0 }
            refCount = usage.flexibleInt(.refCount).flatMap { $0 < 0 ? nil : Int($0) }
        } else {
            sizeBytes = nil
            refCount = nil
        }
    }

    public var composeProject: String? { labels[ComposeLabel.project] }

    /// Anonymous volumes have 64-hex-character generated names.
    public var isAnonymous: Bool {
        name.count == 64 && name.allSatisfy(\.isHexDigit)
    }
}

struct VolumeListResponse: Decodable {
    var volumes: [DockerVolume]
    var warnings: [String]

    enum CodingKeys: String, CodingKey { case volumes = "Volumes", warnings = "Warnings" }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        volumes = c.lenient(.volumes, default: [])
        warnings = c.lenient(.warnings, default: [])
    }
}

/// Output of `GET /containers/{id}/top`.
public struct ContainerTop: Sendable, Equatable, Decodable {
    public var titles: [String]
    public var processes: [[String]]

    public init(titles: [String], processes: [[String]]) {
        self.titles = titles
        self.processes = processes
    }

    enum CodingKeys: String, CodingKey { case titles = "Titles", processes = "Processes" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        titles = c.lenient(.titles, default: [])
        processes = c.lenient(.processes, default: [])
    }
}

/// Result of any `POST /{kind}/prune` call.
public struct PruneReport: Sendable, Equatable, Decodable {
    public var deleted: [String]
    public var spaceReclaimed: Int64

    public init(deleted: [String], spaceReclaimed: Int64) {
        self.deleted = deleted
        self.spaceReclaimed = spaceReclaimed
    }

    enum CodingKeys: String, CodingKey {
        case containersDeleted = "ContainersDeleted", volumesDeleted = "VolumesDeleted"
        case networksDeleted = "NetworksDeleted", imagesDeleted = "ImagesDeleted"
        case spaceReclaimed = "SpaceReclaimed"
    }

    enum ImageDeleteKeys: String, CodingKey { case untagged = "Untagged", deleted = "Deleted" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        var deleted: [String] = []
        deleted += c.lenient(.containersDeleted, default: [String]())
        deleted += c.lenient(.volumesDeleted, default: [String]())
        deleted += c.lenient(.networksDeleted, default: [String]())
        if var images = try? c.nestedUnkeyedContainer(forKey: .imagesDeleted) {
            while !images.isAtEnd {
                guard let item = try? images.nestedContainer(keyedBy: ImageDeleteKeys.self) else { break }
                if let value: String = item.lenient(.deleted) { deleted.append(value) }
                else if let value: String = item.lenient(.untagged) { deleted.append(value) }
            }
        }
        self.deleted = deleted
        spaceReclaimed = c.flexibleInt(.spaceReclaimed) ?? 0
    }
}

/// `GET /version`.
public struct DockerVersion: Sendable, Equatable, Decodable {
    public var version: String
    public var apiVersion: String
    public var minAPIVersion: String
    public var os: String
    public var arch: String
    public var kernelVersion: String

    public init(
        version: String,
        apiVersion: String,
        minAPIVersion: String = "",
        os: String = "",
        arch: String = "",
        kernelVersion: String = ""
    ) {
        self.version = version
        self.apiVersion = apiVersion
        self.minAPIVersion = minAPIVersion
        self.os = os
        self.arch = arch
        self.kernelVersion = kernelVersion
    }

    enum CodingKeys: String, CodingKey {
        case version = "Version", apiVersion = "ApiVersion", minAPIVersion = "MinAPIVersion", os = "Os"
        case arch = "Arch", kernelVersion = "KernelVersion"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = c.lenient(.version, default: "")
        apiVersion = c.lenient(.apiVersion, default: "")
        minAPIVersion = c.lenient(.minAPIVersion, default: "")
        os = c.lenient(.os, default: "")
        arch = c.lenient(.arch, default: "")
        kernelVersion = c.lenient(.kernelVersion, default: "")
    }
}

/// `GET /info` (host-level summary for the header bar).
public struct SystemInfo: Sendable, Equatable, Decodable {
    public var name: String
    public var serverVersion: String
    public var operatingSystem: String
    public var osType: String
    public var architecture: String
    public var cpuCount: Int
    public var memoryTotal: Int64
    public var containers: Int
    public var containersRunning: Int
    public var containersPaused: Int
    public var containersStopped: Int
    public var images: Int

    public init(
        name: String = "",
        serverVersion: String = "",
        operatingSystem: String = "",
        osType: String = "",
        architecture: String = "",
        cpuCount: Int = 0,
        memoryTotal: Int64 = 0,
        containers: Int = 0,
        containersRunning: Int = 0,
        containersPaused: Int = 0,
        containersStopped: Int = 0,
        images: Int = 0
    ) {
        self.name = name
        self.serverVersion = serverVersion
        self.operatingSystem = operatingSystem
        self.osType = osType
        self.architecture = architecture
        self.cpuCount = cpuCount
        self.memoryTotal = memoryTotal
        self.containers = containers
        self.containersRunning = containersRunning
        self.containersPaused = containersPaused
        self.containersStopped = containersStopped
        self.images = images
    }

    enum CodingKeys: String, CodingKey {
        case name = "Name", serverVersion = "ServerVersion", operatingSystem = "OperatingSystem"
        case osType = "OSType", architecture = "Architecture", cpuCount = "NCPU", memoryTotal = "MemTotal"
        case containers = "Containers", containersRunning = "ContainersRunning"
        case containersPaused = "ContainersPaused", containersStopped = "ContainersStopped", images = "Images"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = c.lenient(.name, default: "")
        serverVersion = c.lenient(.serverVersion, default: "")
        operatingSystem = c.lenient(.operatingSystem, default: "")
        osType = c.lenient(.osType, default: "")
        architecture = c.lenient(.architecture, default: "")
        cpuCount = Int(c.flexibleInt(.cpuCount) ?? 0)
        memoryTotal = c.flexibleInt(.memoryTotal) ?? 0
        containers = Int(c.flexibleInt(.containers) ?? 0)
        containersRunning = Int(c.flexibleInt(.containersRunning) ?? 0)
        containersPaused = Int(c.flexibleInt(.containersPaused) ?? 0)
        containersStopped = Int(c.flexibleInt(.containersStopped) ?? 0)
        images = Int(c.flexibleInt(.images) ?? 0)
    }
}

/// One message from `GET /events`.
public struct DockerEvent: Sendable, Equatable, Decodable {
    public var type: String
    public var action: String
    public var actorID: String
    public var attributes: [String: String]
    public var time: Int64
    public var timeNano: Int64

    public init(
        type: String,
        action: String,
        actorID: String,
        attributes: [String: String] = [:],
        time: Int64 = 0,
        timeNano: Int64 = 0
    ) {
        self.type = type
        self.action = action
        self.actorID = actorID
        self.attributes = attributes
        self.time = time
        self.timeNano = timeNano
    }

    enum CodingKeys: String, CodingKey {
        case type = "Type", action = "Action", actor = "Actor", time, timeNano
        case legacyStatus = "status", legacyID = "id"
    }

    enum ActorKeys: String, CodingKey { case id = "ID", attributes = "Attributes" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = c.lenient(.type, default: "")
        let action: String = c.lenient(.action, default: "")
        self.action = action.isEmpty ? c.lenient(.legacyStatus, default: "") : action
        if let actor = try? c.nestedContainer(keyedBy: ActorKeys.self, forKey: .actor) {
            actorID = actor.lenient(.id, default: "")
            attributes = actor.lenient(.attributes, default: [:])
        } else {
            actorID = c.lenient(.legacyID, default: "")
            attributes = [:]
        }
        time = c.flexibleInt(.time) ?? 0
        timeNano = c.flexibleInt(.timeNano) ?? 0
    }

    /// Exec events look like `exec_start: sh -c ...`; strip the payload to get the verb.
    public var baseAction: String {
        String(action.split(separator: ":", maxSplits: 1).first ?? Substring(action)).trimmingWhitespace()
    }

    /// Events that change what a container list or resource list should show.
    public var affectsInventory: Bool {
        switch type {
        case "container":
            return [
                "create", "start", "stop", "die", "kill", "pause", "unpause", "restart",
                "destroy", "rename", "update", "health_status", "oom"
            ].contains(baseAction)
        case "image", "network", "volume":
            return true
        default:
            return false
        }
    }
}
