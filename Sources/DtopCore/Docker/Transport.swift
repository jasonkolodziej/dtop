/// A single bidirectional byte stream to the Docker daemon. Implementations are blocking; the
/// HTTP layer hops off the cooperative thread pool before using them.
public protocol DockerConnection: AnyObject, Sendable {
    /// Writes every byte or throws.
    func write(_ bytes: [UInt8]) throws
    /// Reads up to `maxLength` bytes. An empty result means the peer closed the stream.
    func read(maxLength: Int) throws -> [UInt8]
    /// Unblocks any in-flight `read` from another thread (used for cancelling streams).
    func interrupt()
    /// Releases the underlying handle. Safe to call more than once.
    func close()
}

/// Opens connections to a Docker endpoint.
public protocol DockerTransport: Sendable {
    var endpoint: DockerEndpoint { get }
    func connect() throws -> any DockerConnection
}

/// Builds the right transport for an endpoint on the current platform.
public enum TransportFactory {
    public static func make(for endpoint: DockerEndpoint, timeoutSeconds: Int = 30) throws -> any DockerTransport {
        switch endpoint {
        case .unixSocket(let path):
            #if os(Windows)
            throw DockerError.unsupportedEndpoint("unix://\(path) (use npipe:// on Windows)")
            #else
            return UnixSocketTransport(path: path, timeoutSeconds: timeoutSeconds)
            #endif
        case .tcp(let host, let port):
            #if os(Windows)
            throw DockerError.unsupportedEndpoint("tcp://\(host):\(port) (tcp is not yet supported on Windows)")
            #else
            return TCPTransport(host: host, port: port, timeoutSeconds: timeoutSeconds)
            #endif
        case .namedPipe(let path):
            #if os(Windows)
            return NamedPipeTransport(path: path)
            #else
            throw DockerError.unsupportedEndpoint("npipe://\(path) (named pipes only exist on Windows)")
            #endif
        }
    }
}
