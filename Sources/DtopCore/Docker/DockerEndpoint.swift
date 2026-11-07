/// Where the Docker Engine API lives.
public enum DockerEndpoint: Equatable, Sendable, CustomStringConvertible {
    case unixSocket(path: String)
    case namedPipe(path: String)
    case tcp(host: String, port: Int)

    public static let defaultTCPPort = 2375
    /// Docker's conventional port for TLS (`--tlsverify`) daemons.
    public static let defaultTLSPort = 2376
    public static let defaultWindowsPipe = #"\\.\pipe\docker_engine"#
    public static let defaultUnixSocket = "/var/run/docker.sock"

    public var description: String {
        switch self {
        case .unixSocket(let path):
            return "unix://\(path)"
        case .namedPipe(let path):
            return "npipe://\(path.replacingBackslashes())"
        case .tcp(let host, let port):
            let formattedHost = host.contains(":") ? "[\(host)]" : host
            return "tcp://\(formattedHost):\(port)"
        }
    }

    /// Value sent in the HTTP `Host` header. Docker ignores it for local transports but HTTP/1.1
    /// requires one.
    public var hostHeader: String {
        switch self {
        case .tcp(let host, let port):
            let formattedHost = host.contains(":") ? "[\(host)]" : host
            return "\(formattedHost):\(port)"
        default:
            return "docker"
        }
    }

    /// The host name or address of a TCP endpoint (nil for local transports).
    public var tcpHost: String? {
        if case .tcp(let host, _) = self { return host }
        return nil
    }

    /// Whether `raw` names an endpoint that is TLS by its scheme (`https://`).
    public static func schemeImpliesTLS(_ raw: String) -> Bool {
        raw.trimmingWhitespace().lowercased().hasPrefix("https://")
    }

    /// Parses `DOCKER_HOST`-style strings. A TCP address without a port gets 2376 when `tls` is
    /// true (or the scheme is `https://`) and 2375 otherwise, as the Docker CLI does.
    public static func parse(_ raw: String, tls: Bool = false) throws -> DockerEndpoint {
        let value = raw.trimmingWhitespace()
        guard !value.isEmpty else { throw DockerError.invalidEndpoint(raw) }

        if let rest = value.dropPrefix("unix://") {
            guard !rest.isEmpty else { throw DockerError.invalidEndpoint(raw) }
            return .unixSocket(path: rest)
        }
        if let rest = value.dropPrefix("npipe://") {
            // npipe:////./pipe/docker_engine -> \\.\pipe\docker_engine
            let normalized = rest.replacingOccurrencesOf("/", with: "\\")
            guard normalized.hasPrefix(#"\\"#), normalized.count > 2 else { throw DockerError.invalidEndpoint(raw) }
            return .namedPipe(path: normalized)
        }
        if let rest = value.dropPrefix("tcp://") ?? value.dropPrefix("http://") {
            return try parseHostPort(rest, original: raw, defaultPort: tls ? defaultTLSPort : defaultTCPPort)
        }
        if let rest = value.dropPrefix("https://") {
            return try parseHostPort(rest, original: raw, defaultPort: defaultTLSPort)
        }
        if value.hasPrefix("ssh://") {
            throw DockerError.unsupportedEndpoint(raw)
        }
        if value.contains("://") {
            throw DockerError.invalidEndpoint(raw)
        }
        // Bare absolute paths are accepted as a convenience.
        if value.hasPrefix("/") {
            return .unixSocket(path: value)
        }
        if value.hasPrefix(#"\\"#) {
            return .namedPipe(path: value)
        }
        throw DockerError.invalidEndpoint(raw)
    }

    private static func parseHostPort(_ rest: String, original: String, defaultPort: Int) throws -> DockerEndpoint {
        var authority = rest
        if let slash = authority.firstIndex(of: "/") {
            let path = authority[slash...]
            guard path == "/" else { throw DockerError.invalidEndpoint(original) }
            authority = String(authority[..<slash])
        }
        guard !authority.isEmpty else { throw DockerError.invalidEndpoint(original) }

        var host: String
        var portText: String?
        if authority.hasPrefix("[") {
            guard let close = authority.firstIndex(of: "]") else { throw DockerError.invalidEndpoint(original) }
            host = String(authority[authority.index(after: authority.startIndex)..<close])
            let after = authority[authority.index(after: close)...]
            if !after.isEmpty {
                guard after.hasPrefix(":") else { throw DockerError.invalidEndpoint(original) }
                portText = String(after.dropFirst())
            }
        } else if let colon = authority.lastIndex(of: ":") {
            host = String(authority[..<colon])
            portText = String(authority[authority.index(after: colon)...])
        } else {
            host = authority
        }
        if host.isEmpty { host = "127.0.0.1" }

        var port = defaultPort
        if let portText {
            guard let parsed = Int(portText), (1...65_535).contains(parsed) else {
                throw DockerError.invalidEndpoint(original)
            }
            port = parsed
        }
        return .tcp(host: host, port: port)
    }

    /// Picks the endpoint to use: an explicit override, then `DOCKER_HOST`, then the first
    /// well-known socket that exists for the platform.
    public static func resolve(
        override: String? = nil,
        environment: [String: String],
        platform: Platform = .current,
        homeDirectory: String?,
        fileExists: (String) -> Bool
    ) throws -> DockerEndpoint {
        if let override, !override.trimmingWhitespace().isEmpty {
            return try parse(override)
        }
        if let host = environment["DOCKER_HOST"], !host.trimmingWhitespace().isEmpty {
            return try parse(host)
        }
        if platform == .windows {
            return .namedPipe(path: defaultWindowsPipe)
        }
        let candidates = candidateSockets(environment: environment, platform: platform, homeDirectory: homeDirectory)
        if let existing = candidates.first(where: fileExists) {
            return .unixSocket(path: existing)
        }
        return .unixSocket(path: defaultUnixSocket)
    }

    /// Well-known socket locations in priority order.
    public static func candidateSockets(
        environment: [String: String],
        platform: Platform,
        homeDirectory: String?
    ) -> [String] {
        var candidates = [defaultUnixSocket]
        switch platform {
        case .macOS:
            if let home = homeDirectory {
                candidates.append(home + "/.docker/run/docker.sock")
                candidates.append(home + "/.orbstack/run/docker.sock")
                candidates.append(home + "/.colima/default/docker.sock")
                candidates.append(home + "/.colima/docker.sock")
                candidates.append(home + "/.rd/docker.sock")
            }
        case .linux, .other:
            if let runtime = environment["XDG_RUNTIME_DIR"], !runtime.isEmpty {
                candidates.append(runtime + "/docker.sock")
            }
            if let home = homeDirectory {
                candidates.append(home + "/.docker/desktop/docker.sock")
            }
        case .windows:
            return []
        }
        return candidates
    }
}

extension String {
    func dropPrefix(_ prefix: String) -> String? {
        guard hasPrefix(prefix) else { return nil }
        return String(dropFirst(prefix.count))
    }

    func trimmingWhitespace() -> String {
        var scalars = Substring(self)
        while let first = scalars.first, first.isWhitespace { scalars = scalars.dropFirst() }
        while let last = scalars.last, last.isWhitespace { scalars = scalars.dropLast() }
        return String(scalars)
    }

    func replacingOccurrencesOf(_ target: Character, with replacement: String) -> String {
        var result = ""
        result.reserveCapacity(count)
        for character in self {
            if character == target { result += replacement } else { result.append(character) }
        }
        return result
    }

    func replacingBackslashes() -> String {
        replacingOccurrencesOf("\\", with: "/")
    }
}
