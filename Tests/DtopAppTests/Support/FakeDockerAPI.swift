import Foundation
@testable import DtopApp
@testable import DtopCore

/// Scriptable in-memory Docker API that records every call.
final class FakeDockerAPI: DockerAPI, @unchecked Sendable {
    private let lock = NSLock()
    private var _containers: [Container] = []
    private var _stats: [String: [ContainerStats]] = [:]
    private var _calls: [String] = []
    private var _failure: DockerError?
    private var _inventoryFailure: DockerError?
    private var _operationFailures: [String: DockerError] = [:]

    var images: [DockerImage] = [DockerImage(id: "sha256:aaa", repoTags: ["nginx:latest"])]
    var networks: [DockerNetwork] = [DockerNetwork(id: "n1", name: "bridge")]
    var volumes: [DockerVolume] = [DockerVolume(name: "data")]
    var info = SystemInfo(name: "host", serverVersion: "27.0", cpuCount: 4, memoryTotal: 8_000_000_000)
    var top = ContainerTop(titles: ["PID", "PPID", "CMD"], processes: [["1", "0", "init"], ["2", "1", "worker"]])
    var logLines = [LogLine(stream: .stdout, text: "hello"), LogLine(stream: .stderr, text: "warn")]

    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try body()
    }

    var containers: [Container] {
        get { locked { _containers } }
        set { locked { _containers = newValue } }
    }

    var calls: [String] { locked { _calls } }

    /// When set, every call fails with this error (simulates a dead daemon).
    var failure: DockerError? {
        get { locked { _failure } }
        set { locked { _failure = newValue } }
    }

    /// When set, images/networks/volumes/info fail but containers work.
    var inventoryFailure: DockerError? {
        get { locked { _inventoryFailure } }
        set { locked { _inventoryFailure = newValue } }
    }

    func failOperation(on containerID: String, with error: DockerError) {
        locked { _operationFailures[containerID] = error }
    }

    /// Queues stats payloads returned in order (the last one repeats).
    func setStats(_ id: String, _ sequence: [ContainerStats]) {
        locked { _stats[id] = sequence }
    }

    private func record(_ call: String) throws {
        try locked {
            _calls.append(call)
            if let failure = _failure { throw failure }
        }
    }

    private func recordInventory(_ call: String) throws {
        try record(call)
        if let failure = inventoryFailure { throw failure }
    }

    func ping() async throws -> APIVersion? {
        try record("ping")
        return APIVersion(major: 1, minor: 45)
    }

    func version() async throws -> DockerVersion {
        try record("version")
        return DockerVersion(version: "27.0.0", apiVersion: "1.45")
    }

    func info() async throws -> SystemInfo {
        try recordInventory("info")
        return info
    }

    func listContainers(all: Bool) async throws -> [Container] {
        try record("listContainers")
        return containers
    }

    func inspectContainer(id: String) async throws -> ContainerDetails {
        try record("inspect \(id)")
        let json = #"{"Id":"\#(id)","Name":"/\#(id)","Path":"sh","Args":["-c","run"],"State":{"Status":"running","Running":true},"RestartCount":1}"#
        return try JSONDecoder().decode(ContainerDetails.self, from: Data(json.utf8))
    }

    func stats(id: String) async throws -> ContainerStats {
        try record("stats \(id)")
        return locked {
            guard var sequence = _stats[id], !sequence.isEmpty else {
                return ContainerStats(cpu: .init(usage: .init(totalUsage: 0)))
            }
            let next = sequence.removeFirst()
            _stats[id] = sequence.isEmpty ? [next] : sequence
            return next
        }
    }

    func top(id: String) async throws -> ContainerTop {
        try record("top \(id)")
        return top
    }

    func logs(id: String, tail: Int) async throws -> [LogLine] {
        try record("logs \(id) \(tail)")
        return logLines
    }

    func perform(_ action: ContainerAction, containerID: String) async throws {
        try record("\(action.verb) \(containerID)")
        if let error = locked({ _operationFailures[containerID] }) { throw error }
    }

    func removeContainer(id: String, force: Bool, removeVolumes: Bool) async throws {
        try record("rm \(id) force=\(force)")
    }

    func pruneContainers() async throws -> PruneReport {
        try record("prune containers")
        return PruneReport(deleted: ["a", "b"], spaceReclaimed: 2048)
    }

    func listImages() async throws -> [DockerImage] {
        try recordInventory("listImages")
        return images
    }

    func removeImage(id: String, force: Bool) async throws {
        try record("rmi \(id)")
    }

    func pruneImages(danglingOnly: Bool) async throws -> PruneReport {
        try record("prune images dangling=\(danglingOnly)")
        return PruneReport(deleted: [], spaceReclaimed: 0)
    }

    func listNetworks() async throws -> [DockerNetwork] {
        try recordInventory("listNetworks")
        return networks
    }

    func removeNetwork(id: String) async throws {
        try record("rm network \(id)")
    }

    func pruneNetworks() async throws -> PruneReport {
        try record("prune networks")
        return PruneReport(deleted: ["n"], spaceReclaimed: 0)
    }

    func listVolumes() async throws -> [DockerVolume] {
        try recordInventory("listVolumes")
        return volumes
    }

    func removeVolume(name: String, force: Bool) async throws {
        try record("rm volume \(name)")
    }

    func pruneVolumes() async throws -> PruneReport {
        try record("prune volumes")
        return PruneReport(deleted: [], spaceReclaimed: 0)
    }

    func events() -> AsyncThrowingStream<DockerEvent, Error> {
        AsyncThrowingStream { $0.finish() }
    }
}

/// Records invocations instead of spawning processes.
final class FakeCommandRunner: CommandRunning, @unchecked Sendable {
    private let lock = NSLock()
    private var _invocations: [CommandInvocation] = []
    var result = CommandResult(exitCode: 0, output: "ok")

    var invocations: [CommandInvocation] {
        lock.lock()
        defer { lock.unlock() }
        return _invocations
    }

    private func append(_ invocation: CommandInvocation) {
        lock.lock()
        _invocations.append(invocation)
        lock.unlock()
    }

    func run(_ invocation: CommandInvocation) async throws -> CommandResult {
        append(invocation)
        return result
    }
}

/// A clock tests can advance.
final class TestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Double

    init(_ start: Double = 1_000) {
        value = start
    }

    var now: Double {
        lock.lock()
        defer { lock.unlock() }
        return value
    }

    func advance(_ seconds: Double) {
        lock.lock()
        value += seconds
        lock.unlock()
    }
}

enum AppFixtures {
    static func container(
        _ name: String,
        state: ContainerState = .running,
        project: String? = nil,
        service: String? = nil,
        image: String = "alpine:3",
        created: Int64 = 0,
        status: String = "Up 1 minute"
    ) -> Container {
        var labels: [String: String] = [:]
        if let project {
            labels[ComposeLabel.project] = project
            labels[ComposeLabel.workingDir] = "/srv/\(project)"
            labels[ComposeLabel.configFiles] = "/srv/\(project)/compose.yaml"
        }
        if let service { labels[ComposeLabel.service] = service }
        return Container(id: "id-" + name, names: ["/" + name], image: image, imageID: "sha256:aaa", created: created,
                         labels: labels, state: state, status: status)
    }

    static func stats(read: String, total: UInt64, system: UInt64, memory: UInt64 = 1_048_576) -> ContainerStats {
        ContainerStats(
            read: read,
            cpu: .init(usage: .init(totalUsage: total), systemUsage: system, onlineCPUs: 2),
            memory: .init(usage: memory, limit: 10_485_760),
            networks: ["eth0": .init(rxBytes: total, txBytes: total / 2)]
        )
    }

    /// A connected snapshot with a compose project, a standalone container and metrics.
    static func snapshot() -> DockerSnapshot {
        let containers = [
            container("web-1", project: "shop", service: "web", image: "nginx", created: 30),
            container("db-1", state: .exited, project: "shop", service: "db", image: "postgres", created: 20,
                      status: "Exited (0) 1 hour ago"),
            container("cache", image: "redis", created: 10),
            container("old", state: .exited, image: "busybox", created: 5, status: "Exited (1) 2 days ago")
        ]
        var metrics = MetricsStore(capacity: 10)
        metrics.record([
            "id-web-1": ResourceSample(cpuPercent: 12.5, cpuCount: 4, memoryUsage: 50_000_000, memoryLimit: 100_000_000),
            "id-cache": ResourceSample(cpuPercent: 40, cpuCount: 4, memoryUsage: 10_000_000, memoryLimit: 100_000_000)
        ], timestamp: 1)
        return DockerSnapshot(
            endpoint: "unix:///var/run/docker.sock",
            connection: .connected,
            version: DockerVersion(version: "27.0.0", apiVersion: "1.45"),
            info: SystemInfo(name: "host", cpuCount: 4, memoryTotal: 8_000_000_000),
            containers: containers,
            images: [
                DockerImage(id: "sha256:bbb", repoTags: ["redis:7"], created: 100, size: 2_000),
                DockerImage(id: "sha256:aaa", repoTags: ["nginx:latest"], created: 200, size: 1_000),
                DockerImage(id: "sha256:ccc", created: 50, size: 3_000)
            ],
            networks: [DockerNetwork(id: "n2", name: "shop_default"), DockerNetwork(id: "n1", name: "bridge")],
            volumes: [
                DockerVolume(name: String(repeating: "a", count: 64)),
                DockerVolume(name: "shop_data", labels: [ComposeLabel.project: "shop"])
            ],
            metrics: metrics
        )
    }
}
