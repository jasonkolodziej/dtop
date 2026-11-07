import Foundation
import Testing
@testable import DtopCore

/// Routes requests by method + path so one mock can serve a whole client session.
func router(_ routes: [String: MockResponse], fallback: MockResponse = .error(404, message: "no route")) -> @Sendable (RecordedRequest) -> MockResponse {
    { request in routes["\(request.method) \(request.path)"] ?? fallback }
}

@Suite("Docker client requests")
struct DockerClientRequestTests {
    @Test("lists all containers and decodes them")
    func listContainers() async throws {
        let transport = MockTransport(handler: router(["GET /containers/json": .json(Fixtures.containerList)]))
        let client = DockerClient(transport: transport)
        let containers = try await client.listContainers(all: true)
        #expect(containers.count == 2)
        #expect(containers[0].name == "web-1")
        #expect(transport.requests.first?.query["all"] == "1")
        _ = try await client.listContainers(all: false)
        #expect(transport.requests.last?.query["all"] == "0")
    }

    @Test("prefixes paths with the negotiated API version")
    func versionPrefix() async throws {
        let transport = MockTransport(handler: router(["GET /v1.43/containers/json": .json("[]")]))
        let client = DockerClient(transport: transport, apiVersion: APIVersion(major: 1, minor: 43))
        #expect(try await client.listContainers(all: true).isEmpty)
        #expect(transport.requests.first?.path == "/v1.43/containers/json")
    }

    @Test("negotiation pings, reads API-Version and caps at the supported maximum")
    func negotiation() async throws {
        let transport = MockTransport(handler: router([
            "GET /_ping": .http(headers: [("API-Version", "1.99")], body: "OK")
        ]))
        let negotiated = try await DockerClient(transport: transport).negotiated()
        #expect(negotiated.apiVersion == APIVersion.maximumSupported)

        let older = MockTransport(handler: router(["GET /_ping": .http(headers: [("Api-Version", "1.40")], body: "OK")]))
        #expect(try await DockerClient(transport: older).negotiated().apiVersion == APIVersion(major: 1, minor: 40))

        let unknown = MockTransport(handler: router(["GET /_ping": .http(body: "OK")]))
        #expect(try await DockerClient(transport: unknown).negotiated().apiVersion == nil)
    }

    @Test("stats requests one-shot snapshots when the API supports it")
    func statsQuery() async throws {
        let stats = Fixtures.linuxStats(read: "2024-05-01T10:00:00Z", total: 100, system: 1000)
        let transport = MockTransport(handler: { _ in .json(stats) })
        _ = try await DockerClient(transport: transport).stats(id: "abc")
        #expect(transport.requests[0].path == "/containers/abc/stats")
        #expect(transport.requests[0].query["stream"] == "false")
        #expect(transport.requests[0].query["one-shot"] == "true")

        _ = try await DockerClient(transport: transport, apiVersion: APIVersion(major: 1, minor: 40)).stats(id: "abc")
        #expect(transport.requests[1].query["one-shot"] == nil)
    }

    @Test("container actions map to POST endpoints with their parameters")
    func actions() async throws {
        let transport = MockTransport(always: .http(status: 204, reason: "No Content"))
        let client = DockerClient(transport: transport)
        try await client.perform(.start, containerID: "c1")
        try await client.perform(.stop(timeoutSeconds: 5), containerID: "c1")
        try await client.perform(.restart(timeoutSeconds: nil), containerID: "c1")
        try await client.perform(.kill(signal: "SIGTERM"), containerID: "c1")
        try await client.perform(.pause, containerID: "c1")
        try await client.perform(.unpause, containerID: "c1")
        let requests = transport.requests
        #expect(requests.map(\.method) == Array(repeating: "POST", count: 6))
        #expect(requests.map(\.path) == ["start", "stop", "restart", "kill", "pause", "unpause"].map { "/containers/c1/\($0)" })
        #expect(requests[1].query["t"] == "5")
        #expect(requests[2].query["t"] == nil)
        #expect(requests[3].query["signal"] == "SIGTERM")
        #expect(requests.allSatisfy { $0.headers["content-length"] == "0" })
    }

    @Test("304 Not Modified (already started/stopped) is not an error")
    func notModified() async throws {
        let transport = MockTransport(always: .http(status: 304, reason: "Not Modified"))
        try await DockerClient(transport: transport).perform(.start, containerID: "c1")
    }

    @Test("removal endpoints use DELETE with force flags")
    func removals() async throws {
        let transport = MockTransport(always: .http(status: 204, reason: "No Content"))
        let client = DockerClient(transport: transport)
        try await client.removeContainer(id: "c1", force: true, removeVolumes: false)
        try await client.removeImage(id: "sha256:abc", force: false)
        try await client.removeImage(id: "library/nginx:latest", force: true)
        try await client.removeNetwork(id: "n1")
        try await client.removeVolume(name: "my vol", force: true)
        let requests = transport.requests
        #expect(requests.allSatisfy { $0.method == "DELETE" })
        #expect(requests[0].target == "/containers/c1?force=1&v=0")
        #expect(requests[1].target == "/images/sha256:abc?force=0")
        #expect(requests[2].target == "/images/library/nginx:latest?force=1")
        #expect(requests[3].target == "/networks/n1")
        #expect(requests[4].target == "/volumes/my%20vol?force=1")
    }

    @Test("prune endpoints decode reports")
    func prune() async throws {
        let transport = MockTransport(handler: router([
            "POST /containers/prune": .json(#"{"ContainersDeleted":["a","b"],"SpaceReclaimed":1024}"#),
            "POST /images/prune": .json(#"{"ImagesDeleted":[{"Untagged":"x:1"},{"Deleted":"sha256:y"}],"SpaceReclaimed":2048}"#),
            "POST /networks/prune": .json(#"{"NetworksDeleted":null}"#),
            "POST /volumes/prune": .json(#"{"VolumesDeleted":["v"],"SpaceReclaimed":0}"#)
        ]))
        let client = DockerClient(transport: transport)
        #expect(try await client.pruneContainers() == PruneReport(deleted: ["a", "b"], spaceReclaimed: 1024))
        #expect(try await client.pruneImages(danglingOnly: true) == PruneReport(deleted: ["x:1", "sha256:y"], spaceReclaimed: 2048))
        #expect(try await client.pruneNetworks() == PruneReport(deleted: [], spaceReclaimed: 0))
        #expect(try await client.pruneVolumes().deleted == ["v"])
        #expect(transport.requests[1].query["filters"] == #"{"dangling":["true"]}"#)
    }

    @Test("logs are demultiplexed into lines")
    func logs() async throws {
        var body: [UInt8] = []
        body += [1, 0, 0, 0, 0, 0, 0, 6] + Array("hello\n".utf8)
        body += [2, 0, 0, 0, 0, 0, 0, 5] + Array("oops\n".utf8)
        let response = MockResponse(chunks: [MockResponse.httpBytes(status: 200, reason: "OK", headers: [], body: body, chunked: true)])
        let transport = MockTransport(always: response)
        let lines = try await DockerClient(transport: transport).logs(id: "c1", tail: 50)
        #expect(lines == [LogLine(stream: .stdout, text: "hello"), LogLine(stream: .stderr, text: "oops")])
        #expect(transport.requests[0].query == ["stdout": "1", "stderr": "1", "tail": "50"])
    }

    @Test("inventory endpoints decode their payloads")
    func inventory() async throws {
        let transport = MockTransport(handler: router([
            "GET /images/json": .json(Fixtures.images),
            "GET /networks": .json(Fixtures.networks),
            "GET /volumes": .json(Fixtures.volumes),
            "GET /version": .json(Fixtures.version),
            "GET /info": .json(Fixtures.info),
            "GET /containers/abc/json": .json(Fixtures.containerInspect),
            "GET /containers/abc/top": .json(Fixtures.top)
        ]))
        let client = DockerClient(transport: transport)
        #expect(try await client.listImages().count == 3)
        #expect(try await client.listNetworks().count == 2)
        #expect(try await client.listVolumes().count == 2)
        #expect(try await client.version().apiVersion == "1.47")
        #expect(try await client.info().cpuCount == 8)
        #expect(try await client.inspectContainer(id: "abc").name == "web-1")
        #expect(try await client.top(id: "abc").processes.count == 4)
    }

    @Test("daemon error messages surface with their status code")
    func errors() async {
        let transport = MockTransport(always: .error(404, message: "No such container: nope"))
        await #expect(throws: DockerError.http(status: 404, message: "No such container: nope")) {
            _ = try await DockerClient(transport: transport).inspectContainer(id: "nope")
        }
        let plain = MockTransport(always: .http(status: 500, reason: "Internal Server Error", body: "boom"))
        await #expect(throws: DockerError.http(status: 500, message: "boom")) {
            _ = try await DockerClient(transport: plain).listImages()
        }
        let empty = MockTransport(always: .http(status: 409, reason: "Conflict"))
        await #expect(throws: DockerError.http(status: 409, message: "Conflict")) {
            try await DockerClient(transport: empty).removeNetwork(id: "x")
        }
    }

    @Test("undecodable payloads raise decoding errors")
    func decodingError() async {
        let transport = MockTransport(always: .json(#"{"not":"a list"}"#))
        await #expect(throws: DockerError.self) { _ = try await DockerClient(transport: transport).listContainers(all: true) }
    }

    @Test("path segments are encoded but keep : and / for image references")
    func pathSegments() {
        #expect(DockerClient.pathSegment("abc123") == "abc123")
        #expect(DockerClient.pathSegment("sha256:deadbeef") == "sha256:deadbeef")
        #expect(DockerClient.pathSegment("ghcr.io/org/app:1.0") == "ghcr.io/org/app:1.0")
        #expect(DockerClient.pathSegment("app@sha256:x") == "app@sha256:x")
        #expect(DockerClient.pathSegment("a b?c") == "a%20b%3Fc")
    }
}

@Suite("Docker event stream")
struct DockerEventStreamTests {
    @Test("yields decoded events line by line and finishes at EOF")
    func streamsEvents() async throws {
        let lines = [
            #"{"Type":"container","Action":"start","Actor":{"ID":"c1","Attributes":{"name":"web"}},"time":1,"timeNano":1000}"#,
            #"{"Type":"container","Action":"die","Actor":{"ID":"c1","Attributes":{}},"time":2}"#,
            "not json",
            #"{"Type":"image","Action":"pull","Actor":{"ID":"nginx"}}"#
        ]
        let body = lines.joined(separator: "\n") + "\n"
        let transport = MockTransport(always: .http(body: body, chunked: true))
        var received: [DockerEvent] = []
        for try await event in DockerClient(transport: transport).events() {
            received.append(event)
        }
        #expect(received.map(\.action) == ["start", "die", "pull"])
        #expect(received[0].attributes["name"] == "web")
        #expect(transport.requests[0].query["filters"]?.contains("container") == true)
    }

    @Test("cancelling the consumer interrupts the blocked connection")
    func cancellation() async throws {
        let line = Array(#"{"Type":"container","Action":"start","Actor":{"ID":"abcdefabc"}}"#.utf8) + [10]
        let transport = MockTransport(always: MockResponse(chunks: [
            Array("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".utf8),
            Array("\(String(line.count, radix: 16))\r\n".utf8) + line + [13, 10]
        ], hangsAfterChunks: true))
        let stream = DockerClient(transport: transport).events()
        let task = Task {
            var count = 0
            for try await _ in stream { count += 1 }
            return count
        }
        // Wait for the event to be consumed, then cancel the consuming task (what dtop does on quit).
        for _ in 0..<200 where transport.requests.isEmpty {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        try await Task.sleep(nanoseconds: 50_000_000)
        task.cancel()
        _ = try? await task.value
        // The producer thread notices the interrupt and closes the connection.
        for _ in 0..<200 where !(transport.connections.first?.isClosed ?? false) {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        #expect(transport.connections.first?.wasInterrupted == true)
        #expect(transport.connections.first?.isClosed == true)
    }

    @Test("an HTTP error ends the stream with that error")
    func httpError() async {
        let transport = MockTransport(always: .error(500, message: "daemon sad"))
        await #expect(throws: DockerError.self) {
            for try await _ in DockerClient(transport: transport).events() {}
        }
    }

    @Test("connection failure ends the stream with an error")
    func connectionError() async {
        let transport = MockTransport(always: .http())
        transport.connectError = .connectionFailed(endpoint: "mock", reason: "refused")
        await #expect(throws: DockerError.self) {
            for try await _ in DockerClient(transport: transport).events() {}
        }
    }
}

@Suite("JSON line splitter")
struct JSONLineSplitterTests {
    @Test("splits on newlines across arbitrary chunk boundaries")
    func splits() throws {
        var splitter = JSONLineSplitter()
        #expect(try splitter.feed(Array("{\"a\":1}\n{\"b\"".utf8)).map { String(decoding: $0, as: UTF8.self) } == [#"{"a":1}"#])
        #expect(try splitter.feed(Array(":2}\r\n\n  \n{\"c\":3}".utf8)).map { String(decoding: $0, as: UTF8.self) }
            == ["{\"b\":2}\r"])
        #expect(splitter.finish().map { String(decoding: $0, as: UTF8.self) } == #"{"c":3}"#)
        #expect(splitter.finish() == nil)
    }
}

@Suite("API versions")
struct APIVersionTests {
    @Test("parses and compares numerically")
    func compare() {
        #expect(APIVersion("1.9")! < APIVersion("1.10")!)
        #expect(APIVersion("1.43")! > APIVersion("1.41")!)
        #expect(APIVersion("2.0")! > APIVersion("1.99")!)
        #expect(APIVersion(" 1.41 ")?.description == "1.41")
        #expect(APIVersion("1") == nil)
        #expect(APIVersion("a.b") == nil)
        #expect(APIVersion("1.2.3") == nil)
    }

    @Test("negotiation never exceeds what dtop supports")
    func negotiate() {
        #expect(APIVersion.negotiate(server: APIVersion(major: 1, minor: 30)) == APIVersion(major: 1, minor: 30))
        #expect(APIVersion.negotiate(server: APIVersion(major: 1, minor: 60)) == APIVersion.maximumSupported)
    }

    @Test("action verbs match Engine API paths")
    func verbs() {
        #expect(ContainerAction.start.verb == "start")
        #expect(ContainerAction.kill(signal: nil).verb == "kill")
        #expect(ContainerAction.stop(timeoutSeconds: 3).query.first?.1 == "3")
    }
}
