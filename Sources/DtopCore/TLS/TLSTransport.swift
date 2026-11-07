import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

/// TCP + TLS to a Docker daemon (`--tlsverify`, `DOCKER_TLS_VERIFY`, `https://`). The platform's
/// own TLS stack does the cryptography (ADR 0003):
///
/// - Linux: OpenSSL, loaded at runtime (`OpenSSLConnection`).
/// - macOS: Network.framework (`AppleTLSConnection`).
/// - Windows: SChannel over Winsock (`SChannelConnection`).
public struct TLSTransport: DockerTransport {
    public let host: String
    public let port: Int
    public let configuration: TLSConfiguration
    public let timeoutSeconds: Int

    public init(host: String, port: Int, configuration: TLSConfiguration, timeoutSeconds: Int = 30) {
        self.host = host
        self.port = port
        self.configuration = configuration
        self.timeoutSeconds = timeoutSeconds
    }

    public var endpoint: DockerEndpoint { .tcp(host: host, port: port) }

    /// The name the server certificate must match.
    public var serverName: String { configuration.serverName ?? host }

    public func connect() throws -> any DockerConnection {
        TLSSupport.ignoreBrokenPipes()
        #if canImport(Network)
        return try AppleTLSConnection(host: host, port: port, configuration: configuration, serverName: serverName,
                                      endpoint: endpoint.description, timeoutSeconds: timeoutSeconds)
        #elseif os(Windows)
        let raw = try WinsockTCPTransport(host: host, port: port, timeoutSeconds: timeoutSeconds).connect()
        return try SChannelConnection(underlying: raw, configuration: configuration, serverName: serverName,
                                      endpoint: endpoint.description)
        #else
        let raw = try TCPTransport(host: host, port: port, timeoutSeconds: timeoutSeconds).connect()
        return try OpenSSLConnection(underlying: raw, configuration: configuration, serverName: serverName,
                                     endpoint: endpoint.description)
        #endif
    }
}

extension TransportFactory {
    /// Builds the transport for a host: TLS when the host has TLS settings, otherwise the
    /// endpoint's plain transport.
    public static func make(for host: DockerHost, timeoutSeconds: Int = 30) throws -> any DockerTransport {
        if let tls = host.tls, case .tcp(let name, let port) = host.endpoint {
            return TLSTransport(host: name, port: port, configuration: tls, timeoutSeconds: timeoutSeconds)
        }
        return try make(for: host.endpoint, timeoutSeconds: timeoutSeconds)
    }
}

extension DockerClient {
    /// Builds a client for a host, using TLS when the host asks for it.
    public static func make(host: DockerHost, timeoutSeconds: Int = 30) throws -> DockerClient {
        let transport = try TransportFactory.make(for: host, timeoutSeconds: timeoutSeconds)
        let streaming = try TransportFactory.make(for: host, timeoutSeconds: 0)
        return DockerClient(transport: transport, streamingTransport: streaming)
    }
}

/// Platform-independent pieces of the TLS backends: name checks and error messages that say
/// what to do next.
enum TLSSupport {
    /// A peer that closes while we write (e.g. a daemon rejecting our certificate) must surface
    /// as an EPIPE error, not kill the process with SIGPIPE. dtop's own sockets already opt out,
    /// but the platform TLS stacks write on sockets dtop does not configure.
    static func ignoreBrokenPipes() {
        _ = ignoredBrokenPipes
    }

    private static let ignoredBrokenPipes: Void = {
        #if !os(Windows)
        signal(SIGPIPE, SIG_IGN)
        #endif
    }()

    /// `DTOP_TLS_TRACE=1` logs TLS internals to stderr, for diagnosing handshakes.
    static let traceEnabled = ProcessInfo.processInfo.environment["DTOP_TLS_TRACE"] == "1"

    static func trace(_ message: @autoclosure () -> String) {
        guard traceEnabled else { return }
        FileHandle.standardError.write(Data(("dtop tls: " + message() + "\n").utf8))
    }

    /// Whether `name` is an IPv4 or IPv6 literal (certificates match those against IP SANs).
    static func isIPAddress(_ name: String) -> Bool {
        if name.contains(":") {
            // IPv6, optionally with a zone (fe80::1%en0).
            let address = name.split(separator: "%", maxSplits: 1).first.map(String.init) ?? name
            return !address.isEmpty && address.allSatisfy { $0.isHexDigit || $0 == ":" || $0 == "." }
        }
        let parts = name.split(separator: ".", omittingEmptySubsequences: false)
        return parts.count == 4 && parts.allSatisfy { part in
            !part.isEmpty && part.count <= 3 && part.allSatisfy(\.isASCIIDigit) && (Int(part) ?? 256) <= 255
        }
    }

    static func closedDuringHandshakeHint(_ configuration: TLSConfiguration) -> String {
        if !configuration.hasClientCertificate {
            return "the daemon probably requires a client certificate: pass --tlscert and --tlskey, "
                + "or put cert.pem and key.pem in ~/.docker (or DOCKER_CERT_PATH)"
        }
        return "check that the port serves TLS and that the daemon trusts your client certificate"
    }

    static func nameMismatchMessage(serverName: String, endpoint: String) -> String {
        "the certificate of \(endpoint) is not valid for '\(serverName)' — connect with a name listed in the "
            + "certificate, or reissue the daemon certificate with '\(serverName)' as a subjectAltName"
    }

    static func untrustedMessage(reason: String, endpoint: String, configuration: TLSConfiguration) -> String {
        var message = "could not verify the certificate of \(endpoint) (\(reason)) — "
        if let caFile = configuration.caFile {
            message += "\(caFile) did not issue it; check --tlscacert or DOCKER_CERT_PATH"
        } else {
            message += "pass the CA that issued it with --tlscacert, or put it in ~/.docker/ca.pem"
        }
        return message
    }

    static func handshakeMessage(detail: String, endpoint: String, configuration: TLSConfiguration) -> String {
        let lowered = detail.lowercased()
        if lowered.contains("wrong version number") || lowered.contains("packet length too long")
            || lowered.contains("http request") || lowered.contains("record layer failure") {
            return "\(endpoint) does not speak TLS on this port (\(detail)) — plain-HTTP daemons usually "
                + "listen on 2375; drop --tls/--tlsverify for them"
        }
        if lowered.contains("certificate required")
            || (!configuration.hasClientCertificate && lowered.contains("handshake failure")) {
            return "\(endpoint) requires a client certificate (\(detail)) — "
                + closedDuringHandshakeHint(configuration)
        }
        if lowered.contains("unknown ca") || lowered.contains("bad certificate") || lowered.contains("certificate unknown")
            || lowered.contains("decrypt error") {
            return "\(endpoint) rejected the client certificate (\(detail)) — it must be issued by the CA "
                + "the daemon trusts (dockerd --tlscacert)"
        }
        var message = "TLS handshake with \(endpoint) failed: \(detail)"
        if !configuration.hasClientCertificate {
            message += " — if the daemon requires a client certificate, pass --tlscert and --tlskey, "
                + "or put cert.pem and key.pem in ~/.docker (or DOCKER_CERT_PATH)"
        }
        return message
    }
}

extension Character {
    var isASCIIDigit: Bool { ("0"..."9").contains(self) }
}
