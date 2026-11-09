#if !os(Windows)
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

import Foundation

/// A connected POSIX socket file descriptor.
final class POSIXSocketConnection: DockerConnection, @unchecked Sendable {
    // The lock guards `fd`/`isClosed` so `interrupt()` and `close()` may race with I/O safely.
    private let lock = NSLock()
    private var fd: Int32
    private var isClosed = false

    init(fd: Int32) {
        self.fd = fd
        Self.disableSigPipe(fd)
    }

    deinit {
        close()
    }

    private static func disableSigPipe(_ fd: Int32) {
        #if canImport(Darwin)
        var one: Int32 = 1
        _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
        #endif
    }

    private func currentFD() throws -> Int32 {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { throw DockerError.cancelled }
        return fd
    }

    func write(_ bytes: [UInt8]) throws {
        let descriptor = try currentFD()
        var offset = 0
        while offset < bytes.count {
            let written = bytes.withUnsafeBytes { raw -> Int in
                #if canImport(Darwin)
                return Darwin.send(descriptor, raw.baseAddress!.advanced(by: offset), bytes.count - offset, 0)
                #else
                return send(descriptor, raw.baseAddress!.advanced(by: offset), bytes.count - offset, Int32(MSG_NOSIGNAL))
                #endif
            }
            if written < 0 {
                if errno == EINTR { continue }
                throw DockerError.io("write failed: \(String(cString: strerror(errno)))")
            }
            offset += written
        }
    }

    func read(maxLength: Int) throws -> [UInt8] {
        let descriptor = try currentFD()
        var buffer = [UInt8](repeating: 0, count: max(1, maxLength))
        while true {
            let count = buffer.withUnsafeMutableBytes { raw in
                recv(descriptor, raw.baseAddress!, raw.count, 0)
            }
            if count < 0 {
                if errno == EINTR { continue }
                if errno == EAGAIN || errno == EWOULDBLOCK {
                    throw DockerError.timedOut("socket read")
                }
                lock.lock()
                let closed = isClosed
                lock.unlock()
                if closed { throw DockerError.cancelled }
                throw DockerError.io("read failed: \(String(cString: strerror(errno)))")
            }
            buffer.removeSubrange(count...)
            return buffer
        }
    }

    func interrupt() {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { return }
        // shutdown() (unlike close()) reliably wakes a thread blocked in recv() on Linux and macOS.
        _ = shutdown(fd, Int32(SHUT_RDWR))
    }

    func close() {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { return }
        isClosed = true
        #if canImport(Darwin)
        _ = Darwin.close(fd)
        #elseif canImport(Glibc)
        _ = Glibc.close(fd)
        #elseif canImport(Musl)
        _ = Musl.close(fd)
        #endif
        fd = -1
    }
}

private func socketStreamType() -> Int32 {
    #if canImport(Glibc) || canImport(Musl)
    return Int32(SOCK_STREAM.rawValue)
    #else
    return SOCK_STREAM
    #endif
}

private func applyTimeouts(_ fd: Int32, seconds: Int) {
    guard seconds > 0 else { return }
    var timeout = timeval()
    timeout.tv_sec = time_t(seconds)
    _ = setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    _ = setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
}

private func systemConnect(_ fd: Int32, _ address: UnsafePointer<sockaddr>, _ length: socklen_t) -> Int32 {
    #if canImport(Darwin)
    return Darwin.connect(fd, address, length)
    #elseif canImport(Glibc)
    return Glibc.connect(fd, address, length)
    #else
    return Musl.connect(fd, address, length)
    #endif
}

private func closeDescriptor(_ fd: Int32) {
    #if canImport(Darwin)
    _ = Darwin.close(fd)
    #elseif canImport(Glibc)
    _ = Glibc.close(fd)
    #elseif canImport(Musl)
    _ = Musl.close(fd)
    #endif
}

/// Connects to a Unix domain socket such as `/var/run/docker.sock`.
public struct UnixSocketTransport: DockerTransport {
    public let path: String
    public let timeoutSeconds: Int

    public init(path: String, timeoutSeconds: Int = 30) {
        self.path = path
        self.timeoutSeconds = timeoutSeconds
    }

    public var endpoint: DockerEndpoint { .unixSocket(path: path) }

    /// Used by streaming requests (events, logs -f) which must not time out while idle.
    public func withoutTimeout() -> UnixSocketTransport {
        UnixSocketTransport(path: path, timeoutSeconds: 0)
    }

    public func connect() throws -> any DockerConnection {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard pathBytes.count < capacity else { throw DockerError.socketPathTooLong(path) }

        let fd = socket(AF_UNIX, socketStreamType(), 0)
        guard fd >= 0 else {
            throw DockerError.connectionFailed(endpoint: endpoint.description, reason: String(cString: strerror(errno)))
        }
        withUnsafeMutableBytes(of: &address.sun_path) { raw in
            raw.copyBytes(from: pathBytes)
            raw[pathBytes.count] = 0
        }
        applyTimeouts(fd, seconds: timeoutSeconds)

        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                systemConnect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard result == 0 else {
            let reason = String(cString: strerror(errno))
            closeDescriptor(fd)
            throw DockerError.connectionFailed(endpoint: endpoint.description, reason: reason)
        }
        return POSIXSocketConnection(fd: fd)
    }
}

/// Plain-HTTP TCP transport (e.g. `tcp://127.0.0.1:2375`).
public struct TCPTransport: DockerTransport {
    public let host: String
    public let port: Int
    public let timeoutSeconds: Int

    public init(host: String, port: Int, timeoutSeconds: Int = 30) {
        self.host = host
        self.port = port
        self.timeoutSeconds = timeoutSeconds
    }

    public var endpoint: DockerEndpoint { .tcp(host: host, port: port) }

    public func connect() throws -> any DockerConnection {
        var hints = addrinfo()
        hints.ai_family = AF_UNSPEC
        hints.ai_socktype = socketStreamType()
        var results: UnsafeMutablePointer<addrinfo>?
        let status = getaddrinfo(host, String(port), &hints, &results)
        guard status == 0, let first = results else {
            let reason = status == 0 ? "no addresses" : String(cString: gai_strerror(status))
            throw DockerError.connectionFailed(endpoint: endpoint.description, reason: reason)
        }
        defer { freeaddrinfo(results) }

        var lastError = "no addresses"
        var cursor: UnsafeMutablePointer<addrinfo>? = first
        while let info = cursor {
            let fd = socket(info.pointee.ai_family, info.pointee.ai_socktype, info.pointee.ai_protocol)
            if fd >= 0 {
                applyTimeouts(fd, seconds: timeoutSeconds)
                if systemConnect(fd, info.pointee.ai_addr, info.pointee.ai_addrlen) == 0 {
                    return POSIXSocketConnection(fd: fd)
                }
                lastError = String(cString: strerror(errno))
                closeDescriptor(fd)
            } else {
                lastError = String(cString: strerror(errno))
            }
            cursor = info.pointee.ai_next
        }
        throw DockerError.connectionFailed(endpoint: endpoint.description, reason: lastError)
    }
}
#endif
