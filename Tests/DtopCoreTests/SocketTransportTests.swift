#if !os(Windows)
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

import Foundation
import Testing
@testable import DtopCore

/// A tiny HTTP server on a real Unix domain socket, answering each connection with `reply`.
final class UnixSocketServer: @unchecked Sendable {
    let path: String
    private let fd: Int32
    private let reply: @Sendable (String) -> [UInt8]
    private let lock = NSLock()
    private var received: [String] = []

    init(reply: @escaping @Sendable (String) -> [UInt8]) throws {
        path = "/tmp/dtop-\(UUID().uuidString.prefix(8)).sock"
        self.reply = reply
        unlink(path)
        #if canImport(Glibc) || canImport(Musl)
        fd = socket(AF_UNIX, Int32(SOCK_STREAM.rawValue), 0)
        #else
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        #endif
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        withUnsafeMutableBytes(of: &address.sun_path) { raw in
            raw.copyBytes(from: bytes)
            raw[bytes.count] = 0
        }
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0, listen(fd, 8) == 0 else { throw DockerError.io("test server could not bind \(path)") }
        let thread = Thread { [self] in self.acceptLoop() }
        thread.start()
    }

    var requests: [String] {
        lock.lock()
        defer { lock.unlock() }
        return received
    }

    private func acceptLoop() {
        while true {
            let client = accept(fd, nil, nil)
            if client < 0 { return }
            var data: [UInt8] = []
            var buffer = [UInt8](repeating: 0, count: 4096)
            while HTTPResponseParser.find(HTTPResponseParser.crlfcrlf, in: data) == nil {
                let count = buffer.withUnsafeMutableBytes { recv(client, $0.baseAddress, $0.count, 0) }
                if count <= 0 { break }
                data += buffer[..<count]
            }
            let request = String(decoding: data, as: UTF8.self)
            lock.lock()
            received.append(request)
            lock.unlock()
            let response = reply(request)
            _ = response.withUnsafeBytes { send(client, $0.baseAddress, $0.count, 0) }
            close(client)
        }
    }

    func stop() {
        shutdown(fd, Int32(SHUT_RDWR))
        close(fd)
        unlink(path)
    }
}

@Suite("Unix socket transport")
struct UnixSocketTransportTests {
    @Test("round-trips a Docker API call over a real Unix socket")
    func roundTrip() async throws {
        let server = try UnixSocketServer { request in
            let body = request.hasPrefix("GET /version") ? Fixtures.version : "[]"
            return MockResponse.httpBytes(status: 200, reason: "OK", headers: [("Content-Type", "application/json")],
                                          body: Array(body.utf8), chunked: true)
        }
        defer { server.stop() }
        let client = DockerClient(transport: UnixSocketTransport(path: server.path, timeoutSeconds: 5))
        #expect(try await client.version().version == "27.3.1")
        #expect(try await client.listContainers(all: true).isEmpty)
        #expect(server.requests.count == 2)
        #expect(server.requests[0].contains("Host: docker\r\n"))
        #expect(server.requests[1].hasPrefix("GET /containers/json?all=1 HTTP/1.1"))
    }

    @Test("a missing socket fails with an actionable connection error")
    func missingSocket() {
        let transport = UnixSocketTransport(path: "/tmp/dtop-does-not-exist-\(UUID().uuidString.prefix(6)).sock")
        do {
            _ = try transport.connect()
            Issue.record("connecting to a missing socket should fail")
        } catch DockerError.connectionFailed(let endpoint, _) {
            #expect(endpoint.hasPrefix("unix:///tmp/dtop-does-not-exist"))
        } catch {
            Issue.record("unexpected error \(error)")
        }
    }

    @Test("overlong socket paths are rejected before connecting")
    func tooLong() {
        let path = "/tmp/" + String(repeating: "x", count: 200) + ".sock"
        #expect(throws: DockerError.socketPathTooLong(path)) { _ = try UnixSocketTransport(path: path).connect() }
    }

    @Test("interrupt unblocks a reader stuck waiting for data")
    func interrupt() async throws {
        let server = try UnixSocketServer { _ in
            // Never answer: sleep long enough that only an interrupt can end the read.
            Thread.sleep(forTimeInterval: 3)
            return []
        }
        defer { server.stop() }
        let connection = try UnixSocketTransport(path: server.path, timeoutSeconds: 0).connect()
        try connection.write(Array("GET / HTTP/1.1\r\n\r\n".utf8))
        let started = Date()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.2) { connection.interrupt() }
        let bytes = try? connection.read(maxLength: 16)
        #expect(bytes == nil || bytes == [])
        #expect(Date().timeIntervalSince(started) < 2.5)
        connection.close()
        connection.close()
        #expect(throws: DockerError.cancelled) { try connection.write([1]) }
    }

    @Test("TCP transport reports refused connections")
    func tcpRefused() {
        // Port 1 on localhost is essentially never listening.
        #expect(throws: DockerError.self) { _ = try TCPTransport(host: "127.0.0.1", port: 1, timeoutSeconds: 2).connect() }
        #expect(throws: DockerError.self) { _ = try TCPTransport(host: "no-such-host.invalid", port: 2375).connect() }
    }
}
#endif
