import DtopCore

public enum PruneTarget: String, Sendable, Equatable, CaseIterable {
    case containers
    case images
    case networks
    case volumes
}

/// A management action the user asked for. Destructive ones go through a confirmation dialog.
public enum DockerOperation: Sendable, Equatable {
    case container(ContainerAction, id: String, name: String)
    case removeContainer(id: String, name: String, force: Bool)
    case compose(ComposeOperation, project: String)
    case removeImage(id: String, name: String, force: Bool)
    case removeNetwork(id: String, name: String)
    case removeVolume(name: String)
    case prune(PruneTarget)

    public var isDestructive: Bool {
        switch self {
        case .container(let action, _, _):
            if case .kill = action { return true }
            return false
        case .compose(let operation, _):
            return operation.isDestructive
        case .removeContainer, .removeImage, .removeNetwork, .removeVolume, .prune:
            return true
        }
    }

    public var confirmationTitle: String {
        switch self {
        case .container(let action, _, _): return "\(action.verb.capitalizedFirst) container?"
        case .removeContainer: return "Remove container?"
        case .compose(let operation, _): return "Compose \(operation.rawValue)?"
        case .removeImage: return "Remove image?"
        case .removeNetwork: return "Remove network?"
        case .removeVolume: return "Remove volume?"
        case .prune(let target): return "Prune \(target.rawValue)?"
        }
    }

    public var confirmationMessage: String {
        switch self {
        case .container(.kill, _, let name):
            return "Send SIGKILL to '\(name)'? The process gets no chance to shut down cleanly."
        case .container(let action, _, let name):
            return "\(action.verb.capitalizedFirst) '\(name)'?"
        case .removeContainer(_, let name, let force):
            return force
                ? "Force-remove '\(name)'? It will be killed if running. Its anonymous volumes are kept."
                : "Remove '\(name)'? Its anonymous volumes are kept."
        case .compose(.down, let project):
            return "Run 'docker compose down' for '\(project)'? Containers and networks of the project are removed; named volumes are kept."
        case .compose(let operation, let project):
            return "Run compose \(operation.rawValue) for '\(project)'?"
        case .removeImage(_, let name, let force):
            return force ? "Force-remove image '\(name)' even if containers use it?" : "Remove image '\(name)'?"
        case .removeNetwork(_, let name):
            return "Remove network '\(name)'?"
        case .removeVolume(let name):
            return "Remove volume '\(name)'? Its data is deleted permanently."
        case .prune(.containers):
            return "Remove all stopped containers?"
        case .prune(.images):
            return "Remove all dangling (untagged) images?"
        case .prune(.networks):
            return "Remove all networks not used by any container?"
        case .prune(.volumes):
            return "Remove all unused anonymous volumes? Their data is deleted permanently."
        }
    }

    /// Present-progressive description shown while the operation runs.
    public var progressDescription: String {
        switch self {
        case .container(let action, _, let name): return "\(action.progressVerb) \(name)…"
        case .removeContainer(_, let name, _): return "Removing \(name)…"
        case .compose(let operation, let project): return "compose \(operation.rawValue) \(project)…"
        case .removeImage(_, let name, _): return "Removing image \(name)…"
        case .removeNetwork(_, let name): return "Removing network \(name)…"
        case .removeVolume(let name): return "Removing volume \(name)…"
        case .prune(let target): return "Pruning \(target.rawValue)…"
        }
    }

    public var successDescription: String {
        switch self {
        case .container(let action, _, let name): return "\(action.pastVerb) \(name)"
        case .removeContainer(_, let name, _): return "Removed \(name)"
        case .compose(let operation, let project): return "compose \(operation.rawValue) \(project) finished"
        case .removeImage(_, let name, _): return "Removed image \(name)"
        case .removeNetwork(_, let name): return "Removed network \(name)"
        case .removeVolume(let name): return "Removed volume \(name)"
        case .prune(let target): return "Pruned \(target.rawValue)"
        }
    }

    /// Whether the change affects images/networks/volumes lists (so they should refresh too).
    public var touchesInventory: Bool {
        switch self {
        case .container: return false
        default: return true
        }
    }
}

public struct OperationResult: Sendable, Equatable {
    public var operation: DockerOperation
    public var succeeded: Bool
    public var message: String

    public init(operation: DockerOperation, succeeded: Bool, message: String) {
        self.operation = operation
        self.succeeded = succeeded
        self.message = message
    }
}

extension ContainerAction {
    var progressVerb: String {
        switch self {
        case .start: return "Starting"
        case .stop: return "Stopping"
        case .restart: return "Restarting"
        case .kill: return "Killing"
        case .pause: return "Pausing"
        case .unpause: return "Resuming"
        }
    }

    var pastVerb: String {
        switch self {
        case .start: return "Started"
        case .stop: return "Stopped"
        case .restart: return "Restarted"
        case .kill: return "Killed"
        case .pause: return "Paused"
        case .unpause: return "Resumed"
        }
    }
}

extension String {
    var capitalizedFirst: String {
        guard let first else { return self }
        return first.uppercased() + dropFirst()
    }
}
