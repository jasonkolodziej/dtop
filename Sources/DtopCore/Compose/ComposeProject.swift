/// A Compose service: one or more replica containers sharing a service label.
public struct ComposeService: Sendable, Equatable, Identifiable {
    public var project: String
    public var name: String
    public var containers: [Container]

    public init(project: String, name: String, containers: [Container]) {
        self.project = project
        self.name = name
        self.containers = containers
    }

    public var id: String { "\(project)/\(name)" }
    public var runningCount: Int { containers.filter { $0.state == .running }.count }

    /// Worst state across replicas so a single crashing replica is visible at the service level.
    public var aggregateStatus: ComposeStatus { ComposeStatus.aggregate(containers) }
}

/// A Compose project reconstructed from container labels (the Engine API has no project object).
public struct ComposeProject: Sendable, Equatable, Identifiable {
    public var name: String
    public var workingDirectory: String?
    public var configFiles: [String]
    public var services: [ComposeService]

    public init(name: String, workingDirectory: String? = nil, configFiles: [String] = [], services: [ComposeService]) {
        self.name = name
        self.workingDirectory = workingDirectory
        self.configFiles = configFiles
        self.services = services
    }

    public var id: String { name }
    public var containers: [Container] { services.flatMap(\.containers) }
    public var runningCount: Int { containers.filter { $0.state == .running }.count }
    public var aggregateStatus: ComposeStatus { ComposeStatus.aggregate(containers) }

    /// Groups containers into projects and services, returning standalone containers separately.
    /// Projects, services and replicas are sorted for a stable display order.
    public static func group(_ containers: [Container]) -> (projects: [ComposeProject], standalone: [Container]) {
        var byProject: [String: [Container]] = [:]
        var standalone: [Container] = []
        for container in containers {
            if let project = container.composeProject, !project.isEmpty {
                byProject[project, default: []].append(container)
            } else {
                standalone.append(container)
            }
        }

        let projects = byProject.keys.sorted().map { name -> ComposeProject in
            let members = byProject[name] ?? []
            var byService: [String: [Container]] = [:]
            for container in members {
                byService[container.composeService ?? "(unknown)", default: []].append(container)
            }
            let services = byService.keys.sorted().map { serviceName in
                ComposeService(
                    project: name,
                    name: serviceName,
                    containers: (byService[serviceName] ?? []).sorted(by: replicaOrder)
                )
            }
            // Different containers may have been created from different directories after a move;
            // prefer the most common value so a single stale container does not win.
            let workingDirectory = mostCommon(members.compactMap(\.composeWorkingDirectory))
            let configFiles = members.first { $0.composeWorkingDirectory == workingDirectory }?.composeConfigFiles
                ?? members.first?.composeConfigFiles ?? []
            return ComposeProject(
                name: name,
                workingDirectory: workingDirectory,
                configFiles: configFiles,
                services: services
            )
        }
        return (projects, standalone.sorted { $0.name < $1.name })
    }

    private static func replicaOrder(_ lhs: Container, _ rhs: Container) -> Bool {
        let left = lhs.composeContainerNumber ?? Int.max
        let right = rhs.composeContainerNumber ?? Int.max
        return left == right ? lhs.name < rhs.name : left < right
    }

    private static func mostCommon(_ values: [String]) -> String? {
        var counts: [String: Int] = [:]
        for value in values { counts[value, default: 0] += 1 }
        return counts.max { lhs, rhs in
            lhs.value == rhs.value ? lhs.key > rhs.key : lhs.value < rhs.value
        }?.key
    }
}

/// Roll-up status for a set of containers.
public enum ComposeStatus: String, Sendable, Equatable {
    case running
    case partial
    case degraded
    case paused
    case stopped
    case empty

    public static func aggregate(_ containers: [Container]) -> ComposeStatus {
        guard !containers.isEmpty else { return .empty }
        let running = containers.filter { $0.state == .running }
        if containers.contains(where: { $0.state == .restarting || $0.state == .dead || $0.health == .unhealthy }) {
            return .degraded
        }
        if running.count == containers.count { return .running }
        if containers.allSatisfy({ $0.state == .paused }) { return .paused }
        if running.isEmpty && !containers.contains(where: { $0.state == .paused }) { return .stopped }
        return .partial
    }
}
