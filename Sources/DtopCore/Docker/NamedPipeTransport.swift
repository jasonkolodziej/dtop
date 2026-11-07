#if os(Windows)
import WinSDK
import Foundation

/// A synchronous handle to a Windows named pipe such as `\\.\pipe\docker_engine`.
final class NamedPipeConnection: DockerConnection, @unchecked Sendable {
    private let lock = NSLock()
    private var handle: HANDLE?

    init(handle: HANDLE) {
        self.handle = handle
    }

    deinit {
        close()
    }

    private func currentHandle() throws -> HANDLE {
        lock.lock()
        defer { lock.unlock() }
        guard let handle else { throw DockerError.cancelled }
        return handle
    }

    func write(_ bytes: [UInt8]) throws {
        let pipe = try currentHandle()
        var offset = 0
        while offset < bytes.count {
            var written: DWORD = 0
            let ok = bytes.withUnsafeBytes { raw in
                WriteFile(pipe, raw.baseAddress! + offset, DWORD(bytes.count - offset), &written, nil)
            }
            guard ok.boolValue else {
                throw DockerError.io("WriteFile failed with Windows error \(GetLastError())")
            }
            offset += Int(written)
        }
    }

    func read(maxLength: Int) throws -> [UInt8] {
        let pipe = try currentHandle()
        var buffer = [UInt8](repeating: 0, count: max(1, maxLength))
        var count: DWORD = 0
        let ok = buffer.withUnsafeMutableBytes { raw in
            ReadFile(pipe, raw.baseAddress!, DWORD(raw.count), &count, nil)
        }
        if !ok.boolValue {
            let error = GetLastError()
            switch error {
            case DWORD(ERROR_BROKEN_PIPE), DWORD(ERROR_PIPE_NOT_CONNECTED):
                return []
            case DWORD(ERROR_OPERATION_ABORTED):
                throw DockerError.cancelled
            case DWORD(ERROR_MORE_DATA):
                break
            default:
                throw DockerError.io("ReadFile failed with Windows error \(error)")
            }
        }
        buffer.removeSubrange(Int(count)...)
        return buffer
    }

    func interrupt() {
        lock.lock()
        defer { lock.unlock() }
        if let handle {
            _ = CancelIoEx(handle, nil)
        }
    }

    func close() {
        lock.lock()
        defer { lock.unlock() }
        if let handle {
            _ = CloseHandle(handle)
        }
        handle = nil
    }
}

/// Connects to Docker Desktop's engine pipe on Windows.
public struct NamedPipeTransport: DockerTransport {
    public let path: String
    public let busyWaitMilliseconds: UInt32

    public init(path: String, busyWaitMilliseconds: UInt32 = 5_000) {
        self.path = path
        self.busyWaitMilliseconds = busyWaitMilliseconds
    }

    public var endpoint: DockerEndpoint { .namedPipe(path: path) }

    public func connect() throws -> any DockerConnection {
        let genericRead: DWORD = 0x8000_0000
        let genericWrite: DWORD = 0x4000_0000
        let openExisting: DWORD = 3
        let invalid = HANDLE(bitPattern: -1)

        for attempt in 0..<2 {
            let handle = path.withCString(encodedAs: UTF16.self) { name in
                CreateFileW(name, genericRead | genericWrite, 0, nil, openExisting, 0, nil)
            }
            if let handle, handle != invalid {
                return NamedPipeConnection(handle: handle)
            }
            let error = GetLastError()
            if error == DWORD(ERROR_PIPE_BUSY), attempt == 0 {
                let available = path.withCString(encodedAs: UTF16.self) { name in
                    WaitNamedPipeW(name, busyWaitMilliseconds)
                }
                if available.boolValue { continue }
            }
            let reason = error == DWORD(ERROR_FILE_NOT_FOUND)
                ? "pipe not found"
                : "Windows error \(error)"
            throw DockerError.connectionFailed(endpoint: endpoint.description, reason: reason)
        }
        throw DockerError.connectionFailed(endpoint: endpoint.description, reason: "pipe busy")
    }
}
#endif
