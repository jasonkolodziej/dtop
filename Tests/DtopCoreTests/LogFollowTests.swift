import Foundation
import Testing
@testable import DtopCore

/// One multiplexed log frame (`[stream, 0, 0, 0, size(be32)] + payload`).
private func frame(_ stream: UInt8, _ text: String) -> [UInt8] {
    let payload = Array(text.utf8)
    let size = UInt32(payload.count)
    return [stream, 0, 0, 0, UInt8(size >> 24), UInt8(size >> 16 & 0xFF), UInt8(size >> 8 & 0xFF), UInt8(size & 0xFF)] + payload
}

/// One HTTP chunk wrapping `bytes`.
private func chunk(_ bytes: [UInt8]) -> [UInt8] {
    Array(String(bytes.count, radix: 16).utf8) + [13, 10] + bytes + [13, 10]
}

private let chunkedHead = Array("HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.multiplexed-stream\r\nTransfer-Encoding: chunked\r\n\r\n".utf8)

@Suite("Followed logs")
struct LogFollowTests {
    @Test("frames arrive as batches of lines, in order, with their stream; the request follows")
    func batches() async throws {
        let transport = MockTransport(always: MockResponse(chunks: [
            chunkedHead,
            chunk(frame(1, "first\nsec")),
            chunk(frame(1, "ond\n") + frame(2, "oops\n")),
            chunk(frame(1, "tail without newline")),
            Array("0\r\n\r\n".utf8)
        ]))
        var batches: [[LogLine]] = []
        for try await batch in DockerClient(transport: transport).followLogs(id: "web/1", tail: 50) {
            batches.append(batch)
        }
        let lines = batches.flatMap { $0 }
        #expect(lines == [LogLine(stream: .stdout, text: "first"), LogLine(stream: .stdout, text: "second"),
                          LogLine(stream: .stderr, text: "oops"), LogLine(stream: .stdout, text: "tail without newline")])
        #expect(batches.count >= 3, "lines are delivered as they arrive, not all at the end")
        let request = try #require(transport.requests.first)
        #expect(request.path == "/containers/web/1/logs")
        #expect(request.query["follow"] == "1")
        #expect(request.query["tail"] == "50")
        #expect(request.query["timestamps"] == "1")
        #expect(request.query["since"] == nil)
        #expect(request.query["stdout"] == "1" && request.query["stderr"] == "1")
    }

    @Test("timestamps are split off each line; resuming asks for everything since the last one")
    func timestamps() async throws {
        let transport = MockTransport(always: MockResponse(chunks: [
            chunkedHead, chunk(frame(1, "2024-05-01T12:34:56.5Z hello\nnot-a-time line\n")), Array("0\r\n\r\n".utf8)
        ]))
        var lines: [LogLine] = []
        for try await batch in DockerClient(transport: transport).followLogs(id: "c", tail: 9, since: 1_714_566_896.5) {
            lines += batch
        }
        #expect(lines.map(\.text) == ["hello", "not-a-time line"])
        #expect(lines[0].timestamp == 1_714_566_896.5)
        #expect(lines[1].timestamp == nil)
        let request = try #require(transport.requests.first)
        #expect(request.query["since"] == "1714566896.500000000")
        #expect(request.query["tail"] == "all")
    }

    @Test("since timestamps use Docker's seconds.nanoseconds form")
    func sinceFormat() {
        #expect(DockerClient.unixTimestamp(0) == "0.000000000")
        #expect(DockerClient.unixTimestamp(1_714_566_896.25) == "1714566896.250000000")
        #expect(DockerClient.unixTimestamp(5.9999999999) == "6.000000000", "rounding carries into the seconds")
    }

    @Test("without timestamps the prefix is ordinary text")
    func plainDecoder() {
        var decoder = LogStreamDecoder(mode: .raw)
        #expect(decoder.feed(Array("2024-05-01T12:34:56Z hi\n".utf8)) == [LogLine(stream: .stdout, text: "2024-05-01T12:34:56Z hi")])
    }

    @Test("TTY containers stream raw text")
    func raw() async throws {
        let transport = MockTransport(always: MockResponse(chunks: [
            chunkedHead, chunk(Array("$ ls\r\nfile\n".utf8)), Array("0\r\n\r\n".utf8)
        ]))
        var lines: [LogLine] = []
        for try await batch in DockerClient(transport: transport).followLogs(id: "t", tail: 10) { lines += batch }
        #expect(lines.map(\.text) == ["$ ls", "file"])
    }

    @Test("cancelling the consumer interrupts the open stream")
    func cancellation() async throws {
        let transport = MockTransport(always: MockResponse(chunks: [chunkedHead, chunk(frame(1, "hello\n"))],
                                                           hangsAfterChunks: true))
        let stream = DockerClient(transport: transport).followLogs(id: "x", tail: 1)
        let task = Task {
            var received: [LogLine] = []
            for try await batch in stream { received += batch }
            return received
        }
        for _ in 0..<200 where transport.requests.isEmpty {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        try await Task.sleep(nanoseconds: 50_000_000)
        task.cancel()
        _ = try? await task.value
        for _ in 0..<200 where !(transport.connections.first?.isClosed ?? false) {
            try await Task.sleep(nanoseconds: 10_000_000)
        }
        #expect(transport.connections.first?.wasInterrupted == true)
        #expect(transport.connections.first?.isClosed == true)
    }

    @Test("a missing container ends the stream with the daemon's message")
    func notFound() async {
        let transport = MockTransport(always: .error(404, message: "No such container: ghost"))
        do {
            for try await _ in DockerClient(transport: transport).followLogs(id: "ghost", tail: 1) {}
            Issue.record("expected an error")
        } catch {
            #expect(error as? DockerError == .http(status: 404, message: "No such container: ghost"))
        }
    }

    @Test("APIs without streaming fall back to one fetch")
    func fallback() async throws {
        struct OneShot: DockerAPI {
            func ping() async throws -> APIVersion? { nil }
            func version() async throws -> DockerVersion { DockerVersion(version: "1", apiVersion: "1.45") }
            func info() async throws -> SystemInfo { SystemInfo() }
            func listContainers(all: Bool) async throws -> [Container] { [] }
            func inspectContainer(id: String) async throws -> ContainerDetails { throw DockerError.cancelled }
            func stats(id: String) async throws -> ContainerStats { throw DockerError.cancelled }
            func top(id: String) async throws -> ContainerTop { ContainerTop(titles: [], processes: []) }
            func logs(id: String, tail: Int) async throws -> [LogLine] { [LogLine(stream: .stdout, text: "\(id):\(tail)")] }
            func perform(_ action: ContainerAction, containerID: String) async throws {}
            func removeContainer(id: String, force: Bool, removeVolumes: Bool) async throws {}
            func pruneContainers() async throws -> PruneReport { PruneReport(deleted: [], spaceReclaimed: 0) }
            func listImages() async throws -> [DockerImage] { [] }
            func removeImage(id: String, force: Bool) async throws {}
            func pruneImages(danglingOnly: Bool) async throws -> PruneReport { PruneReport(deleted: [], spaceReclaimed: 0) }
            func listNetworks() async throws -> [DockerNetwork] { [] }
            func removeNetwork(id: String) async throws {}
            func pruneNetworks() async throws -> PruneReport { PruneReport(deleted: [], spaceReclaimed: 0) }
            func listVolumes() async throws -> [DockerVolume] { [] }
            func removeVolume(name: String, force: Bool) async throws {}
            func pruneVolumes() async throws -> PruneReport { PruneReport(deleted: [], spaceReclaimed: 0) }
            func events() -> AsyncThrowingStream<DockerEvent, Error> { AsyncThrowingStream { $0.finish() } }
        }
        var batches: [[LogLine]] = []
        for try await batch in OneShot().followLogs(id: "c", tail: 7) { batches.append(batch) }
        #expect(batches == [[LogLine(stream: .stdout, text: "c:7")]])
    }
}

@Suite("Hosts and the docker CLI")
struct HostCLITests {
    @Test("compose commands reach the same daemon: context by name, else --host plus TLS flags")
    func cliArguments() {
        let context = DockerHost(name: "prod", endpoint: .tcp(host: "p", port: 2376), tls: TLSConfiguration(), source: "context prod")
        #expect(context.dockerCLIArguments == ["--context", "prod"])
        #expect(context.contextName == "prod")
        let defaultContext = DockerHost(name: "default", endpoint: .unixSocket(path: "/s"), source: "context default")
        #expect(defaultContext.contextName == nil)
        #expect(defaultContext.dockerCLIArguments == ["--host", "unix:///s"])
        let tls = DockerHost(name: "lab", endpoint: .tcp(host: "lab", port: 2376),
                             tls: TLSConfiguration(verifyPeer: true, caFile: "/ca", certificateFile: "/c", keyFile: "/k"))
        #expect(tls.dockerCLIArguments == ["--host", "tcp://lab:2376", "--tlsverify", "--tlscacert", "/ca", "--tlscert", "/c",
                                           "--tlskey", "/k"])
        let insecure = DockerHost(name: "x", endpoint: .tcp(host: "x", port: 2376), tls: TLSConfiguration(verifyPeer: false))
        #expect(insecure.dockerCLIArguments == ["--host", "tcp://x:2376", "--tls"])

        let project = ComposeProject.group([Container(id: "1", names: ["/a"], image: "i", labels: [
            ComposeLabel.project: "shop", ComposeLabel.workingDir: "/srv/shop"
        ], state: .running)]).projects[0]
        let invocation = ComposeCommandBuilder.invocation(for: .up, project: project, host: tls)
        #expect(Array(invocation.arguments.prefix(9)) == tls.dockerCLIArguments)
        #expect(invocation.arguments.contains("compose"))
    }

    @Test("local endpoints are sockets, pipes and loopback TCP")
    func local() {
        #expect(DockerEndpoint.unixSocket(path: "/s").isLocal)
        #expect(DockerEndpoint.namedPipe(path: #"\\.\pipe\x"#).isLocal)
        #expect(DockerEndpoint.tcp(host: "127.0.0.1", port: 2375).isLocal)
        #expect(DockerEndpoint.tcp(host: "LOCALHOST", port: 2376).isLocal)
        #expect(!DockerEndpoint.tcp(host: "10.0.0.5", port: 2376).isLocal)
    }

    @Test("plain HTTP to a TLS daemon says to turn TLS on")
    func httpsHint() {
        let error = DockerError.http(status: 400, message: "Client sent an HTTP request to an HTTPS server.")
        #expect(error.description.contains("--tlsverify"))
        #expect(!DockerError.http(status: 400, message: "bad").description.contains("--tlsverify"))
    }
}
