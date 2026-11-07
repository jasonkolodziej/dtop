import Foundation

/// Minimal blocking HTTP/1.1 client over a `DockerTransport`. One request per connection.
public struct HTTPClient: Sendable {
    public let transport: any DockerTransport
    public let readChunkSize: Int
    /// Hard cap on buffered (non-streaming) response bodies.
    public let maxBodySize: Int

    public init(transport: any DockerTransport, readChunkSize: Int = 64 * 1024, maxBodySize: Int = 256 * 1024 * 1024) {
        self.transport = transport
        self.readChunkSize = readChunkSize
        self.maxBodySize = maxBodySize
    }

    /// Sends a request and buffers the whole response.
    public func send(_ request: HTTPRequest) throws -> HTTPResponse {
        var head: HTTPResponseHead?
        var body: [UInt8] = []
        try stream(request, onHead: { head = $0 }, onBody: { chunk in
            body.append(contentsOf: chunk)
            if body.count > maxBodySize {
                throw DockerError.malformedResponse("response body exceeds \(maxBodySize) bytes")
            }
            return true
        })
        guard let head else { throw DockerError.malformedResponse("no response head") }
        return HTTPResponse(head: head, body: body)
    }

    /// Sends a request and delivers body fragments as they arrive. Return `false` from `onBody`
    /// to stop early. `onConnect` hands out the live connection so another thread can interrupt it.
    public func stream(
        _ request: HTTPRequest,
        onConnect: (any DockerConnection) -> Void = { _ in },
        onHead: (HTTPResponseHead) throws -> Void,
        onBody: ([UInt8]) throws -> Bool
    ) throws {
        let connection = try transport.connect()
        defer { connection.close() }
        onConnect(connection)
        try connection.write(request.serialized(host: transport.endpoint.hostHeader))

        var parser = HTTPResponseParser(isHeadRequest: request.method == .head)
        while true {
            let bytes = try connection.read(maxLength: readChunkSize)
            let events = bytes.isEmpty ? try parser.finish() : try parser.feed(bytes)
            for event in events {
                switch event {
                case .head(let head):
                    try onHead(head)
                case .body(let chunk):
                    if try !onBody(chunk) { return }
                case .end:
                    return
                }
            }
            if bytes.isEmpty { return }
        }
    }
}

/// Runs blocking work on a background queue so it never stalls the Swift concurrency pool.
func runBlocking<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws -> T {
    try await withCheckedThrowingContinuation { continuation in
        DispatchQueue.global(qos: .userInitiated).async {
            continuation.resume(with: Result { try body() })
        }
    }
}
