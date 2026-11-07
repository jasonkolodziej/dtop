import Foundation
@testable import DtopCore

/// A request as the mock daemon saw it on the wire.
struct RecordedRequest: Sendable, Equatable {
    var method: String
    var target: String
    var headers: [String: String]
    var body: [UInt8]

    var path: String { String(target.split(separator: "?", maxSplits: 1).first ?? "") }

    var query: [String: String] {
        guard let questionMark = target.firstIndex(of: "?") else { return [:] }
        var result: [String: String] = [:]
        for pair in target[target.index(after: questionMark)...].split(separator: "&") {
            let parts = pair.split(separator: "=", maxSplits: 1).map(String.init)
            let key = parts[0].removingPercentEncoding ?? parts[0]
            let value = parts.count > 1 ? (parts[1].removingPercentEncoding ?? parts[1]) : ""
            result[key] = value
        }
        return result
    }

    static func parse(_ bytes: [UInt8]) -> RecordedRequest? {
        guard let headEnd = HTTPResponseParser.find(HTTPResponseParser.crlfcrlf, in: bytes) else { return nil }
        let head = String(decoding: bytes[..<headEnd], as: UTF8.self)
        let lines = head.components(separatedBy: "\r\n")
        let requestLine = lines[0].split(separator: " ")
        guard requestLine.count == 3 else { return nil }
        var headers: [String: String] = [:]
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else { continue }
            headers[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
        let body = Array(bytes[(headEnd + 4)...])
        return RecordedRequest(method: String(requestLine[0]), target: String(requestLine[1]), headers: headers, body: body)
    }
}

/// What the mock daemon answers with.
struct MockResponse: Sendable {
    var chunks: [[UInt8]]
    /// When true, reads block after the scripted chunks until the connection is interrupted.
    var hangsAfterChunks = false

    static func http(
        status: Int = 200,
        reason: String = "OK",
        headers: [(String, String)] = [],
        body: String = "",
        chunked: Bool = false
    ) -> MockResponse {
        MockResponse(chunks: [httpBytes(status: status, reason: reason, headers: headers, body: Array(body.utf8), chunked: chunked)])
    }

    static func json(_ body: String, status: Int = 200) -> MockResponse {
        .http(status: status, reason: status == 200 ? "OK" : "Error", headers: [("Content-Type", "application/json")], body: body)
    }

    static func error(_ status: Int, message: String) -> MockResponse {
        .json(#"{"message":"\#(message)"}"#, status: status)
    }

    static func httpBytes(status: Int, reason: String, headers: [(String, String)], body: [UInt8], chunked: Bool) -> [UInt8] {
        var head = "HTTP/1.1 \(status) \(reason)\r\n"
        for (name, value) in headers { head += "\(name): \(value)\r\n" }
        if chunked {
            head += "Transfer-Encoding: chunked\r\n\r\n"
            var bytes = Array(head.utf8)
            if !body.isEmpty {
                bytes += Array(String(body.count, radix: 16).utf8) + [13, 10] + body + [13, 10]
            }
            bytes += Array("0\r\n\r\n".utf8)
            return bytes
        }
        head += "Content-Length: \(body.count)\r\n\r\n"
        return Array(head.utf8) + body
    }
}

final class ScriptedConnection: DockerConnection, @unchecked Sendable {
    private let lock = NSLock()
    private let handler: @Sendable (RecordedRequest) -> MockResponse
    private let onRequest: @Sendable (RecordedRequest) -> Void
    private var written: [UInt8] = []
    private var pending: [[UInt8]]?
    private var hangs = false
    private let wakeUp = DispatchSemaphore(value: 0)
    private(set) var isClosed = false
    private(set) var wasInterrupted = false

    init(handler: @escaping @Sendable (RecordedRequest) -> MockResponse, onRequest: @escaping @Sendable (RecordedRequest) -> Void) {
        self.handler = handler
        self.onRequest = onRequest
    }

    var writtenBytes: [UInt8] {
        lock.lock()
        defer { lock.unlock() }
        return written
    }

    func write(_ bytes: [UInt8]) throws {
        lock.lock()
        defer { lock.unlock() }
        if isClosed { throw DockerError.cancelled }
        written += bytes
    }

    func read(maxLength: Int) throws -> [UInt8] {
        lock.lock()
        if wasInterrupted {
            lock.unlock()
            throw DockerError.cancelled
        }
        if pending == nil {
            guard let request = RecordedRequest.parse(written) else {
                lock.unlock()
                throw DockerError.io("mock received an incomplete request")
            }
            let response = handler(request)
            pending = response.chunks
            hangs = response.hangsAfterChunks
            lock.unlock()
            onRequest(request)
            lock.lock()
        }
        if var chunks = pending, !chunks.isEmpty {
            var chunk = chunks.removeFirst()
            if chunk.count > maxLength {
                chunks.insert(Array(chunk[maxLength...]), at: 0)
                chunk = Array(chunk[..<maxLength])
            }
            pending = chunks
            lock.unlock()
            return chunk
        }
        let shouldHang = hangs
        lock.unlock()
        if shouldHang {
            wakeUp.wait()
            throw DockerError.cancelled
        }
        return []
    }

    func interrupt() {
        lock.lock()
        let alreadyInterrupted = wasInterrupted
        wasInterrupted = true
        lock.unlock()
        if !alreadyInterrupted { wakeUp.signal() }
    }

    func close() {
        lock.lock()
        isClosed = true
        lock.unlock()
    }
}

/// An in-memory Docker daemon: routes each request through `handler` and records it.
final class MockTransport: DockerTransport, @unchecked Sendable {
    let endpoint: DockerEndpoint
    private let lock = NSLock()
    private let handler: @Sendable (RecordedRequest) -> MockResponse
    private var recorded: [RecordedRequest] = []
    private var opened: [ScriptedConnection] = []
    var connectError: DockerError?

    init(endpoint: DockerEndpoint = .unixSocket(path: "/mock/docker.sock"),
         handler: @escaping @Sendable (RecordedRequest) -> MockResponse) {
        self.endpoint = endpoint
        self.handler = handler
    }

    /// Answers every request with the same response.
    convenience init(always response: MockResponse) {
        self.init { _ in response }
    }

    var requests: [RecordedRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    var connections: [ScriptedConnection] {
        lock.lock()
        defer { lock.unlock() }
        return opened
    }

    func connect() throws -> any DockerConnection {
        if let connectError { throw connectError }
        let connection = ScriptedConnection(handler: handler) { [weak self] request in
            guard let self else { return }
            self.lock.lock()
            self.recorded.append(request)
            self.lock.unlock()
        }
        lock.lock()
        opened.append(connection)
        lock.unlock()
        return connection
    }
}
