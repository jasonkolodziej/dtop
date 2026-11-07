/// Lifecycle operations on a single container.
public enum ContainerAction: Sendable, Equatable {
    case start
    case stop(timeoutSeconds: Int?)
    case restart(timeoutSeconds: Int?)
    case kill(signal: String?)
    case pause
    case unpause

    public var verb: String {
        switch self {
        case .start: return "start"
        case .stop: return "stop"
        case .restart: return "restart"
        case .kill: return "kill"
        case .pause: return "pause"
        case .unpause: return "unpause"
        }
    }

    var query: [(String, String?)] {
        switch self {
        case .stop(let timeout), .restart(let timeout):
            return [("t", timeout.map(String.init))]
        case .kill(let signal):
            return [("signal", signal)]
        default:
            return []
        }
    }
}

/// A Docker API version such as `1.43`, comparable numerically.
public struct APIVersion: Sendable, Comparable, CustomStringConvertible {
    public var major: Int
    public var minor: Int

    public init(major: Int, minor: Int) {
        self.major = major
        self.minor = minor
    }

    public init?(_ text: String) {
        let parts = text.trimmingWhitespace().split(separator: ".")
        guard parts.count == 2, let major = Int(parts[0]), let minor = Int(parts[1]) else { return nil }
        self.init(major: major, minor: minor)
    }

    public var description: String { "\(major).\(minor)" }

    public static func < (lhs: APIVersion, rhs: APIVersion) -> Bool {
        (lhs.major, lhs.minor) < (rhs.major, rhs.minor)
    }

    /// Newest API version dtop has been written against.
    public static let maximumSupported = APIVersion(major: 1, minor: 47)
    /// `stats?one-shot=true` arrived in 1.41.
    public static let oneShotStats = APIVersion(major: 1, minor: 41)

    /// Picks the version to speak: the daemon's, capped at what dtop understands.
    public static func negotiate(server: APIVersion) -> APIVersion {
        min(server, maximumSupported)
    }
}

/// Everything dtop needs from the Docker Engine. `DockerClient` is the real implementation;
/// tests substitute fakes.
public protocol DockerAPI: Sendable {
    func ping() async throws -> APIVersion?
    func version() async throws -> DockerVersion
    func info() async throws -> SystemInfo

    func listContainers(all: Bool) async throws -> [Container]
    func inspectContainer(id: String) async throws -> ContainerDetails
    func stats(id: String) async throws -> ContainerStats
    func top(id: String) async throws -> ContainerTop
    func logs(id: String, tail: Int) async throws -> [LogLine]
    /// The last `tail` lines, then every new line as the container writes it (`logs -f`).
    /// Yields batches as they arrive; finishes when the container stops or the stream is cancelled.
    func followLogs(id: String, tail: Int) -> AsyncThrowingStream<[LogLine], Error>
    func perform(_ action: ContainerAction, containerID: String) async throws
    func removeContainer(id: String, force: Bool, removeVolumes: Bool) async throws
    func pruneContainers() async throws -> PruneReport

    func listImages() async throws -> [DockerImage]
    func removeImage(id: String, force: Bool) async throws
    func pruneImages(danglingOnly: Bool) async throws -> PruneReport

    func listNetworks() async throws -> [DockerNetwork]
    func removeNetwork(id: String) async throws
    func pruneNetworks() async throws -> PruneReport

    func listVolumes() async throws -> [DockerVolume]
    func removeVolume(name: String, force: Bool) async throws
    func pruneVolumes() async throws -> PruneReport

    func events() -> AsyncThrowingStream<DockerEvent, Error>
}

extension DockerAPI {
    /// Fallback for implementations without streaming: one fetch, then the stream ends.
    public func followLogs(id: String, tail: Int) -> AsyncThrowingStream<[LogLine], Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    continuation.yield(try await logs(id: id, tail: tail))
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
