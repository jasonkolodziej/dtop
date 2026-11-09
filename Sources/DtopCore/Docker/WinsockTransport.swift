#if os(Windows)
import Foundation
import WinSDK

/// Winsock must be started once per process before any socket call.
enum Winsock {
    static let started: Bool = {
        var data = WSADATA()
        return WSAStartup(0x0202, &data) == 0
    }()

    static func lastError() -> Int32 { WSAGetLastError() }

    static let invalidSocket = SOCKET.max
    static let timedOut: Int32 = 10060 // WSAETIMEDOUT
    static let shutdownBoth: Int32 = 2 // SD_BOTH
}

/// A connected Winsock TCP socket.
final class WinsockConnection: DockerConnection, @unchecked Sendable {
    private let lock = NSLock()
    private var socketHandle: SOCKET
    private var isClosed = false

    init(socket: SOCKET) {
        self.socketHandle = socket
    }

    deinit {
        close()
    }

    private func current() throws -> SOCKET {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { throw DockerError.cancelled }
        return socketHandle
    }

    func write(_ bytes: [UInt8]) throws {
        let handle = try current()
        var offset = 0
        while offset < bytes.count {
            let sent = bytes.withUnsafeBytes { raw -> Int32 in
                let base = raw.baseAddress!.advanced(by: offset).assumingMemoryBound(to: CChar.self)
                return send(handle, base, Int32(min(raw.count - offset, Int(Int32.max))), 0)
            }
            if sent == SOCKET_ERROR {
                let code = Winsock.lastError()
                if code == Winsock.timedOut { throw DockerError.timedOut("socket write") }
                throw DockerError.io("write failed: Winsock error \(code)")
            }
            offset += Int(sent)
        }
    }

    func read(maxLength: Int) throws -> [UInt8] {
        let handle = try current()
        var buffer = [UInt8](repeating: 0, count: max(1, maxLength))
        let count = buffer.withUnsafeMutableBytes { raw -> Int32 in
            recv(handle, raw.baseAddress!.assumingMemoryBound(to: CChar.self), Int32(min(raw.count, Int(Int32.max))), 0)
        }
        if count == SOCKET_ERROR {
            let code = Winsock.lastError()
            lock.lock()
            let closed = isClosed
            lock.unlock()
            if closed { throw DockerError.cancelled }
            if code == Winsock.timedOut { throw DockerError.timedOut("socket read") }
            throw DockerError.io("read failed: Winsock error \(code)")
        }
        return Array(buffer[0..<Int(count)])
    }

    func interrupt() {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { return }
        _ = shutdown(socketHandle, Winsock.shutdownBoth)
    }

    func close() {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { return }
        isClosed = true
        _ = closesocket(socketHandle)
    }
}

/// Plain TCP on Windows (`tcp://host:2375`), and the carrier for TLS (`SChannelConnection`).
public struct WinsockTCPTransport: DockerTransport {
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
        guard Winsock.started else {
            throw DockerError.connectionFailed(endpoint: endpoint.description, reason: "Winsock could not be started")
        }
        var hints = ADDRINFOA()
        hints.ai_family = AF_UNSPEC
        hints.ai_socktype = SOCK_STREAM
        var results: UnsafeMutablePointer<ADDRINFOA>?
        let status = getaddrinfo(host, String(port), &hints, &results)
        guard status == 0, let first = results else {
            throw DockerError.connectionFailed(endpoint: endpoint.description, reason: "cannot resolve \(host) (error \(status))")
        }
        defer { freeaddrinfo(results) }

        var lastError = "no addresses"
        var cursor: UnsafeMutablePointer<ADDRINFOA>? = first
        while let info = cursor {
            let handle = socket(info.pointee.ai_family, info.pointee.ai_socktype, info.pointee.ai_protocol)
            if handle != Winsock.invalidSocket {
                if timeoutSeconds > 0 {
                    var milliseconds = DWORD(timeoutSeconds * 1000)
                    withUnsafeBytes(of: &milliseconds) { raw in
                        let value = raw.baseAddress!.assumingMemoryBound(to: CChar.self)
                        _ = setsockopt(handle, SOL_SOCKET, SO_RCVTIMEO, value, Int32(raw.count))
                        _ = setsockopt(handle, SOL_SOCKET, SO_SNDTIMEO, value, Int32(raw.count))
                    }
                }
                if WinSDK.connect(handle, info.pointee.ai_addr, Int32(truncatingIfNeeded: info.pointee.ai_addrlen)) == 0 {
                    return WinsockConnection(socket: handle)
                }
                lastError = "Winsock error \(Winsock.lastError())"
                _ = closesocket(handle)
            } else {
                lastError = "Winsock error \(Winsock.lastError())"
            }
            cursor = info.pointee.ai_next
        }
        throw DockerError.connectionFailed(endpoint: endpoint.description, reason: lastError)
    }
}
#endif
