import Testing
@testable import DtopCore

@Suite("HTTP requests")
struct HTTPRequestTests {
    @Test("serializes a GET with default headers and no body")
    func serializeGet() {
        let request = HTTPRequest(method: .get, target: "/v1.43/containers/json?all=1")
        let text = String(decoding: request.serialized(host: "docker"), as: UTF8.self)
        #expect(text.hasPrefix("GET /v1.43/containers/json?all=1 HTTP/1.1\r\n"))
        #expect(text.contains("Host: docker\r\n"))
        #expect(text.contains("User-Agent: dtop\r\n"))
        #expect(text.contains("Connection: close\r\n"))
        #expect(!text.contains("Content-Length"))
        #expect(text.hasSuffix("\r\n\r\n"))
    }

    @Test("POST always carries Content-Length, even with an empty body")
    func serializePost() {
        let empty = String(decoding: HTTPRequest(method: .post, target: "/x").serialized(host: "h"), as: UTF8.self)
        #expect(empty.contains("Content-Length: 0\r\n"))
        let withBody = HTTPRequest(method: .post, target: "/x", body: Array("{}".utf8))
        let text = String(decoding: withBody.serialized(host: "h"), as: UTF8.self)
        #expect(text.contains("Content-Length: 2\r\n"))
        #expect(text.hasSuffix("\r\n\r\n{}"))
    }

    @Test("caller headers are not overwritten")
    func customHeaders() {
        var headers = HTTPHeaders()
        headers.set("Host", "custom")
        headers.set("Connection", "keep-alive")
        let text = String(decoding: HTTPRequest(method: .get, target: "/", headers: headers).serialized(host: "h"),
                          as: UTF8.self)
        #expect(text.contains("Host: custom\r\n"))
        #expect(!text.contains("Host: h\r\n"))
        #expect(text.contains("Connection: keep-alive\r\n"))
    }

    @Test("percent-encodes everything but unreserved characters")
    func percentEncoding() {
        #expect(HTTPRequest.percentEncode("abc-._~XYZ019") == "abc-._~XYZ019")
        #expect(HTTPRequest.percentEncode(#"{"a":["b c"]}"#) == "%7B%22a%22%3A%5B%22b%20c%22%5D%7D")
        #expect(HTTPRequest.percentEncode("a&b=c+d/e") == "a%26b%3Dc%2Bd%2Fe")
        #expect(HTTPRequest.percentEncode("é") == "%C3%A9")
        #expect(HTTPRequest.percentEncode("") == "")
    }

    @Test("builds targets from ordered query items and skips nil values")
    func targets() {
        #expect(HTTPRequest.target("/a") == "/a")
        #expect(HTTPRequest.target("/a", query: [("x", "1"), ("y", nil), ("z", "a b")]) == "/a?x=1&z=a%20b")
        #expect(HTTPRequest.target("/a", query: [("y", nil)]) == "/a")
    }
}

@Suite("HTTP headers")
struct HTTPHeadersTests {
    @Test("lookups are case-insensitive and keep insertion order")
    func caseInsensitive() {
        var headers = HTTPHeaders([("Content-Type", "a"), ("X-Thing", "1")])
        headers.add("x-thing", "2")
        #expect(headers["content-type"] == "a")
        #expect(headers["X-THING"] == "1")
        #expect(headers.values(for: "x-thing") == ["1", "2"])
        headers.set("X-Thing", "3")
        #expect(headers.values(for: "x-thing") == ["3"])
        #expect(headers["missing"] == nil)
        #expect(Array(headers.map(\.name)) == ["Content-Type", "X-Thing"])
    }

    @Test("equality compares names and values in order")
    func equality() {
        #expect(HTTPHeaders([("A", "1")]) == HTTPHeaders([("A", "1")]))
        #expect(HTTPHeaders([("A", "1")]) != HTTPHeaders([("A", "2")]))
        #expect(HTTPHeaders([("A", "1"), ("B", "2")]) != HTTPHeaders([("B", "2"), ("A", "1")]))
    }
}

@Suite("HTTP response parser")
struct HTTPResponseParserTests {
    func parseAll(_ chunks: [[UInt8]], finish: Bool = true) throws -> (HTTPResponseHead?, [UInt8], Bool) {
        var parser = HTTPResponseParser()
        var head: HTTPResponseHead?
        var body: [UInt8] = []
        var ended = false
        var events: [HTTPParserEvent] = []
        for chunk in chunks { events += try parser.feed(chunk) }
        if finish && !parser.isComplete { events += try parser.finish() }
        for event in events {
            switch event {
            case .head(let value): head = value
            case .body(let bytes): body += bytes
            case .end: ended = true
            }
        }
        return (head, body, ended)
    }

    func bytes(_ text: String) -> [UInt8] { Array(text.utf8) }

    @Test("parses a Content-Length response")
    func contentLength() throws {
        let (head, body, ended) = try parseAll([bytes("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b\r\n\r\nhello")])
        #expect(head?.status == 200)
        #expect(head?.reason == "OK")
        #expect(head?.headers["x-a"] == "b")
        #expect(String(decoding: body, as: UTF8.self) == "hello")
        #expect(ended)
    }

    @Test("ignores bytes past Content-Length")
    func trailingGarbage() throws {
        let (_, body, ended) = try parseAll([bytes("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhiEXTRA")], finish: false)
        #expect(String(decoding: body, as: UTF8.self) == "hi")
        #expect(ended)
    }

    @Test("decodes chunked bodies with extensions and trailers")
    func chunked() throws {
        let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
            + "5;ext=1\r\nhello\r\n" + "1\r\n \r\n" + "A\r\n0123456789\r\n" + "0\r\nX-Trailer: yes\r\n\r\n"
        let (_, body, ended) = try parseAll([bytes(raw)], finish: false)
        #expect(String(decoding: body, as: UTF8.self) == "hello 0123456789")
        #expect(ended)
    }

    @Test("produces identical output no matter how the bytes are split")
    func byteAtATime() throws {
        let raw = bytes("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n4\r\ndefg\r\n0\r\n\r\n")
        let (_, body, ended) = try parseAll(raw.map { [$0] }, finish: false)
        #expect(String(decoding: body, as: UTF8.self) == "abcdefg")
        #expect(ended)

        let fixed = bytes("HTTP/1.1 201 Created\r\nContent-Length: 4\r\n\r\nwxyz")
        for split in 1..<fixed.count {
            let (head, body, ended) = try parseAll([Array(fixed[..<split]), Array(fixed[split...])], finish: false)
            #expect(head?.status == 201)
            #expect(String(decoding: body, as: UTF8.self) == "wxyz")
            #expect(ended)
        }
    }

    @Test("reads until close when there is no length")
    func untilClose() throws {
        var parser = HTTPResponseParser()
        var events = try parser.feed(bytes("HTTP/1.0 200 OK\r\n\r\npart1"))
        events += try parser.feed(bytes("part2"))
        #expect(!parser.isComplete)
        events += try parser.finish()
        #expect(events.last == .end)
        let body = events.compactMap { event -> [UInt8]? in
            if case .body(let bytes) = event { return bytes }
            return nil
        }.flatMap { $0 }
        #expect(String(decoding: body, as: UTF8.self) == "part1part2")
        #expect(parser.isComplete)
    }

    @Test("204, 304 and HEAD responses have no body", arguments: [204, 304])
    func noBodyStatuses(status: Int) throws {
        let (head, body, ended) = try parseAll([bytes("HTTP/1.1 \(status) X\r\nContent-Length: 10\r\n\r\n")], finish: false)
        #expect(head?.status == status)
        #expect(body.isEmpty)
        #expect(ended)
    }

    @Test("HEAD requests complete at the end of headers")
    func headRequest() throws {
        var parser = HTTPResponseParser(isHeadRequest: true)
        let events = try parser.feed(bytes("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n"))
        #expect(events.last == .end)
    }

    @Test("Content-Length 0 ends immediately")
    func zeroLength() throws {
        let (_, body, ended) = try parseAll([bytes("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")], finish: false)
        #expect(body.isEmpty)
        #expect(ended)
    }

    @Test("tolerates a status line without a reason phrase")
    func missingReason() throws {
        let (head, _, _) = try parseAll([bytes("HTTP/1.1 200\r\nContent-Length: 0\r\n\r\n")])
        #expect(head?.status == 200)
        #expect(head?.reason == "")
    }

    @Test("rejects malformed responses", arguments: [
        "garbage\r\n\r\n",
        "HTTP/1.1 abc OK\r\n\r\n",
        "HTTP/1.1 42 Too Low\r\n\r\n",
        "HTTP/1.1 200 OK\r\nNoColonHere\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: ten\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabXY"
    ])
    func malformed(raw: String) {
        var parser = HTTPResponseParser()
        #expect(throws: DockerError.self) { _ = try parser.feed(bytes(raw)) }
    }

    @Test("EOF before or during the response is an error")
    func prematureEOF() throws {
        var empty = HTTPResponseParser()
        #expect(throws: DockerError.self) { _ = try empty.finish() }

        var partialHead = HTTPResponseParser()
        _ = try partialHead.feed(bytes("HTTP/1.1 200 OK\r\nContent-"))
        #expect(throws: DockerError.self) { _ = try partialHead.finish() }

        var partialBody = HTTPResponseParser()
        _ = try partialBody.feed(bytes("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"))
        #expect(throws: DockerError.self) { _ = try partialBody.finish() }
    }

    @Test("refuses unbounded headers")
    func headerLimit() {
        var parser = HTTPResponseParser()
        let huge = [UInt8](repeating: UInt8(ascii: "a"), count: HTTPResponseParser.maxHeadSize + 10)
        #expect(throws: DockerError.self) { _ = try parser.feed(huge) }
    }

    @Test("finish after completion is a no-op")
    func finishAfterDone() throws {
        var parser = HTTPResponseParser()
        _ = try parser.feed(bytes("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"))
        #expect(try parser.finish().isEmpty)
    }

    @Test("byte search helper finds needles at any position")
    func find() {
        #expect(HTTPResponseParser.find([1, 2], in: [1, 2, 3]) == 0)
        #expect(HTTPResponseParser.find([2, 3], in: [1, 2, 3]) == 1)
        #expect(HTTPResponseParser.find([3, 4], in: [1, 2, 3]) == nil)
        #expect(HTTPResponseParser.find([1, 2, 3, 4], in: [1, 2, 3]) == nil)
        #expect(HTTPResponseParser.find([1, 1, 2], in: [1, 1, 1, 2]) == 1)
    }

    @Test("CRLF splitting tolerates bare LF and keeps stray CR")
    func crlfSplit() {
        #expect("a\r\nb\nc".components(separatedByCRLF: ()) == ["a", "b", "c"])
        #expect("a\rb".components(separatedByCRLF: ()) == ["a\rb"])
        #expect("".components(separatedByCRLF: ()) == [""])
        #expect("x\r".components(separatedByCRLF: ()) == ["x\r"])
    }
}

@Suite("HTTP client")
struct HTTPClientTests {
    @Test("sends the serialized request and buffers the response")
    func sendBuffers() throws {
        let transport = MockTransport(always: .http(body: "pong", chunked: true))
        let client = HTTPClient(transport: transport)
        let response = try client.send(HTTPRequest(method: .get, target: "/_ping"))
        #expect(response.status == 200)
        #expect(response.bodyText == "pong")
        #expect(transport.requests.first?.target == "/_ping")
        #expect(transport.requests.first?.headers["host"] == "docker")
        #expect(transport.connections.allSatisfy { $0.isClosed })
    }

    @Test("small read chunks still reassemble the body")
    func smallReads() throws {
        let body = String(repeating: "x", count: 1000)
        let transport = MockTransport(always: .http(body: body))
        let client = HTTPClient(transport: transport, readChunkSize: 7)
        #expect(try client.send(HTTPRequest(method: .get, target: "/")).bodyText == body)
    }

    @Test("streaming stops when the callback returns false")
    func earlyStop() throws {
        let transport = MockTransport(always: MockResponse(chunks: [
            Array("HTTP/1.1 200 OK\r\n\r\n".utf8), Array("one".utf8), Array("two".utf8)
        ], hangsAfterChunks: true))
        let client = HTTPClient(transport: transport)
        var received: [String] = []
        try client.stream(HTTPRequest(method: .get, target: "/events"), onHead: { _ in }, onBody: { chunk in
            received.append(String(decoding: chunk, as: UTF8.self))
            return received.count < 2
        })
        #expect(received == ["one", "two"])
    }

    @Test("enforces the maximum buffered body size")
    func bodyLimit() {
        let transport = MockTransport(always: .http(body: String(repeating: "y", count: 100)))
        let client = HTTPClient(transport: transport, maxBodySize: 10)
        #expect(throws: DockerError.self) { _ = try client.send(HTTPRequest(method: .get, target: "/")) }
    }

    @Test("connection failures propagate")
    func connectFailure() {
        let transport = MockTransport(always: .http())
        transport.connectError = .connectionFailed(endpoint: "mock", reason: "refused")
        #expect(throws: DockerError.connectionFailed(endpoint: "mock", reason: "refused")) {
            _ = try HTTPClient(transport: transport).send(HTTPRequest(method: .get, target: "/"))
        }
    }

    @Test("a connection closed mid-body is reported")
    func truncated() {
        let transport = MockTransport(always: MockResponse(chunks: [Array("HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort".utf8)]))
        #expect(throws: DockerError.self) { _ = try HTTPClient(transport: transport).send(HTTPRequest(method: .get, target: "/")) }
    }
}
