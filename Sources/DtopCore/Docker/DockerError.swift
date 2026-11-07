/// Errors surfaced by the Docker client. Every description is written to be actionable for the
/// person staring at the terminal: what failed, and what they can do about it.
public enum DockerError: Error, Equatable, Sendable, CustomStringConvertible {
    case invalidEndpoint(String)
    case unsupportedEndpoint(String)
    case connectionFailed(endpoint: String, reason: String)
    case socketPathTooLong(String)
    case io(String)
    case malformedResponse(String)
    case http(status: Int, message: String)
    case decoding(String)
    case cancelled
    case timedOut(String)

    public var description: String {
        switch self {
        case .invalidEndpoint(let value):
            return "Invalid Docker endpoint '\(value)' — use unix:///path/to/docker.sock, "
                + "npipe:////./pipe/docker_engine or tcp://host:port (via --host or DOCKER_HOST)"
        case .unsupportedEndpoint(let value):
            return "Unsupported Docker endpoint '\(value)' — dtop speaks plain HTTP over unix sockets, "
                + "named pipes and tcp; for ssh:// or TLS endpoints, forward the socket locally first"
        case .connectionFailed(let endpoint, let reason):
            return "Cannot connect to Docker at \(endpoint) (\(reason)) — is the Docker daemon running? "
                + "Set DOCKER_HOST or pass --host to point dtop at another endpoint"
        case .socketPathTooLong(let path):
            return "Unix socket path is too long (\(path.utf8.count) bytes): \(path) — "
                + "use a shorter path or a symlink to the socket"
        case .io(let reason):
            return "I/O error while talking to Docker: \(reason)"
        case .malformedResponse(let reason):
            return "Docker returned a response dtop could not parse: \(reason)"
        case .http(let status, let message):
            return "Docker API error \(status): \(message)"
        case .decoding(let reason):
            return "Could not decode Docker API response: \(reason) — your Docker version may be unsupported"
        case .cancelled:
            return "Request was cancelled"
        case .timedOut(let operation):
            return "Timed out waiting for Docker during \(operation) — the daemon may be overloaded"
        }
    }

    /// Whether retrying the same request later could reasonably succeed.
    public var isTransient: Bool {
        switch self {
        case .connectionFailed, .io, .timedOut:
            return true
        case .http(let status, _):
            return status >= 500
        default:
            return false
        }
    }

    public var isNotFound: Bool {
        if case .http(404, _) = self { return true }
        return false
    }
}
