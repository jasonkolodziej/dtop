import Foundation

/// Splits a byte stream into newline-delimited records (used for `/events` and streamed stats).
public struct JSONLineSplitter: Sendable {
    private var buffer: [UInt8] = []
    public static let maxLineLength = 8 * 1024 * 1024

    public init() {}

    public mutating func feed(_ bytes: [UInt8]) throws -> [[UInt8]] {
        buffer.append(contentsOf: bytes)
        var lines: [[UInt8]] = []
        var start = 0
        for index in buffer.indices where buffer[index] == 10 {
            let line = Array(buffer[start..<index])
            if line.contains(where: { $0 != 13 && $0 != 32 }) { lines.append(line) }
            start = index + 1
        }
        buffer.removeSubrange(..<start)
        if buffer.count > Self.maxLineLength {
            throw DockerError.malformedResponse("streamed record exceeds \(Self.maxLineLength) bytes")
        }
        return lines
    }

    /// Remaining bytes when the stream ends without a trailing newline.
    public mutating func finish() -> [UInt8]? {
        defer { buffer.removeAll() }
        return buffer.contains(where: { $0 != 13 && $0 != 32 && $0 != 10 }) ? buffer : nil
    }
}

/// Holds the live connection of a streaming request so it can be interrupted from elsewhere.
final class StreamCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var connection: (any DockerConnection)?
    private var cancelled = false

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    func attach(_ connection: any DockerConnection) {
        lock.lock()
        self.connection = connection
        let shouldInterrupt = cancelled
        lock.unlock()
        if shouldInterrupt { connection.interrupt() }
    }

    func cancel() {
        lock.lock()
        cancelled = true
        let connection = self.connection
        lock.unlock()
        connection?.interrupt()
    }
}

/// Docker Engine API client over a pluggable transport.
public struct DockerClient: DockerAPI {
    public let http: HTTPClient
    /// Separate client for long-lived streams, whose transport must not apply read timeouts.
    public let streamingHTTP: HTTPClient
    public let apiVersion: APIVersion?

    public init(transport: any DockerTransport, streamingTransport: (any DockerTransport)? = nil, apiVersion: APIVersion? = nil) {
        self.http = HTTPClient(transport: transport)
        self.streamingHTTP = HTTPClient(transport: streamingTransport ?? transport)
        self.apiVersion = apiVersion
    }

    private init(http: HTTPClient, streamingHTTP: HTTPClient, apiVersion: APIVersion?) {
        self.http = http
        self.streamingHTTP = streamingHTTP
        self.apiVersion = apiVersion
    }

    /// Builds a client for an endpoint using the platform's transport.
    public static func make(endpoint: DockerEndpoint, timeoutSeconds: Int = 30) throws -> DockerClient {
        let transport = try TransportFactory.make(for: endpoint, timeoutSeconds: timeoutSeconds)
        let streaming = try TransportFactory.make(for: endpoint, timeoutSeconds: 0)
        return DockerClient(transport: transport, streamingTransport: streaming)
    }

    public var endpoint: DockerEndpoint { http.transport.endpoint }

    /// Returns a copy pinned to the API version negotiated with the daemon.
    public func negotiated() async throws -> DockerClient {
        guard let server = try await ping() else { return self }
        return DockerClient(http: http, streamingHTTP: streamingHTTP, apiVersion: APIVersion.negotiate(server: server))
    }

    // MARK: - Request plumbing

    func versionedPath(_ path: String) -> String {
        guard let apiVersion else { return path }
        return "/v\(apiVersion)\(path)"
    }

    func makeRequest(_ method: HTTPMethod, _ path: String, query: [(String, String?)] = [], body: [UInt8] = []) -> HTTPRequest {
        var headers = HTTPHeaders()
        if !body.isEmpty { headers.set("Content-Type", "application/json") }
        return HTTPRequest(
            method: method,
            target: HTTPRequest.target(versionedPath(path), query: query),
            headers: headers,
            body: body
        )
    }

    /// Encodes a single path segment (names may contain `/` for image references).
    static func pathSegment(_ value: String) -> String {
        value.split(separator: "/", omittingEmptySubsequences: false)
            .map { segment in
                HTTPRequest.percentEncode(String(segment)).replacingPercentEncodedColon()
            }
            .joined(separator: "/")
    }

    func execute(_ request: HTTPRequest, acceptNotModified: Bool = false) async throws -> HTTPResponse {
        let http = self.http
        let response = try await runBlocking { try http.send(request) }
        if response.head.isSuccess || (acceptNotModified && response.status == 304) {
            return response
        }
        throw Self.error(from: response)
    }

    static func error(from response: HTTPResponse) -> DockerError {
        struct Message: Decodable { let message: String }
        let message = (try? JSONDecoder().decode(Message.self, from: Data(response.body)))?.message
            ?? (response.bodyText.trimmingWhitespace().isEmpty ? response.head.reason : response.bodyText.trimmingWhitespace())
        return .http(status: response.status, message: message)
    }

    func decode<T: Decodable>(_ type: T.Type, from response: HTTPResponse) throws -> T {
        do {
            return try JSONDecoder().decode(T.self, from: Data(response.body))
        } catch {
            throw DockerError.decoding("\(T.self): \(error)")
        }
    }

    func get<T: Decodable>(_ type: T.Type, _ path: String, query: [(String, String?)] = []) async throws -> T {
        let response = try await execute(makeRequest(.get, path, query: query))
        return try decode(T.self, from: response)
    }

    // MARK: - System

    public func ping() async throws -> APIVersion? {
        let request = HTTPRequest(method: .get, target: "/_ping")
        let response = try await execute(request)
        return response.head.headers["API-Version"].flatMap(APIVersion.init)
    }

    public func version() async throws -> DockerVersion {
        try await get(DockerVersion.self, "/version")
    }

    public func info() async throws -> SystemInfo {
        try await get(SystemInfo.self, "/info")
    }

    // MARK: - Containers

    public func listContainers(all: Bool) async throws -> [Container] {
        try await get([Container].self, "/containers/json", query: [("all", all ? "1" : "0")])
    }

    public func inspectContainer(id: String) async throws -> ContainerDetails {
        try await get(ContainerDetails.self, "/containers/\(Self.pathSegment(id))/json")
    }

    public func stats(id: String) async throws -> ContainerStats {
        let supportsOneShot = apiVersion.map { $0 >= APIVersion.oneShotStats } ?? true
        let query: [(String, String?)] = [("stream", "false"), ("one-shot", supportsOneShot ? "true" : nil)]
        return try await get(ContainerStats.self, "/containers/\(Self.pathSegment(id))/stats", query: query)
    }

    public func top(id: String) async throws -> ContainerTop {
        try await get(ContainerTop.self, "/containers/\(Self.pathSegment(id))/top")
    }

    public func logs(id: String, tail: Int) async throws -> [LogLine] {
        let request = makeRequest(.get, "/containers/\(Self.pathSegment(id))/logs", query: [
            ("stdout", "1"), ("stderr", "1"), ("tail", String(max(0, tail)))
        ])
        let response = try await execute(request)
        var decoder = LogStreamDecoder()
        return decoder.feed(response.body) + decoder.finish()
    }

    public func perform(_ action: ContainerAction, containerID: String) async throws {
        let request = makeRequest(.post, "/containers/\(Self.pathSegment(containerID))/\(action.verb)", query: action.query)
        // 304 means "already in that state", which is fine for an interactive tool.
        _ = try await execute(request, acceptNotModified: true)
    }

    public func removeContainer(id: String, force: Bool, removeVolumes: Bool) async throws {
        let request = makeRequest(.delete, "/containers/\(Self.pathSegment(id))", query: [
            ("force", force ? "1" : "0"), ("v", removeVolumes ? "1" : "0")
        ])
        _ = try await execute(request)
    }

    public func pruneContainers() async throws -> PruneReport {
        let response = try await execute(makeRequest(.post, "/containers/prune"))
        return try decode(PruneReport.self, from: response)
    }

    // MARK: - Images

    public func listImages() async throws -> [DockerImage] {
        try await get([DockerImage].self, "/images/json")
    }

    public func removeImage(id: String, force: Bool) async throws {
        let request = makeRequest(.delete, "/images/\(Self.pathSegment(id))", query: [("force", force ? "1" : "0")])
        _ = try await execute(request)
    }

    public func pruneImages(danglingOnly: Bool) async throws -> PruneReport {
        let filters = danglingOnly ? #"{"dangling":["true"]}"# : #"{"dangling":["false"]}"#
        let response = try await execute(makeRequest(.post, "/images/prune", query: [("filters", filters)]))
        return try decode(PruneReport.self, from: response)
    }

    // MARK: - Networks

    public func listNetworks() async throws -> [DockerNetwork] {
        try await get([DockerNetwork].self, "/networks")
    }

    public func removeNetwork(id: String) async throws {
        _ = try await execute(makeRequest(.delete, "/networks/\(Self.pathSegment(id))"))
    }

    public func pruneNetworks() async throws -> PruneReport {
        let response = try await execute(makeRequest(.post, "/networks/prune"))
        return try decode(PruneReport.self, from: response)
    }

    // MARK: - Volumes

    public func listVolumes() async throws -> [DockerVolume] {
        try await get(VolumeListResponse.self, "/volumes").volumes
    }

    public func removeVolume(name: String, force: Bool) async throws {
        let request = makeRequest(.delete, "/volumes/\(Self.pathSegment(name))", query: [("force", force ? "1" : "0")])
        _ = try await execute(request)
    }

    public func pruneVolumes() async throws -> PruneReport {
        // API >= 1.42 only prunes anonymous volumes unless asked; dtop keeps that safer default.
        let response = try await execute(makeRequest(.post, "/volumes/prune"))
        return try decode(PruneReport.self, from: response)
    }

    // MARK: - Events

    public func events() -> AsyncThrowingStream<DockerEvent, Error> {
        let request = makeRequest(.get, "/events", query: [
            ("filters", #"{"type":["container","image","network","volume"]}"#)
        ])
        return Self.jsonStream(DockerEvent.self, request: request, http: streamingHTTP)
    }

    /// Streams newline-delimited JSON records on a background thread until cancelled.
    static func jsonStream<T: Decodable & Sendable>(
        _ type: T.Type,
        request: HTTPRequest,
        http: HTTPClient
    ) -> AsyncThrowingStream<T, Error> {
        AsyncThrowingStream { continuation in
            let cancellation = StreamCancellation()
            continuation.onTermination = { _ in cancellation.cancel() }
            DispatchQueue.global(qos: .utility).async {
                var splitter = JSONLineSplitter()
                let decoder = JSONDecoder()
                var failure: DockerError?
                do {
                    try http.stream(
                        request,
                        onConnect: { cancellation.attach($0) },
                        onHead: { head in
                            if !head.isSuccess {
                                failure = .http(status: head.status, message: head.reason)
                            }
                        },
                        onBody: { chunk in
                            if failure != nil { return false }
                            for line in try splitter.feed(chunk) {
                                if let value = try? decoder.decode(T.self, from: Data(line)) {
                                    continuation.yield(value)
                                }
                            }
                            return !cancellation.isCancelled
                        }
                    )
                    if let failure {
                        continuation.finish(throwing: failure)
                    } else {
                        continuation.finish()
                    }
                } catch {
                    if cancellation.isCancelled {
                        continuation.finish()
                    } else {
                        continuation.finish(throwing: error)
                    }
                }
            }
        }
    }
}

extension String {
    /// Docker accepts `:` and `@` unescaped in paths (e.g. `sha256:...`, `image@digest`).
    fileprivate func replacingPercentEncodedColon() -> String {
        var result = self
        for (encoded, plain) in [("%3A", ":"), ("%40", "@")] {
            var output = ""
            var remainder = Substring(result)
            while let range = remainder.range(of: encoded) {
                output += remainder[..<range.lowerBound]
                output += plain
                remainder = remainder[range.upperBound...]
            }
            output += remainder
            result = output
        }
        return result
    }
}
