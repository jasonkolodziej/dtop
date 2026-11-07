import Foundation
import Testing
@testable import DtopCore

/// Exercises a real Docker daemon. Opt in with `DTOP_INTEGRATION=1 swift test --filter Integration`.
/// Tests create their own labelled containers and remove them afterwards.
enum IntegrationEnvironment {
    static var isEnabled: Bool { ProcessInfo.processInfo.environment["DTOP_INTEGRATION"] == "1" }

    static func client() async throws -> DockerClient {
        let environment = ProcessInfo.processInfo.environment
        let endpoint = try DockerEndpoint.resolve(
            environment: environment,
            homeDirectory: FileManager.default.homeDirectoryForCurrentUser.path,
            fileExists: { FileManager.default.fileExists(atPath: $0) }
        )
        return try await DockerClient.make(endpoint: endpoint).negotiated()
    }

    /// Creates and starts a throwaway container via the Engine API (pulling the image if needed).
    static func startContainer(_ client: DockerClient, name: String, command: [String]) async throws -> String {
        let image = "alpine:3.20"
        if try await !client.listImages().contains(where: { $0.repoTags.contains(image) }) {
            let pull = client.makeRequest(.post, "/images/create", query: [("fromImage", "alpine"), ("tag", "3.20")])
            _ = try await client.execute(pull)
        }
        let body = try JSONSerialization.data(withJSONObject: [
            "Image": image, "Cmd": command, "Labels": ["dtop.test": "1"]
        ])
        let create = client.makeRequest(.post, "/containers/create", query: [("name", name)], body: Array(body))
        let response = try await client.execute(create)
        struct Created: Decodable { let Id: String }
        let id = try client.decode(Created.self, from: response).Id
        try await client.perform(.start, containerID: id)
        return id
    }
}

@Suite("Integration: real Docker daemon", .enabled(if: IntegrationEnvironment.isEnabled), .serialized)
struct IntegrationTests {
    @Test("pings, negotiates and reads version/info")
    func system() async throws {
        let client = try await IntegrationEnvironment.client()
        #expect(client.apiVersion != nil)
        let version = try await client.version()
        #expect(!version.version.isEmpty)
        let info = try await client.info()
        #expect(info.cpuCount > 0)
        #expect(info.memoryTotal > 0)
    }

    @Test("lists inventory without decoding errors")
    func inventory() async throws {
        let client = try await IntegrationEnvironment.client()
        _ = try await client.listContainers(all: true)
        _ = try await client.listImages()
        #expect(try await client.listNetworks().contains { $0.name == "bridge" || $0.name == "nat" })
        _ = try await client.listVolumes()
    }

    @Test("container lifecycle: stats, top, logs, pause, stop, remove")
    func lifecycle() async throws {
        let client = try await IntegrationEnvironment.client()
        let name = "dtop-it-\(UUID().uuidString.prefix(8).lowercased())"
        let id = try await IntegrationEnvironment.startContainer(
            client, name: name, command: ["sh", "-c", "echo ready; echo oops >&2; while true; do sleep 1; done"])
        do {
            let listed = try await client.listContainers(all: true).first { $0.id == id }
            #expect(listed?.name == name)
            #expect(listed?.state == .running)

            let first = try await client.stats(id: id)
            try await Task.sleep(nanoseconds: 1_100_000_000)
            let second = try await client.stats(id: id)
            let sample = StatsCalculator.sample(current: second, previous: first, fallbackTimestamp: 0)
            #expect(sample.memoryUsage > 0)
            #expect(sample.cpuPercent >= 0)

            let processes = ProcessTree.build(ProcessTree.parse(try await client.top(id: id)))
            #expect(!processes.isEmpty)
            #expect(processes.contains { $0.process.command.contains("sh") })

            let logs = try await client.logs(id: id, tail: 10)
            #expect(logs.contains(LogLine(stream: .stdout, text: "ready")))
            #expect(logs.contains(LogLine(stream: .stderr, text: "oops")))

            let details = try await client.inspectContainer(id: id)
            #expect(details.name == name)
            #expect(details.state.running)

            try await client.perform(.pause, containerID: id)
            #expect(try await client.inspectContainer(id: id).state.paused)
            try await client.perform(.unpause, containerID: id)
            try await client.perform(.stop(timeoutSeconds: 1), containerID: id)
            #expect(try await client.inspectContainer(id: id).state.status == .exited)
            // Starting an already-stopped container twice exercises 304 handling.
            try await client.perform(.stop(timeoutSeconds: 1), containerID: id)
        } catch {
            try? await client.removeContainer(id: id, force: true, removeVolumes: true)
            throw error
        }
        try await client.removeContainer(id: id, force: true, removeVolumes: true)
        await #expect(throws: DockerError.self) { _ = try await client.inspectContainer(id: id) }
    }

    @Test("the event stream reports container starts")
    func events() async throws {
        let client = try await IntegrationEnvironment.client()
        let name = "dtop-ev-\(UUID().uuidString.prefix(8).lowercased())"
        let stream = client.events()
        let watcher = Task { () -> Bool in
            for try await event in stream where event.type == "container" && event.action == "start"
                && event.attributes["name"] == name {
                return true
            }
            return false
        }
        try await Task.sleep(nanoseconds: 300_000_000)
        let id = try await IntegrationEnvironment.startContainer(client, name: name, command: ["sleep", "5"])
        let timeout = Task {
            try await Task.sleep(nanoseconds: 10_000_000_000)
            watcher.cancel()
        }
        let sawStart = (try? await watcher.value) ?? false
        timeout.cancel()
        try? await client.removeContainer(id: id, force: true, removeVolumes: true)
        #expect(sawStart)
    }
}
