/// The subset of `GET /containers/{id}/json` that dtop renders in the details panel.
public struct ContainerDetails: Sendable, Equatable, Decodable {
    public struct State: Sendable, Equatable, Decodable {
        public var status: ContainerState
        public var running: Bool
        public var paused: Bool
        public var restarting: Bool
        public var oomKilled: Bool
        public var dead: Bool
        public var pid: Int
        public var exitCode: Int
        public var error: String
        public var startedAt: String
        public var finishedAt: String
        public var health: HealthStatus?
        public var failingStreak: Int

        enum CodingKeys: String, CodingKey {
            case status = "Status", running = "Running", paused = "Paused", restarting = "Restarting"
            case oomKilled = "OOMKilled", dead = "Dead", pid = "Pid", exitCode = "ExitCode", error = "Error"
            case startedAt = "StartedAt", finishedAt = "FinishedAt", health = "Health"
        }

        enum HealthKeys: String, CodingKey {
            case status = "Status", failingStreak = "FailingStreak"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            status = c.lenient(.status, default: .unknown("unknown"))
            running = c.lenient(.running, default: false)
            paused = c.lenient(.paused, default: false)
            restarting = c.lenient(.restarting, default: false)
            oomKilled = c.lenient(.oomKilled, default: false)
            dead = c.lenient(.dead, default: false)
            pid = Int(c.flexibleInt(.pid) ?? 0)
            exitCode = Int(c.flexibleInt(.exitCode) ?? 0)
            error = c.lenient(.error, default: "")
            startedAt = c.lenient(.startedAt, default: "")
            finishedAt = c.lenient(.finishedAt, default: "")
            if let healthContainer = try? c.nestedContainer(keyedBy: HealthKeys.self, forKey: .health) {
                let raw: String = healthContainer.lenient(.status, default: "")
                health = raw.isEmpty ? nil : HealthStatus(rawValue: raw)
                failingStreak = Int(healthContainer.flexibleInt(.failingStreak) ?? 0)
            } else {
                health = nil
                failingStreak = 0
            }
        }
    }

    public struct Mount: Sendable, Equatable, Decodable {
        public var type: String
        public var name: String?
        public var source: String
        public var destination: String
        public var mode: String
        public var readWrite: Bool

        enum CodingKeys: String, CodingKey {
            case type = "Type", name = "Name", source = "Source", destination = "Destination", mode = "Mode"
            case readWrite = "RW"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            type = c.lenient(.type, default: "")
            name = c.lenient(.name)
            source = c.lenient(.source, default: "")
            destination = c.lenient(.destination, default: "")
            mode = c.lenient(.mode, default: "")
            readWrite = c.lenient(.readWrite, default: true)
        }
    }

    public struct NetworkAttachment: Sendable, Equatable {
        public var name: String
        public var ipAddress: String
        public var gateway: String
        public var macAddress: String
    }

    public var id: String
    public var name: String
    public var created: String
    public var path: String
    public var args: [String]
    public var state: State
    public var image: String
    public var imageName: String
    public var restartCount: Int
    public var hostname: String
    public var environment: [String]
    public var labels: [String: String]
    public var tty: Bool
    public var workingDir: String
    public var restartPolicy: String
    public var memoryLimit: Int64
    public var nanoCPUs: Int64
    public var mounts: [Mount]
    public var networks: [NetworkAttachment]
    public var portBindings: [String: [String]]

    enum CodingKeys: String, CodingKey {
        case id = "Id", name = "Name", created = "Created", path = "Path", args = "Args", state = "State"
        case image = "Image", restartCount = "RestartCount", config = "Config", hostConfig = "HostConfig"
        case mounts = "Mounts", networkSettings = "NetworkSettings"
    }

    enum ConfigKeys: String, CodingKey {
        case hostname = "Hostname", env = "Env", labels = "Labels", tty = "Tty", workingDir = "WorkingDir"
        case image = "Image"
    }

    enum HostConfigKeys: String, CodingKey {
        case restartPolicy = "RestartPolicy", memory = "Memory", nanoCPUs = "NanoCpus"
    }

    enum RestartPolicyKeys: String, CodingKey {
        case name = "Name"
    }

    enum NetworkSettingsKeys: String, CodingKey {
        case networks = "Networks", ports = "Ports"
    }

    enum EndpointKeys: String, CodingKey {
        case ipAddress = "IPAddress", gateway = "Gateway", macAddress = "MacAddress"
    }

    enum HostPortKeys: String, CodingKey {
        case hostIP = "HostIp", hostPort = "HostPort"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        let rawName: String = c.lenient(.name, default: "")
        name = rawName.hasPrefix("/") ? String(rawName.dropFirst()) : rawName
        created = c.lenient(.created, default: "")
        path = c.lenient(.path, default: "")
        args = c.lenient(.args, default: [])
        state = try c.decode(State.self, forKey: .state)
        image = c.lenient(.image, default: "")
        restartCount = Int(c.flexibleInt(.restartCount) ?? 0)
        mounts = c.lenient(.mounts, default: [])

        if let config = try? c.nestedContainer(keyedBy: ConfigKeys.self, forKey: .config) {
            hostname = config.lenient(.hostname, default: "")
            environment = config.lenient(.env, default: [])
            labels = config.lenient(.labels, default: [:])
            tty = config.lenient(.tty, default: false)
            workingDir = config.lenient(.workingDir, default: "")
            imageName = config.lenient(.image, default: "")
        } else {
            hostname = ""
            environment = []
            labels = [:]
            tty = false
            workingDir = ""
            imageName = ""
        }

        if let hostConfig = try? c.nestedContainer(keyedBy: HostConfigKeys.self, forKey: .hostConfig) {
            if let policy = try? hostConfig.nestedContainer(keyedBy: RestartPolicyKeys.self, forKey: .restartPolicy) {
                restartPolicy = policy.lenient(.name, default: "")
            } else {
                restartPolicy = ""
            }
            memoryLimit = hostConfig.flexibleInt(.memory) ?? 0
            nanoCPUs = hostConfig.flexibleInt(.nanoCPUs) ?? 0
        } else {
            restartPolicy = ""
            memoryLimit = 0
            nanoCPUs = 0
        }

        var networks: [NetworkAttachment] = []
        var portBindings: [String: [String]] = [:]
        if let settings = try? c.nestedContainer(keyedBy: NetworkSettingsKeys.self, forKey: .networkSettings) {
            if let networkMap = try? settings.nestedContainer(keyedBy: AnyCodingKey.self, forKey: .networks) {
                for key in networkMap.allKeys {
                    guard let endpoint = try? networkMap.nestedContainer(keyedBy: EndpointKeys.self, forKey: key) else {
                        continue
                    }
                    networks.append(NetworkAttachment(
                        name: key.stringValue,
                        ipAddress: endpoint.lenient(.ipAddress, default: ""),
                        gateway: endpoint.lenient(.gateway, default: ""),
                        macAddress: endpoint.lenient(.macAddress, default: "")
                    ))
                }
            }
            if let portMap = try? settings.nestedContainer(keyedBy: AnyCodingKey.self, forKey: .ports) {
                for key in portMap.allKeys {
                    var bindings: [String] = []
                    if var list = try? portMap.nestedUnkeyedContainer(forKey: key) {
                        while !list.isAtEnd {
                            guard let binding = try? list.nestedContainer(keyedBy: HostPortKeys.self) else { break }
                            let hostIP: String = binding.lenient(.hostIP, default: "")
                            let hostPort: String = binding.lenient(.hostPort, default: "")
                            bindings.append(hostIP.isEmpty ? hostPort : "\(hostIP):\(hostPort)")
                        }
                    }
                    portBindings[key.stringValue] = bindings
                }
            }
        }
        self.networks = networks.sorted { $0.name < $1.name }
        self.portBindings = portBindings
    }

    /// The command line as Docker would show it.
    public var commandLine: String {
        ([path] + args).filter { !$0.isEmpty }.joined(separator: " ")
    }
}
