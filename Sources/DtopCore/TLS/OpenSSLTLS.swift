#if !canImport(Darwin) && !os(Windows)
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif
import Foundation

/// OpenSSL, loaded at runtime with `dlopen` so dtop has no build-time dependency on it (ADR 0003).
/// Every Linux distribution ships libssl; dtop needs 1.1.1 or 3.x. The API used here is all
/// opaque pointers and plain C functions, so no struct layouts are declared by hand.
final class OpenSSLLibrary: @unchecked Sendable {
    typealias Method = @convention(c) () -> OpaquePointer?
    typealias CTXNew = @convention(c) (OpaquePointer?) -> OpaquePointer?
    typealias Free = @convention(c) (OpaquePointer?) -> Void
    typealias Ctrl = @convention(c) (OpaquePointer?, Int32, Int, UnsafeMutableRawPointer?) -> Int
    typealias SetVerify = @convention(c) (OpaquePointer?, Int32, OpaquePointer?) -> Void
    typealias LoadLocations = @convention(c) (OpaquePointer?, UnsafePointer<CChar>?, UnsafePointer<CChar>?) -> Int32
    typealias WithContext = @convention(c) (OpaquePointer?) -> Int32
    typealias WithFile = @convention(c) (OpaquePointer?, UnsafePointer<CChar>?) -> Int32
    typealias WithFileType = @convention(c) (OpaquePointer?, UnsafePointer<CChar>?, Int32) -> Int32
    typealias SetBIO = @convention(c) (OpaquePointer?, OpaquePointer?, OpaquePointer?) -> Void
    typealias Void1 = @convention(c) (OpaquePointer?) -> Void
    typealias Read = @convention(c) (OpaquePointer?, UnsafeMutableRawPointer?, Int32) -> Int32
    typealias Write = @convention(c) (OpaquePointer?, UnsafeRawPointer?, Int32) -> Int32
    typealias GetError = @convention(c) (OpaquePointer?, Int32) -> Int32
    typealias GetParam = @convention(c) (OpaquePointer?) -> OpaquePointer?
    typealias VerifyResult = @convention(c) (OpaquePointer?) -> Int
    typealias VerifyString = @convention(c) (Int) -> UnsafePointer<CChar>?
    typealias ErrGet = @convention(c) () -> UInt
    typealias ErrString = @convention(c) (UInt, UnsafeMutablePointer<CChar>?, Int) -> Void
    typealias ErrClear = @convention(c) () -> Void

    let clientMethod: Method
    let ctxNew: CTXNew
    let ctxFree: Free
    let ctxCtrl: Ctrl
    let ctxSetVerify: SetVerify
    let ctxLoadVerifyLocations: LoadLocations
    let ctxSetDefaultVerifyPaths: WithContext
    let ctxUseCertificateChainFile: WithFile
    let ctxUsePrivateKeyFile: WithFileType
    let ctxCheckPrivateKey: WithContext
    let sslNew: CTXNew
    let sslFree: Free
    let sslSetBIO: SetBIO
    let sslSetConnectState: Void1
    let sslDoHandshake: WithContext
    let sslRead: Read
    let sslWrite: Write
    let sslGetError: GetError
    let sslShutdown: WithContext
    let sslCtrl: Ctrl
    let sslSet1Host: WithFile
    let sslGet0Param: GetParam
    let paramSet1IPASCII: WithFile
    let sslGetVerifyResult: VerifyResult
    let verifyErrorString: VerifyString
    let bioMemoryMethod: Method
    let bioNew: CTXNew
    let bioRead: Read
    let bioWrite: Write
    let bioCtrl: Ctrl
    let errGetError: ErrGet
    let errErrorString: ErrString
    let errClearError: ErrClear

    /// The shared library, loaded once. Fails with an actionable error when libssl is missing.
    static let shared: Result<OpenSSLLibrary, any Error> = Result { try OpenSSLLibrary() }

    static func load() throws -> OpenSSLLibrary {
        try shared.get()
    }

    private init() throws {
        let ssl = Self.open(["libssl.so.3", "libssl.so.1.1", "libssl.so"])
        let crypto = Self.open(["libcrypto.so.3", "libcrypto.so.1.1", "libcrypto.so"])
        guard ssl != nil, crypto != nil else {
            throw DockerError.tls("OpenSSL (libssl.so.3 or libssl.so.1.1) is not installed; install it "
                + "(apt install libssl3, dnf install openssl-libs, apk add libssl3) to use TLS endpoints")
        }
        func symbol<T>(_ name: String, as type: T.Type) throws -> T {
            guard let pointer = dlsym(ssl, name) ?? dlsym(crypto, name) else {
                throw DockerError.tls("the installed OpenSSL has no \(name); dtop needs OpenSSL 1.1.1 or newer")
            }
            return unsafeBitCast(pointer, to: T.self)
        }
        clientMethod = try symbol("TLS_client_method", as: Method.self)
        ctxNew = try symbol("SSL_CTX_new", as: CTXNew.self)
        ctxFree = try symbol("SSL_CTX_free", as: Free.self)
        ctxCtrl = try symbol("SSL_CTX_ctrl", as: Ctrl.self)
        ctxSetVerify = try symbol("SSL_CTX_set_verify", as: SetVerify.self)
        ctxLoadVerifyLocations = try symbol("SSL_CTX_load_verify_locations", as: LoadLocations.self)
        ctxSetDefaultVerifyPaths = try symbol("SSL_CTX_set_default_verify_paths", as: WithContext.self)
        ctxUseCertificateChainFile = try symbol("SSL_CTX_use_certificate_chain_file", as: WithFile.self)
        ctxUsePrivateKeyFile = try symbol("SSL_CTX_use_PrivateKey_file", as: WithFileType.self)
        ctxCheckPrivateKey = try symbol("SSL_CTX_check_private_key", as: WithContext.self)
        sslNew = try symbol("SSL_new", as: CTXNew.self)
        sslFree = try symbol("SSL_free", as: Free.self)
        sslSetBIO = try symbol("SSL_set_bio", as: SetBIO.self)
        sslSetConnectState = try symbol("SSL_set_connect_state", as: Void1.self)
        sslDoHandshake = try symbol("SSL_do_handshake", as: WithContext.self)
        sslRead = try symbol("SSL_read", as: Read.self)
        sslWrite = try symbol("SSL_write", as: Write.self)
        sslGetError = try symbol("SSL_get_error", as: GetError.self)
        sslShutdown = try symbol("SSL_shutdown", as: WithContext.self)
        sslCtrl = try symbol("SSL_ctrl", as: Ctrl.self)
        sslSet1Host = try symbol("SSL_set1_host", as: WithFile.self)
        sslGet0Param = try symbol("SSL_get0_param", as: GetParam.self)
        paramSet1IPASCII = try symbol("X509_VERIFY_PARAM_set1_ip_asc", as: WithFile.self)
        sslGetVerifyResult = try symbol("SSL_get_verify_result", as: VerifyResult.self)
        verifyErrorString = try symbol("X509_verify_cert_error_string", as: VerifyString.self)
        bioMemoryMethod = try symbol("BIO_s_mem", as: Method.self)
        bioNew = try symbol("BIO_new", as: CTXNew.self)
        bioRead = try symbol("BIO_read", as: Read.self)
        bioWrite = try symbol("BIO_write", as: Write.self)
        bioCtrl = try symbol("BIO_ctrl", as: Ctrl.self)
        errGetError = try symbol("ERR_get_error", as: ErrGet.self)
        errErrorString = try symbol("ERR_error_string_n", as: ErrString.self)
        errClearError = try symbol("ERR_clear_error", as: ErrClear.self)
    }

    private static func open(_ names: [String]) -> UnsafeMutableRawPointer? {
        for name in names {
            if let handle = dlopen(name, RTLD_NOW | RTLD_GLOBAL) { return handle }
        }
        return nil
    }

    /// Drains OpenSSL's error queue into one message.
    func drainErrors() -> String? {
        var messages: [String] = []
        while true {
            let code = errGetError()
            if code == 0 { break }
            var buffer = [CChar](repeating: 0, count: 256)
            errErrorString(code, &buffer, buffer.count)
            messages.append(String(cString: buffer))
        }
        return messages.isEmpty ? nil : messages.joined(separator: "; ")
    }

    // Constants from ssl.h / bio.h / x509_vfy.h (stable across 1.1 and 3.x).
    static let verifyNone: Int32 = 0
    static let verifyPeer: Int32 = 1
    static let fileTypePEM: Int32 = 1
    static let ctrlSetMinProtoVersion: Int32 = 123
    static let tls12Version = 0x0303
    static let ctrlSetTLSExtHostName: Int32 = 55
    static let bioCtrlPending: Int32 = 10
    static let bioSetMemEOFReturn: Int32 = 130
    static let errorWantRead: Int32 = 2
    static let errorWantWrite: Int32 = 3
    static let errorZeroReturn: Int32 = 6
    static let verifyOK = 0
    static let verifyHostnameMismatch = 62
    static let verifyIPMismatch = 64
}

/// A TLS client session over an existing connection, using memory BIOs so OpenSSL never touches
/// the socket: dtop's transport keeps doing the I/O (timeouts and `interrupt()` keep working).
final class OpenSSLConnection: DockerConnection, @unchecked Sendable {
    private let library: OpenSSLLibrary
    private let underlying: any DockerConnection
    private let context: OpaquePointer
    private let ssl: OpaquePointer
    private let input: OpaquePointer
    private let output: OpaquePointer
    // Serializes SSL object access (reads and writes come from one thread per request, but
    // `close()` may arrive from another).
    private let lock = NSLock()
    private var isClosed = false
    private let configuration: TLSConfiguration
    private let endpoint: String

    init(underlying: any DockerConnection, configuration: TLSConfiguration, serverName: String, endpoint: String) throws {
        let library = try OpenSSLLibrary.load()
        self.library = library
        self.underlying = underlying
        self.configuration = configuration
        self.endpoint = endpoint
        library.errClearError()

        guard let context = library.ctxNew(library.clientMethod()) else {
            throw DockerError.tls("could not create a TLS context: \(library.drainErrors() ?? "unknown error")")
        }
        do {
            // The context must be complete before SSL_new, which copies its certificate and key.
            try Self.configureContext(context, library: library, configuration: configuration)
        } catch {
            library.ctxFree(context)
            underlying.close()
            throw error
        }
        self.context = context
        guard let ssl = library.sslNew(context) else {
            library.ctxFree(context)
            throw DockerError.tls("could not create a TLS session: \(library.drainErrors() ?? "unknown error")")
        }
        self.ssl = ssl
        guard let input = library.bioNew(library.bioMemoryMethod()), let output = library.bioNew(library.bioMemoryMethod()) else {
            library.sslFree(ssl)
            library.ctxFree(context)
            throw DockerError.tls("could not allocate TLS buffers")
        }
        // An empty input BIO means "wait for more bytes", not end of stream.
        _ = library.bioCtrl(input, OpenSSLLibrary.bioSetMemEOFReturn, -1, nil)
        self.input = input
        self.output = output
        library.sslSetBIO(ssl, input, output) // the session now owns both BIOs

        do {
            try configureSession(configuration, serverName: serverName)
            try handshake(configuration: configuration, serverName: serverName, endpoint: endpoint)
        } catch {
            underlying.close()
            throw error
        }
    }

    deinit {
        library.sslFree(ssl)
        library.ctxFree(context)
    }

    private static func configureContext(_ context: OpaquePointer, library: OpenSSLLibrary,
                                         configuration: TLSConfiguration) throws {
        _ = library.ctxCtrl(context, OpenSSLLibrary.ctrlSetMinProtoVersion, OpenSSLLibrary.tls12Version, nil)
        if configuration.verifyPeer {
            library.ctxSetVerify(context, OpenSSLLibrary.verifyPeer, nil)
            if let caFile = configuration.caFile {
                guard library.ctxLoadVerifyLocations(context, caFile, nil) == 1 else {
                    throw DockerError.tls("could not load the CA certificate \(caFile): \(library.drainErrors() ?? "not a PEM certificate")")
                }
            } else {
                _ = library.ctxSetDefaultVerifyPaths(context)
            }
        } else {
            library.ctxSetVerify(context, OpenSSLLibrary.verifyNone, nil)
        }
        if let certificate = configuration.certificateFile, let key = configuration.keyFile {
            guard library.ctxUseCertificateChainFile(context, certificate) == 1 else {
                throw DockerError.tls("could not load the client certificate \(certificate): "
                    + (library.drainErrors() ?? "not a PEM certificate"))
            }
            guard library.ctxUsePrivateKeyFile(context, key, OpenSSLLibrary.fileTypePEM) == 1 else {
                throw DockerError.tls("could not load the client key \(key): \(library.drainErrors() ?? "not a PEM private key") "
                    + "(encrypted keys are not supported)")
            }
            guard library.ctxCheckPrivateKey(context) == 1 else {
                library.errClearError()
                throw DockerError.tls("the client certificate \(certificate) and key \(key) do not belong together")
            }
        }
    }

    private func configureSession(_ configuration: TLSConfiguration, serverName: String) throws {
        let isAddress = TLSSupport.isIPAddress(serverName)
        if configuration.verifyPeer {
            let accepted = isAddress
                ? library.paramSet1IPASCII(library.sslGet0Param(ssl), serverName)
                : library.sslSet1Host(ssl, serverName)
            guard accepted == 1 else { throw DockerError.tls("'\(serverName)' is not a valid name to verify") }
        }
        if !isAddress {
            // Server Name Indication, so name-based virtual hosts present the right certificate.
            serverName.withCString { name in
                _ = library.sslCtrl(ssl, OpenSSLLibrary.ctrlSetTLSExtHostName, 0, UnsafeMutableRawPointer(mutating: name))
            }
        }
    }

    private func handshake(configuration: TLSConfiguration, serverName: String, endpoint: String) throws {
        library.sslSetConnectState(ssl)
        while true {
            let result = library.sslDoHandshake(ssl)
            try flushOutput()
            if result == 1 { return }
            switch library.sslGetError(ssl, result) {
            case OpenSSLLibrary.errorWantRead:
                let bytes = try underlying.read(maxLength: 16 * 1024)
                guard !bytes.isEmpty else {
                    throw DockerError.tls("\(endpoint) closed the connection during the TLS handshake — "
                        + TLSSupport.closedDuringHandshakeHint(configuration))
                }
                feed(bytes)
            case OpenSSLLibrary.errorWantWrite:
                continue
            default:
                throw handshakeError(configuration: configuration, serverName: serverName, endpoint: endpoint)
            }
        }
    }

    private func handshakeError(configuration: TLSConfiguration, serverName: String, endpoint: String) -> DockerError {
        let verify = library.sslGetVerifyResult(ssl)
        if configuration.verifyPeer, verify != OpenSSLLibrary.verifyOK {
            let reason = library.verifyErrorString(verify).map { String(cString: $0) } ?? "code \(verify)"
            if verify == OpenSSLLibrary.verifyHostnameMismatch || verify == OpenSSLLibrary.verifyIPMismatch {
                return .tls(TLSSupport.nameMismatchMessage(serverName: serverName, endpoint: endpoint))
            }
            return .tls(TLSSupport.untrustedMessage(reason: reason, endpoint: endpoint, configuration: configuration))
        }
        let detail = library.drainErrors() ?? "handshake failed"
        return .tls(TLSSupport.handshakeMessage(detail: detail, endpoint: endpoint, configuration: configuration))
    }

    private func feed(_ bytes: [UInt8]) {
        bytes.withUnsafeBytes { raw in
            _ = library.bioWrite(input, raw.baseAddress, Int32(raw.count))
        }
    }

    private func flushOutput() throws {
        while library.bioCtrl(output, OpenSSLLibrary.bioCtrlPending, 0, nil) > 0 {
            var buffer = [UInt8](repeating: 0, count: 16 * 1024)
            let count = buffer.withUnsafeMutableBytes { raw in
                library.bioRead(output, raw.baseAddress, Int32(raw.count))
            }
            guard count > 0 else { break }
            try underlying.write(Array(buffer[0..<Int(count)]))
        }
    }

    func write(_ bytes: [UInt8]) throws {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { throw DockerError.cancelled }
        var offset = 0
        while offset < bytes.count {
            let written = bytes.withUnsafeBytes { raw in
                library.sslWrite(ssl, raw.baseAddress! + offset, Int32(min(raw.count - offset, 1 << 20)))
            }
            try flushOutput()
            if written > 0 {
                offset += Int(written)
                continue
            }
            let error = library.sslGetError(ssl, written)
            if error == OpenSSLLibrary.errorWantRead {
                let incoming = try underlying.read(maxLength: 16 * 1024)
                if incoming.isEmpty { throw DockerError.io("the TLS connection closed while writing") }
                feed(incoming)
            } else if error != OpenSSLLibrary.errorWantWrite {
                throw DockerError.io("TLS write failed: \(library.drainErrors() ?? "error \(error)")")
            }
        }
    }

    func read(maxLength: Int) throws -> [UInt8] {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { throw DockerError.cancelled }
        var buffer = [UInt8](repeating: 0, count: max(1, maxLength))
        while true {
            let count = buffer.withUnsafeMutableBytes { raw in
                library.sslRead(ssl, raw.baseAddress, Int32(min(raw.count, Int(Int32.max))))
            }
            if count > 0 {
                try flushOutput()
                return Array(buffer[0..<Int(count)])
            }
            let error = library.sslGetError(ssl, count)
            switch error {
            case OpenSSLLibrary.errorWantRead:
                try flushOutput()
                let incoming = try underlying.read(maxLength: 16 * 1024)
                // A peer that closes without close_notify still ends the stream (HTTP framing
                // tells us whether the body was complete).
                if incoming.isEmpty { return [] }
                feed(incoming)
            case OpenSSLLibrary.errorWantWrite:
                try flushOutput()
            case OpenSSLLibrary.errorZeroReturn:
                return []
            default:
                // With TLS 1.3 the server checks our certificate after the client finished its
                // side of the handshake, so a rejection surfaces here, on the first read.
                if let message = library.drainErrors() {
                    throw DockerError.tls(TLSSupport.handshakeMessage(detail: message, endpoint: endpoint,
                                                                      configuration: configuration))
                }
                return []
            }
        }
    }

    func interrupt() {
        underlying.interrupt()
    }

    func close() {
        // Closing the socket first wakes a reader blocked in `underlying.read` (which holds `lock`).
        underlying.close()
        lock.lock()
        isClosed = true
        lock.unlock()
    }
}
#endif
