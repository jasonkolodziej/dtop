public enum HTTPParserEvent: Sendable, Equatable {
    case head(HTTPResponseHead)
    case body([UInt8])
    case end
}

/// Incremental HTTP/1.1 response parser. Feed it bytes as they arrive; it emits the head once,
/// then body fragments (already de-chunked), then `.end`. Supports Content-Length, chunked
/// transfer encoding and read-until-close bodies, which covers everything dockerd sends.
public struct HTTPResponseParser: Sendable {
    private enum State: Sendable {
        case head
        case fixed(remaining: Int)
        case chunkSize
        case chunkData(remaining: Int)
        case chunkDataCRLF
        case trailers
        case untilClose
        case done
    }

    /// Upper bound on header size, so a misbehaving peer cannot make us buffer forever.
    public static let maxHeadSize = 64 * 1024

    private var state: State = .head
    private var buffer: [UInt8] = []
    private let isHeadRequest: Bool

    public init(isHeadRequest: Bool = false) {
        self.isHeadRequest = isHeadRequest
    }

    public var isComplete: Bool {
        if case .done = state { return true }
        return false
    }

    public mutating func feed(_ bytes: [UInt8]) throws -> [HTTPParserEvent] {
        buffer.append(contentsOf: bytes)
        var events: [HTTPParserEvent] = []
        var progressed = true
        while progressed {
            progressed = try step(&events)
        }
        return events
    }

    /// Call when the connection reports EOF.
    public mutating func finish() throws -> [HTTPParserEvent] {
        switch state {
        case .done:
            return []
        case .untilClose:
            var events: [HTTPParserEvent] = []
            if !buffer.isEmpty {
                events.append(.body(buffer))
                buffer.removeAll()
            }
            state = .done
            events.append(.end)
            return events
        case .head:
            throw DockerError.malformedResponse(buffer.isEmpty
                ? "connection closed before any response was received"
                : "connection closed in the middle of the response headers")
        default:
            throw DockerError.malformedResponse("connection closed before the response body was complete")
        }
    }

    private mutating func step(_ events: inout [HTTPParserEvent]) throws -> Bool {
        switch state {
        case .head:
            guard let end = Self.find(Self.crlfcrlf, in: buffer) else {
                if buffer.count > Self.maxHeadSize {
                    throw DockerError.malformedResponse("response headers exceed \(Self.maxHeadSize) bytes")
                }
                return false
            }
            let head = try Self.parseHead(Array(buffer[..<end]))
            buffer.removeSubrange(..<(end + 4))
            events.append(.head(head))
            state = try bodyState(for: head)
            if case .done = state { events.append(.end) }
            return true

        case .fixed(let remaining):
            guard !buffer.isEmpty else { return false }
            let take = min(remaining, buffer.count)
            events.append(.body(Array(buffer[..<take])))
            buffer.removeSubrange(..<take)
            if remaining - take == 0 {
                state = .done
                events.append(.end)
            } else {
                state = .fixed(remaining: remaining - take)
            }
            return true

        case .chunkSize:
            guard let lineEnd = Self.find(Self.crlf, in: buffer) else {
                if buffer.count > 1024 { throw DockerError.malformedResponse("chunk size line too long") }
                return false
            }
            let line = String(decoding: buffer[..<lineEnd], as: UTF8.self)
            buffer.removeSubrange(..<(lineEnd + 2))
            // Chunk extensions (";name=value") are legal and ignored.
            let sizeText = line.split(separator: ";", maxSplits: 1).first.map(String.init)?.trimmingWhitespace() ?? ""
            guard let size = Int(sizeText, radix: 16), size >= 0 else {
                throw DockerError.malformedResponse("invalid chunk size '\(line)'")
            }
            state = size == 0 ? .trailers : .chunkData(remaining: size)
            return true

        case .chunkData(let remaining):
            guard !buffer.isEmpty else { return false }
            let take = min(remaining, buffer.count)
            events.append(.body(Array(buffer[..<take])))
            buffer.removeSubrange(..<take)
            state = remaining - take == 0 ? .chunkDataCRLF : .chunkData(remaining: remaining - take)
            return true

        case .chunkDataCRLF:
            guard buffer.count >= 2 else { return false }
            guard buffer[0] == 13, buffer[1] == 10 else {
                throw DockerError.malformedResponse("missing CRLF after chunk data")
            }
            buffer.removeSubrange(..<2)
            state = .chunkSize
            return true

        case .trailers:
            guard let lineEnd = Self.find(Self.crlf, in: buffer) else { return false }
            let isBlank = lineEnd == 0
            buffer.removeSubrange(..<(lineEnd + 2))
            if isBlank {
                state = .done
                events.append(.end)
            }
            return true

        case .untilClose:
            guard !buffer.isEmpty else { return false }
            events.append(.body(buffer))
            buffer.removeAll()
            return true

        case .done:
            return false
        }
    }

    private func bodyState(for head: HTTPResponseHead) throws -> State {
        if isHeadRequest || head.status == 204 || head.status == 304 || (100..<200).contains(head.status) {
            return .done
        }
        if let encoding = head.headers["Transfer-Encoding"], encoding.lowercased().contains("chunked") {
            return .chunkSize
        }
        if let lengthText = head.headers["Content-Length"] {
            guard let length = Int(lengthText.trimmingWhitespace()), length >= 0 else {
                throw DockerError.malformedResponse("invalid Content-Length '\(lengthText)'")
            }
            return length == 0 ? .done : .fixed(remaining: length)
        }
        return .untilClose
    }

    static let crlf: [UInt8] = [13, 10]
    static let crlfcrlf: [UInt8] = [13, 10, 13, 10]

    static func find(_ needle: [UInt8], in haystack: [UInt8]) -> Int? {
        guard haystack.count >= needle.count else { return nil }
        var index = 0
        let last = haystack.count - needle.count
        while index <= last {
            if haystack[index] == needle[0] {
                var matched = true
                for offset in 1..<needle.count where haystack[index + offset] != needle[offset] {
                    matched = false
                    break
                }
                if matched { return index }
            }
            index += 1
        }
        return nil
    }

    static func parseHead(_ bytes: [UInt8]) throws -> HTTPResponseHead {
        let text = String(decoding: bytes, as: UTF8.self)
        var lines = text.components(separatedByCRLF: ())
        guard !lines.isEmpty else { throw DockerError.malformedResponse("empty status line") }
        let statusLine = lines.removeFirst()
        let parts = statusLine.split(separator: " ", maxSplits: 2, omittingEmptySubsequences: true)
        guard parts.count >= 2, parts[0].hasPrefix("HTTP/"), let status = Int(parts[1]), (100...999).contains(status) else {
            throw DockerError.malformedResponse("invalid status line '\(statusLine)'")
        }
        let reason = parts.count > 2 ? String(parts[2]) : ""
        var headers = HTTPHeaders()
        for line in lines where !line.isEmpty {
            guard let colon = line.firstIndex(of: ":") else {
                throw DockerError.malformedResponse("invalid header line '\(line)'")
            }
            let name = String(line[..<colon]).trimmingWhitespace()
            let value = String(line[line.index(after: colon)...]).trimmingWhitespace()
            guard !name.isEmpty else { throw DockerError.malformedResponse("empty header name") }
            headers.add(name, value)
        }
        return HTTPResponseHead(status: status, reason: reason, headers: headers)
    }
}

extension String {
    /// Splits on CRLF (and tolerates bare LF) without pulling in Foundation.
    func components(separatedByCRLF _: Void) -> [String] {
        var result: [String] = []
        var current = ""
        var previousWasCR = false
        for scalar in unicodeScalars {
            if scalar == "\n" {
                result.append(current)
                current = ""
                previousWasCR = false
                continue
            }
            if previousWasCR { current.unicodeScalars.append("\r") }
            previousWasCR = scalar == "\r"
            if !previousWasCR { current.unicodeScalars.append(scalar) }
        }
        if previousWasCR { current.unicodeScalars.append("\r") }
        result.append(current)
        return result
    }
}
