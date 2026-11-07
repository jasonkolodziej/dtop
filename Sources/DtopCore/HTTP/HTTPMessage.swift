public enum HTTPMethod: String, Sendable, Equatable {
    case get = "GET"
    case post = "POST"
    case put = "PUT"
    case delete = "DELETE"
    case head = "HEAD"
}

/// Ordered, case-insensitive HTTP header list.
public struct HTTPHeaders: Sendable, Equatable, Sequence {
    public private(set) var entries: [(name: String, value: String)]

    public init(_ entries: [(String, String)] = []) {
        self.entries = entries.map { (name: $0.0, value: $0.1) }
    }

    public subscript(name: String) -> String? {
        let lowered = name.lowercased()
        return entries.first { $0.name.lowercased() == lowered }?.value
    }

    public func values(for name: String) -> [String] {
        let lowered = name.lowercased()
        return entries.filter { $0.name.lowercased() == lowered }.map(\.value)
    }

    public mutating func add(_ name: String, _ value: String) {
        entries.append((name: name, value: value))
    }

    /// Replaces every header with this name.
    public mutating func set(_ name: String, _ value: String) {
        let lowered = name.lowercased()
        entries.removeAll { $0.name.lowercased() == lowered }
        entries.append((name: name, value: value))
    }

    public func makeIterator() -> IndexingIterator<[(name: String, value: String)]> {
        entries.makeIterator()
    }

    public static func == (lhs: HTTPHeaders, rhs: HTTPHeaders) -> Bool {
        lhs.entries.count == rhs.entries.count
            && zip(lhs.entries, rhs.entries).allSatisfy { $0.name == $1.name && $0.value == $1.value }
    }
}

public struct HTTPRequest: Sendable, Equatable {
    public var method: HTTPMethod
    /// Path including the query string, e.g. `/v1.43/containers/json?all=1`.
    public var target: String
    public var headers: HTTPHeaders
    public var body: [UInt8]

    public init(method: HTTPMethod, target: String, headers: HTTPHeaders = HTTPHeaders(), body: [UInt8] = []) {
        self.method = method
        self.target = target
        self.headers = headers
        self.body = body
    }

    /// Builds a request target from a path and ordered query items; `nil` values are omitted.
    public static func target(_ path: String, query: [(String, String?)] = []) -> String {
        let items = query.compactMap { key, value -> String? in
            guard let value else { return nil }
            return percentEncode(key) + "=" + percentEncode(value)
        }
        return items.isEmpty ? path : path + "?" + items.joined(separator: "&")
    }

    /// RFC 3986 percent-encoding that only leaves unreserved characters untouched.
    public static func percentEncode(_ value: String) -> String {
        var result = ""
        for byte in value.utf8 {
            switch byte {
            case UInt8(ascii: "A")...UInt8(ascii: "Z"), UInt8(ascii: "a")...UInt8(ascii: "z"),
                 UInt8(ascii: "0")...UInt8(ascii: "9"), UInt8(ascii: "-"), UInt8(ascii: "."),
                 UInt8(ascii: "_"), UInt8(ascii: "~"):
                result.append(Character(Unicode.Scalar(byte)))
            default:
                let hex = Array("0123456789ABCDEF")
                result.append("%")
                result.append(hex[Int(byte >> 4)])
                result.append(hex[Int(byte & 0x0F)])
            }
        }
        return result
    }

    /// Serializes to HTTP/1.1 wire format. We always ask the daemon to close the connection,
    /// which keeps the client simple: one request per connection, EOF terminates the body.
    public func serialized(host: String) -> [UInt8] {
        var head = "\(method.rawValue) \(target) HTTP/1.1\r\n"
        var headers = self.headers
        if headers["Host"] == nil { headers.set("Host", host) }
        if headers["User-Agent"] == nil { headers.set("User-Agent", "dtop") }
        if headers["Connection"] == nil { headers.set("Connection", "close") }
        if !body.isEmpty || method == .post || method == .put {
            headers.set("Content-Length", String(body.count))
        }
        for header in headers {
            head += "\(header.name): \(header.value)\r\n"
        }
        head += "\r\n"
        return Array(head.utf8) + body
    }
}

public struct HTTPResponseHead: Sendable, Equatable {
    public var status: Int
    public var reason: String
    public var headers: HTTPHeaders

    public init(status: Int, reason: String, headers: HTTPHeaders) {
        self.status = status
        self.reason = reason
        self.headers = headers
    }

    public var isSuccess: Bool { (200..<300).contains(status) }
}

public struct HTTPResponse: Sendable, Equatable {
    public var head: HTTPResponseHead
    public var body: [UInt8]

    public init(head: HTTPResponseHead, body: [UInt8]) {
        self.head = head
        self.body = body
    }

    public var status: Int { head.status }
    public var bodyText: String { String(decoding: body, as: UTF8.self) }
}
