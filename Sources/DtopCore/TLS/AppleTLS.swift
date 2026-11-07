#if canImport(Network)
import Foundation
import Network
import Security

/// TLS on macOS through Network.framework, which owns the socket. Reads and writes are blocking
/// (semaphores over Network.framework's callbacks) to fit dtop's `DockerConnection` model.
///
/// - Verification: a verify block evaluates the server's trust with an SSL policy for the server
///   name, anchored to `--tlscacert` when given (only that CA is trusted), else the system roots.
/// - Client certificates: the PEM certificate and key become a `SecIdentity` in memory; nothing
///   is written to the keychain.
final class AppleTLSConnection: DockerConnection, @unchecked Sendable {
    private let connection: NWConnection
    private let queue = DispatchQueue(label: "dtop.tls")
    private let timeoutSeconds: Int
    private let configuration: TLSConfiguration
    private let endpoint: String
    private let serverName: String
    private let state = ConnectionState()

    /// Mutable state shared with Network.framework's callbacks.
    private final class ConnectionState: @unchecked Sendable {
        let lock = NSLock()
        var trustFailure: CFError?
        var cancelled = false
        var receivedAny = false
        /// Set when the connection fails after it was ready (e.g. the daemon rejected us).
        var failure: NWError?

        var currentFailure: NWError? {
            lock.lock()
            defer { lock.unlock() }
            return failure
        }

        func set(trustFailure error: CFError?) {
            lock.lock()
            trustFailure = error
            lock.unlock()
        }
    }

    /// Holds a value produced on Network.framework's queue for the waiting thread.
    private final class Outcome<Value>: @unchecked Sendable {
        let semaphore = DispatchSemaphore(value: 0)
        var value: Value?

        func finish(_ value: Value) {
            self.value = value
            semaphore.signal()
        }

        /// Waits; nil means the timeout passed first, or `abort` became true (checked every
        /// 100 ms, because a failed connection does not always complete pending operations).
        func wait(seconds: Int, abort: () -> Bool = { false }) -> Value? {
            let deadline = seconds > 0 ? DispatchTime.now() + .seconds(seconds) : DispatchTime.distantFuture
            while true {
                if semaphore.wait(timeout: min(deadline, .now() + .milliseconds(100))) == .success { return value }
                if abort() || DispatchTime.now() >= deadline { return nil }
            }
        }
    }

    /// A Security object that crosses into Network.framework's queue.
    private struct Unchecked<Value>: @unchecked Sendable {
        let value: Value
    }

    init(host: String, port: Int, configuration: TLSConfiguration, serverName: String, endpoint: String,
         timeoutSeconds: Int) throws {
        self.timeoutSeconds = timeoutSeconds
        self.configuration = configuration
        self.endpoint = endpoint
        self.serverName = serverName

        let tls = NWProtocolTLS.Options()
        let security = tls.securityProtocolOptions
        sec_protocol_options_set_min_tls_protocol_version(security, .TLSv12)
        if !TLSSupport.isIPAddress(serverName) {
            sec_protocol_options_set_tls_server_name(security, serverName)
        }
        if let identity = try Self.identity(for: configuration) {
            sec_protocol_options_set_local_identity(security, identity)
        }
        let anchors = try configuration.caFile.map { file in Unchecked(value: try Self.certificates(file: file)) }
        let verifyPeer = configuration.verifyPeer
        let state = self.state
        sec_protocol_options_set_verify_block(security, { _, trust, complete in
            guard verifyPeer else {
                complete(true)
                return
            }
            let secTrust = sec_trust_copy_ref(trust).takeRetainedValue()
            SecTrustSetPolicies(secTrust, SecPolicyCreateSSL(true, serverName as CFString))
            if let anchors {
                SecTrustSetAnchorCertificates(secTrust, anchors.value as CFArray)
                SecTrustSetAnchorCertificatesOnly(secTrust, true)
            }
            var error: CFError?
            let trusted = SecTrustEvaluateWithError(secTrust, &error)
            state.set(trustFailure: trusted ? nil : error)
            complete(trusted)
        }, queue)

        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        if timeoutSeconds > 0 { tcp.connectionTimeout = timeoutSeconds }
        guard let nwPort = NWEndpoint.Port(rawValue: UInt16(clamping: port)) else {
            throw DockerError.invalidEndpoint(endpoint)
        }
        connection = NWConnection(host: NWEndpoint.Host(host), port: nwPort, using: NWParameters(tls: tls, tcp: tcp))

        let ready = Outcome<NWError?>()
        connection.stateUpdateHandler = { newState in
            switch newState {
            case .ready:
                ready.finish(nil)
            case .failed(let error), .waiting(let error):
                // `.waiting` means "no route / refused, will retry": dtop reports and retries itself.
                ready.finish(error)
            case .cancelled:
                ready.finish(NWError.posix(.ECANCELED))
            default:
                break
            }
        }
        connection.start(queue: queue)
        let waitSeconds = timeoutSeconds > 0 ? timeoutSeconds + 5 : 60
        guard let outcome = ready.wait(seconds: waitSeconds) else {
            connection.cancel()
            throw DockerError.connectionFailed(endpoint: endpoint, reason: "timed out")
        }
        if let error = outcome {
            connection.stateUpdateHandler = nil
            connection.cancel()
            throw describe(error, duringHandshake: true)
        }
        // Keep watching: a daemon that rejects our certificate fails the connection after `.ready`.
        let watched = self.state
        connection.stateUpdateHandler = { newState in
            if case .failed(let error) = newState {
                watched.lock.lock()
                watched.failure = error
                watched.lock.unlock()
            }
        }
    }

    deinit {
        connection.stateUpdateHandler = nil
        connection.cancel()
    }

    // MARK: - I/O

    func write(_ bytes: [UInt8]) throws {
        let done = Outcome<NWError?>()
        connection.send(content: Data(bytes), completion: .contentProcessed { error in done.finish(error) })
        guard let outcome = done.wait(seconds: timeoutSeconds, abort: { state.currentFailure != nil }) else {
            connection.cancel()
            if let failure = state.currentFailure { throw failedConnection(failure) }
            throw DockerError.timedOut("socket write")
        }
        if let error = outcome {
            state.lock.lock()
            let receivedAny = state.receivedAny
            state.lock.unlock()
            // A daemon that refuses our client certificate drops the connection right after the
            // handshake, so the request write is what fails.
            if !receivedAny, case .posix(let code) = error, [.ENOTCONN, .EPIPE, .ECONNRESET].contains(code) {
                throw DockerError.tls("\(endpoint) closed the connection before answering (\(error)) — "
                    + TLSSupport.closedDuringHandshakeHint(configuration))
            }
            throw describe(error, duringHandshake: false)
        }
    }

    func read(maxLength: Int) throws -> [UInt8] {
        let done = Outcome<(Data?, Bool, NWError?)>()
        connection.receive(minimumIncompleteLength: 1, maximumLength: max(1, maxLength)) { data, _, isComplete, error in
            done.finish((data, isComplete, error))
        }
        guard let (data, isComplete, error) = done.wait(seconds: timeoutSeconds, abort: { state.currentFailure != nil }) else {
            connection.cancel()
            if let failure = state.currentFailure { throw failedConnection(failure) }
            throw DockerError.timedOut("socket read")
        }
        state.lock.lock()
        let cancelled = state.cancelled
        let receivedAny = state.receivedAny
        if let data, !data.isEmpty { state.receivedAny = true }
        state.lock.unlock()
        if let data, !data.isEmpty { return [UInt8](data) }
        if cancelled { throw DockerError.cancelled }
        if !receivedAny {
            // With TLS 1.3 a daemon that rejects (or misses) our client certificate does so after
            // our side of the handshake, by closing before it answers anything.
            let detail = error.map { String(describing: $0) } ?? "connection closed"
            throw DockerError.tls("\(endpoint) closed the connection before answering (\(detail)) — "
                + TLSSupport.closedDuringHandshakeHint(configuration))
        }
        if let error {
            // A peer that resets after sending everything still ends the stream.
            if case .posix(let code) = error, code == .ECONNRESET || code == .ENOTCONN { return [] }
            throw describe(error, duringHandshake: false)
        }
        _ = isComplete
        return []
    }

    func interrupt() {
        state.lock.lock()
        state.cancelled = true
        state.lock.unlock()
        connection.cancel()
    }

    func close() {
        interrupt()
    }

    // MARK: - Errors

    /// The connection failed after it was ready: before any response, that is the daemon
    /// refusing us (usually the client certificate); later, an ordinary I/O failure.
    private func failedConnection(_ error: NWError) -> DockerError {
        state.lock.lock()
        let receivedAny = state.receivedAny
        state.lock.unlock()
        if !receivedAny {
            return .tls("\(endpoint) closed the connection before answering (\(error)) — "
                + TLSSupport.closedDuringHandshakeHint(configuration))
        }
        return describe(error, duringHandshake: false)
    }

    private func describe(_ error: NWError, duringHandshake: Bool) -> DockerError {
        state.lock.lock()
        let trustFailure = state.trustFailure
        state.lock.unlock()
        if let trustFailure {
            let code = CFErrorGetCode(trustFailure)
            let reason = (CFErrorCopyDescription(trustFailure) as String?) ?? "error \(code)"
            if code == Int(errSecHostNameMismatch) || reason.lowercased().contains("does not match") {
                return .tls(TLSSupport.nameMismatchMessage(serverName: serverName, endpoint: endpoint))
            }
            return .tls(TLSSupport.untrustedMessage(reason: reason, endpoint: endpoint, configuration: configuration))
        }
        switch error {
        case .tls(let status):
            let text = (SecCopyErrorMessageString(status, nil) as String?) ?? "OSStatus \(status)"
            return .tls(TLSSupport.handshakeMessage(detail: "\(text) (\(status))", endpoint: endpoint, configuration: configuration))
        case .posix(let code) where duringHandshake:
            if code == .ECONNREFUSED || code == .EHOSTUNREACH || code == .ENETUNREACH || code == .ETIMEDOUT {
                return .connectionFailed(endpoint: endpoint, reason: String(describing: error))
            }
            return .tls(TLSSupport.handshakeMessage(detail: String(describing: error), endpoint: endpoint,
                                                    configuration: configuration))
        case .dns:
            return .connectionFailed(endpoint: endpoint, reason: String(describing: error))
        default:
            return .io(String(describing: error))
        }
    }

    // MARK: - Certificates and identity

    static func certificates(file: String) throws -> [SecCertificate] {
        guard let text = readFile(file) else { throw DockerError.tls("could not read the certificate file \(file)") }
        return try PEM.certificates(text, file: file).map { der in
            guard let certificate = SecCertificateCreateWithData(nil, Data(der) as CFData) else {
                throw DockerError.tls("\(file) contains a certificate macOS cannot parse")
            }
            return certificate
        }
    }

    /// Builds the client identity from PEM files, keeping it out of the keychain. Cached per
    /// configuration because every request opens a new connection.
    static func identity(for configuration: TLSConfiguration) throws -> sec_identity_t? {
        guard let certificateFile = configuration.certificateFile, let keyFile = configuration.keyFile else { return nil }
        identityLock.lock()
        defer { identityLock.unlock() }
        let cacheKey = certificateFile + "\u{0}" + keyFile
        if let cached = identityCache[cacheKey] { return cached.value }

        let chain = try certificates(file: certificateFile)
        guard let keyText = readFile(keyFile) else { throw DockerError.tls("could not read the client key \(keyFile)") }
        let key = try PrivateKey.parse(pem: keyText, file: keyFile)
        let keyType: CFString
        let keyData: [UInt8]
        switch key.kind {
        case .rsa:
            keyType = kSecAttrKeyTypeRSA
            keyData = key.keyDER
        case .ec:
            guard let x963 = key.x963Representation else {
                throw DockerError.tls("\(keyFile) is an EC key without its public part; "
                    + "rewrite it with: openssl ec -in \(keyFile) -out \(keyFile)")
            }
            keyType = kSecAttrKeyTypeECSECPrimeRandom
            keyData = x963
        }
        let attributes: [CFString: Any] = [kSecAttrKeyType: keyType, kSecAttrKeyClass: kSecAttrKeyClassPrivate]
        var error: Unmanaged<CFError>?
        guard let secKey = SecKeyCreateWithData(Data(keyData) as CFData, attributes as CFDictionary, &error) else {
            let reason = error.map { ($0.takeRetainedValue() as Error).localizedDescription } ?? "unknown error"
            throw DockerError.tls("macOS could not load the client key \(keyFile): \(reason)")
        }
        let identity = try makeIdentity(certificate: chain[0], key: secKey, files: (certificateFile, keyFile))
        let secIdentity: sec_identity_t?
        if chain.count > 1 {
            secIdentity = sec_identity_create_with_certificates(identity, Array(chain.dropFirst()) as CFArray)
        } else {
            secIdentity = sec_identity_create(identity)
        }
        guard let secIdentity else { throw DockerError.tls("could not build a TLS identity from \(certificateFile)") }
        identityCache[cacheKey] = Unchecked(value: secIdentity)
        return secIdentity
    }

    private static let identityLock = NSLock()
    nonisolated(unsafe) private static var identityCache: [String: Unchecked<sec_identity_t>] = [:]

    /// `SecIdentityCreate` pairs a certificate with an in-memory key. Security.framework exports
    /// it (it backs `SecPKCS12Import`) but the public headers do not declare it, so it is looked
    /// up at runtime; the public alternatives all require importing the key into a keychain.
    private static func makeIdentity(certificate: SecCertificate, key: SecKey, files: (String, String)) throws -> SecIdentity {
        typealias Create = @convention(c) (CFAllocator?, SecCertificate, SecKey) -> Unmanaged<SecIdentity>?
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecIdentityCreate") else { // RTLD_DEFAULT
            throw DockerError.tls("this macOS version cannot use client certificates from files")
        }
        let create = unsafeBitCast(symbol, to: Create.self)
        guard let identity = create(kCFAllocatorDefault, certificate, key)?.takeRetainedValue() else {
            throw DockerError.tls("the client certificate \(files.0) and key \(files.1) do not belong together")
        }
        // Make sure the key really matches the certificate before the daemon tells us otherwise.
        if let certificateKey = SecCertificateCopyKey(certificate), let publicKey = SecKeyCopyPublicKey(key),
           let a = SecKeyCopyExternalRepresentation(certificateKey, nil) as Data?,
           let b = SecKeyCopyExternalRepresentation(publicKey, nil) as Data?, a != b {
            throw DockerError.tls("the client certificate \(files.0) and key \(files.1) do not belong together")
        }
        return identity
    }

    private static func readFile(_ path: String) -> String? {
        FileManager.default.contents(atPath: path).map { String(decoding: $0, as: UTF8.self) }
    }
}
#endif
