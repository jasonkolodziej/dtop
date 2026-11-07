#if os(Windows)
import Foundation
import WinSDK

// SChannel (Windows' TLS stack) through SSPI. Swift's WinSDK module does not include sspi.h,
// schannel.h or ncrypt.h, so their functions are loaded from secur32.dll / ncrypt.dll at runtime
// and their small structs are written at the documented 64-bit offsets with `RawStruct`, never
// through guessed Swift layouts (ADR 0003). Certificate checking uses wincrypt.h, which WinSDK
// does import, so those calls are fully typed.

/// A zeroed, manually laid out C struct.
final class RawStruct {
    let pointer: UnsafeMutableRawPointer
    let size: Int

    init(size: Int) {
        self.size = size
        pointer = UnsafeMutableRawPointer.allocate(byteCount: size, alignment: 16)
        pointer.initializeMemory(as: UInt8.self, repeating: 0, count: size)
    }

    deinit { pointer.deallocate() }

    func set(_ offset: Int, _ value: UInt32) { pointer.storeBytes(of: value, toByteOffset: offset, as: UInt32.self) }
    func set(_ offset: Int, _ value: UnsafeMutableRawPointer?) {
        pointer.storeBytes(of: UInt(bitPattern: value), toByteOffset: offset, as: UInt.self)
    }
    func uint32(_ offset: Int) -> UInt32 { pointer.load(fromByteOffset: offset, as: UInt32.self) }
    func address(_ offset: Int) -> UnsafeMutableRawPointer? {
        UnsafeMutableRawPointer(bitPattern: pointer.load(fromByteOffset: offset, as: UInt.self))
    }
}

/// `SecBufferDesc` with `count` `SecBuffer`s (each `{ULONG cb; ULONG type; void *pv}`, 16 bytes).
/// NCrypt's `NCryptBufferDesc` / `NCryptBuffer` have the same layout.
final class SecBuffers {
    let descriptor = RawStruct(size: 16)
    let buffers: RawStruct
    let count: Int

    init(_ count: Int) {
        self.count = count
        buffers = RawStruct(size: 16 * count)
        descriptor.set(0, UInt32(0)) // SECBUFFER_VERSION
        descriptor.set(4, UInt32(count))
        descriptor.set(8, buffers.pointer)
    }

    func set(_ index: Int, type: UInt32, length: Int, data: UnsafeMutableRawPointer?) {
        buffers.set(index * 16, UInt32(length))
        buffers.set(index * 16 + 4, type)
        buffers.set(index * 16 + 8, data)
    }

    func type(_ index: Int) -> UInt32 { buffers.uint32(index * 16 + 4) }
    func length(_ index: Int) -> Int { Int(buffers.uint32(index * 16)) }
    func data(_ index: Int) -> UnsafeMutableRawPointer? { buffers.address(index * 16 + 8) }
}

extension String {
    /// A NUL-terminated UTF-16 copy that lives as long as the returned array.
    var wideCString: [UInt16] { Array(utf16) + [0] }
}

/// Functions from secur32.dll and ncrypt.dll.
final class SSPILibrary: @unchecked Sendable {
    typealias Acquire = @convention(c) (UnsafeMutablePointer<UInt16>?, UnsafeMutablePointer<UInt16>?, UInt32,
                                        UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, UnsafeMutableRawPointer?,
                                        UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, UnsafeMutableRawPointer?) -> Int32
    typealias Initialize = @convention(c) (UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, UnsafeMutablePointer<UInt16>?,
                                           UInt32, UInt32, UInt32, UnsafeMutableRawPointer?, UInt32, UnsafeMutableRawPointer?,
                                           UnsafeMutableRawPointer?, UnsafeMutablePointer<UInt32>?, UnsafeMutableRawPointer?) -> Int32
    typealias Query = @convention(c) (UnsafeMutableRawPointer?, UInt32, UnsafeMutableRawPointer?) -> Int32
    typealias Encrypt = @convention(c) (UnsafeMutableRawPointer?, UInt32, UnsafeMutableRawPointer?, UInt32) -> Int32
    typealias Decrypt = @convention(c) (UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, UInt32, UnsafeMutablePointer<UInt32>?) -> Int32
    typealias WithHandle = @convention(c) (UnsafeMutableRawPointer?) -> Int32
    typealias OpenProvider = @convention(c) (UnsafeMutablePointer<UInt>?, UnsafePointer<UInt16>?, UInt32) -> Int32
    typealias ImportKey = @convention(c) (UInt, UInt, UnsafePointer<UInt16>?, UnsafeMutableRawPointer?, UnsafeMutablePointer<UInt>?,
                                          UnsafeMutableRawPointer?, UInt32, UInt32) -> Int32
    typealias DeleteKey = @convention(c) (UInt, UInt32) -> Int32
    typealias FreeObject = @convention(c) (UInt) -> Int32

    let acquireCredentials: Acquire
    let initializeContext: Initialize
    let queryAttributes: Query
    let encrypt: Encrypt
    let decrypt: Decrypt
    let freeContextBuffer: WithHandle
    let deleteContext: WithHandle
    let freeCredentials: WithHandle
    let openStorageProvider: OpenProvider
    let importKey: ImportKey
    let deleteKey: DeleteKey
    let freeObject: FreeObject

    static let shared: Result<SSPILibrary, any Error> = Result { try SSPILibrary() }

    private init() throws {
        func load(_ name: String) throws -> HMODULE {
            guard let module = name.withCString(encodedAs: UTF16.self, { LoadLibraryW($0) }) else {
                throw DockerError.tls("could not load \(name)")
            }
            return module
        }
        let secur32 = try load("secur32.dll")
        let ncrypt = try load("ncrypt.dll")
        func symbol<T>(_ module: HMODULE, _ name: String, as type: T.Type) throws -> T {
            guard let address = GetProcAddress(module, name) else { throw DockerError.tls("\(name) is missing from Windows") }
            return unsafeBitCast(address, to: T.self)
        }
        acquireCredentials = try symbol(secur32, "AcquireCredentialsHandleW", as: Acquire.self)
        initializeContext = try symbol(secur32, "InitializeSecurityContextW", as: Initialize.self)
        queryAttributes = try symbol(secur32, "QueryContextAttributesW", as: Query.self)
        encrypt = try symbol(secur32, "EncryptMessage", as: Encrypt.self)
        decrypt = try symbol(secur32, "DecryptMessage", as: Decrypt.self)
        freeContextBuffer = try symbol(secur32, "FreeContextBuffer", as: WithHandle.self)
        deleteContext = try symbol(secur32, "DeleteSecurityContext", as: WithHandle.self)
        freeCredentials = try symbol(secur32, "FreeCredentialsHandle", as: WithHandle.self)
        openStorageProvider = try symbol(ncrypt, "NCryptOpenStorageProvider", as: OpenProvider.self)
        importKey = try symbol(ncrypt, "NCryptImportKey", as: ImportKey.self)
        deleteKey = try symbol(ncrypt, "NCryptDeleteKey", as: DeleteKey.self)
        freeObject = try symbol(ncrypt, "NCryptFreeObject", as: FreeObject.self)
    }

    // sspi.h / schannel.h / ncrypt.h constants.
    static let credentialsOutbound: UInt32 = 2
    static let schannelCredVersion: UInt32 = 4
    static let protocolTLS12Client: UInt32 = 0x0000_0800
    static let credManualValidation: UInt32 = 0x0000_0008
    static let credNoDefaultCredentials: UInt32 = 0x0000_0010
    static let useStrongCrypto: UInt32 = 0x0040_0000
    static let contextRequirements: UInt32 = 0x8 | 0x4 | 0x10 | 0x100 | 0x8000 | 0x4000 | 0x80 | 0x80000
    static let nativeDataRepresentation: UInt32 = 0x10
    static let bufferEmpty: UInt32 = 0
    static let bufferData: UInt32 = 1
    static let bufferToken: UInt32 = 2
    static let bufferExtra: UInt32 = 5
    static let bufferStreamTrailer: UInt32 = 6
    static let bufferStreamHeader: UInt32 = 7
    static let attributeStreamSizes: UInt32 = 4
    static let attributeRemoteCertificate: UInt32 = 0x53
    static let ok: Int32 = 0
    static let continueNeeded = Int32(bitPattern: 0x0009_0312)
    static let incompleteCredentials = Int32(bitPattern: 0x0009_0320)
    static let contextExpired = Int32(bitPattern: 0x0009_0317)
    static let renegotiate = Int32(bitPattern: 0x0009_0321)
    static let incompleteMessage = Int32(bitPattern: 0x8009_0318)
    static let keyStorageProvider = "Microsoft Software Key Storage Provider"
    static let unifiedProvider = "Microsoft Unified Security Protocol Provider"

    /// A readable name for an SSPI status, worded so `TLSSupport.handshakeMessage` can classify it.
    static func describe(_ status: Int32) -> String {
        let code = UInt32(bitPattern: status)
        let hex = "0x" + String(code, radix: 16, uppercase: true)
        switch code {
        case 0x8009_0308: return "wrong version number: the server did not answer with TLS (SEC_E_INVALID_TOKEN, \(hex))"
        case 0x8009_0326: return "the server sent a handshake failure alert (SEC_E_ILLEGAL_MESSAGE, \(hex))"
        case 0x8009_0327: return "certificate unknown (SEC_E_CERT_UNKNOWN, \(hex))"
        case 0x8009_0325: return "untrusted root (SEC_E_UNTRUSTED_ROOT, \(hex))"
        case 0x8009_0322: return "wrong principal (SEC_E_WRONG_PRINCIPAL, \(hex))"
        case 0x8009_0331: return "no common protocol or cipher (SEC_E_ALGORITHM_MISMATCH, \(hex))"
        case 0x8009_030E: return "no credentials (SEC_E_NO_CREDENTIALS, \(hex))"
        case 0x8009_030D: return "unknown credentials (SEC_E_UNKNOWN_CREDENTIALS, \(hex))"
        case 0x8009_0304: return "internal error (SEC_E_INTERNAL_ERROR, \(hex))"
        default: return "SSPI status \(hex)"
        }
    }
}

/// SChannel credentials for one TLS configuration: the client certificate (with its key
/// imported into CNG) and the credentials handle. Built once per configuration and shared by
/// every connection, because dtop opens a new connection per request.
final class SChannelCredentials: @unchecked Sendable {
    let handle = RawStruct(size: 16) // CredHandle
    private var certificate: PCCERT_CONTEXT?
    private var keyHandle: UInt = 0

    private static let lock = NSLock()
    nonisolated(unsafe) private static var cache: [String: SChannelCredentials] = [:]
    nonisolated(unsafe) private static var registeredCleanup = false

    static func shared(for configuration: TLSConfiguration, library: SSPILibrary) throws -> SChannelCredentials {
        let key = [configuration.certificateFile ?? "", configuration.keyFile ?? ""].joined(separator: "\u{0}")
        lock.lock()
        defer { lock.unlock() }
        if let cached = cache[key] { return cached }
        let credentials = try SChannelCredentials(configuration: configuration, library: library)
        cache[key] = credentials
        if !registeredCleanup {
            registeredCleanup = true
            // Imported client keys are persisted (SChannel cannot use ephemeral keys); remove
            // them when dtop exits.
            _ = atexit {
                SChannelCredentials.lock.lock()
                let all = Array(SChannelCredentials.cache.values)
                SChannelCredentials.cache.removeAll()
                SChannelCredentials.lock.unlock()
                for credentials in all { credentials.release() }
            }
        }
        return credentials
    }

    private let library: SSPILibrary

    private init(configuration: TLSConfiguration, library: SSPILibrary) throws {
        self.library = library
        if let certificateFile = configuration.certificateFile, let keyFile = configuration.keyFile {
            try loadClientCertificate(certificateFile: certificateFile, keyFile: keyFile)
        }
        let credential = RawStruct(size: 80) // SCHANNEL_CRED (64-bit layout)
        credential.set(0, SSPILibrary.schannelCredVersion)
        var certificates = [certificate]
        try certificates.withUnsafeMutableBufferPointer { array in
            if certificate != nil {
                credential.set(4, UInt32(1))
                credential.set(8, UnsafeMutableRawPointer(array.baseAddress))
            }
            credential.set(56, SSPILibrary.protocolTLS12Client)
            credential.set(72, SSPILibrary.credManualValidation | SSPILibrary.credNoDefaultCredentials | SSPILibrary.useStrongCrypto)
            var provider = SSPILibrary.unifiedProvider.wideCString
            let status = provider.withUnsafeMutableBufferPointer { name in
                library.acquireCredentials(nil, name.baseAddress, SSPILibrary.credentialsOutbound, nil, credential.pointer,
                                           nil, nil, handle.pointer, nil)
            }
            guard status == SSPILibrary.ok else {
                throw DockerError.tls("Windows could not set up TLS credentials: \(SSPILibrary.describe(status))")
            }
        }
    }

    private func loadClientCertificate(certificateFile: String, keyFile: String) throws {
        guard let certificateText = FileManager.default.contents(atPath: certificateFile).map({ String(decoding: $0, as: UTF8.self) }) else {
            throw DockerError.tls("could not read the client certificate \(certificateFile)")
        }
        guard let keyText = FileManager.default.contents(atPath: keyFile).map({ String(decoding: $0, as: UTF8.self) }) else {
            throw DockerError.tls("could not read the client key \(keyFile)")
        }
        let der = try PEM.certificates(certificateText, file: certificateFile)[0]
        let key = try PrivateKey.parse(pem: keyText, file: keyFile)
        let context = der.withUnsafeBufferPointer { bytes in
            CertCreateCertificateContext(DWORD(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING), bytes.baseAddress, DWORD(bytes.count))
        }
        guard let context else { throw DockerError.tls("Windows could not parse the client certificate \(certificateFile)") }
        certificate = context

        // Import the key as a named (persisted) CNG key, then point the certificate at it.
        var provider: UInt = 0
        var providerName = SSPILibrary.keyStorageProvider.wideCString
        var status = providerName.withUnsafeBufferPointer { library.openStorageProvider(&provider, $0.baseAddress, 0) }
        guard status == 0 else { throw DockerError.tls("could not open the Windows key storage provider (\(status))") }
        defer { _ = library.freeObject(provider) }

        let keyName = "dtop-" + UUID().uuidString
        var wideKeyName = keyName.wideCString
        var blob = key.pkcs8DER
        let parameters = SecBuffers(1)
        var blobType = "PKCS8_PRIVATEKEY".wideCString
        status = wideKeyName.withUnsafeMutableBytes { name in
            parameters.set(0, type: 45, length: name.count, data: name.baseAddress) // NCRYPTBUFFER_PKCS_KEY_NAME
            return blob.withUnsafeMutableBytes { data in
                blobType.withUnsafeBufferPointer { type in
                    library.importKey(provider, 0, type.baseAddress, parameters.descriptor.pointer, &keyHandle,
                                      data.baseAddress, UInt32(data.count), 0x80) // NCRYPT_OVERWRITE_KEY_FLAG
                }
            }
        }
        guard status == 0 else {
            throw DockerError.tls("Windows could not import the client key \(keyFile) (status 0x\(String(UInt32(bitPattern: status), radix: 16)))")
        }

        var info = CRYPT_KEY_PROV_INFO()
        let set: Bool = wideKeyName.withUnsafeMutableBufferPointer { name in
            providerName.withUnsafeMutableBufferPointer { providerPointer in
                info.pwszContainerName = name.baseAddress
                info.pwszProvName = providerPointer.baseAddress
                info.dwProvType = 0
                info.dwFlags = 0
                info.dwKeySpec = 0
                return win32Succeeded(CertSetCertificateContextProperty(context, DWORD(CERT_KEY_PROV_INFO_PROP_ID), 0, &info))
            }
        }
        guard set else { throw DockerError.tls("Windows could not attach the key to the client certificate") }
    }

    func release() {
        _ = library.freeCredentials(handle.pointer)
        if keyHandle != 0 { _ = library.deleteKey(keyHandle, 0) }
        keyHandle = 0
        if let certificate { _ = CertFreeCertificateContext(certificate) }
        certificate = nil
    }
}

/// A TLS client session over a Winsock connection, through SChannel.
final class SChannelConnection: DockerConnection, @unchecked Sendable {
    private let library: SSPILibrary
    private let underlying: any DockerConnection
    private let credentials: SChannelCredentials
    private let context = RawStruct(size: 16) // CtxtHandle
    private let configuration: TLSConfiguration
    private let serverName: String
    private let endpoint: String
    private let lock = NSLock()
    private var hasContext = false
    private var isClosed = false
    private var encrypted: [UInt8] = []
    private var plaintext: [UInt8] = []
    private var header = 0
    private var trailer = 0
    private var maximumMessage = 16 * 1024

    init(underlying: any DockerConnection, configuration: TLSConfiguration, serverName: String, endpoint: String) throws {
        library = try SSPILibrary.shared.get()
        self.underlying = underlying
        self.configuration = configuration
        self.serverName = serverName
        self.endpoint = endpoint
        do {
            credentials = try SChannelCredentials.shared(for: configuration, library: library)
            try handshake()
            if configuration.verifyPeer { try verifyServer() }
            try readStreamSizes()
        } catch {
            underlying.close() // the parameter, so this works before `self` is complete
            throw error
        }
    }

    deinit {
        if hasContext { _ = library.deleteContext(context.pointer) }
    }

    // MARK: - Handshake

    private func handshake() throws {
        var target = serverName.wideCString
        var first = true
        var needsRead = false
        while true {
            if needsRead {
                let bytes = try underlying.read(maxLength: 16 * 1024)
                guard !bytes.isEmpty else {
                    throw DockerError.tls("\(endpoint) closed the connection during the TLS handshake — "
                        + TLSSupport.closedDuringHandshakeHint(configuration))
                }
                encrypted += bytes
            }
            let output = SecBuffers(1)
            output.set(0, type: SSPILibrary.bufferToken, length: 0, data: nil)
            let input = SecBuffers(2)
            var attributes: UInt32 = 0
            let status: Int32 = encrypted.withUnsafeMutableBytes { raw in
                input.set(0, type: SSPILibrary.bufferToken, length: raw.count, data: raw.baseAddress)
                input.set(1, type: SSPILibrary.bufferEmpty, length: 0, data: nil)
                return target.withUnsafeMutableBufferPointer { name in
                    library.initializeContext(credentials.handle.pointer, first ? nil : context.pointer, name.baseAddress,
                                              SSPILibrary.contextRequirements, 0, SSPILibrary.nativeDataRepresentation,
                                              first ? nil : input.descriptor.pointer, 0, context.pointer,
                                              output.descriptor.pointer, &attributes, nil)
                }
            }
            if first { hasContext = true }
            first = false
            if let token = output.data(0) {
                let length = output.length(0)
                if length > 0 {
                    let bytes = Array(UnsafeRawBufferPointer(start: token, count: length))
                    _ = library.freeContextBuffer(token)
                    try underlying.write(bytes)
                } else {
                    _ = library.freeContextBuffer(token)
                }
            }
            switch status {
            case SSPILibrary.ok:
                encrypted = input.type(1) == SSPILibrary.bufferExtra ? Array(encrypted.suffix(input.length(1))) : []
                return
            case SSPILibrary.continueNeeded:
                if input.type(1) == SSPILibrary.bufferExtra {
                    encrypted = Array(encrypted.suffix(input.length(1)))
                    needsRead = false
                } else {
                    encrypted = []
                    needsRead = true
                }
            case SSPILibrary.incompleteCredentials:
                // The server asked for a client certificate SChannel could not pick; the input
                // was not consumed, so go again with the same bytes (and whatever we have).
                needsRead = false
            case SSPILibrary.incompleteMessage:
                needsRead = true
            default:
                throw DockerError.tls(TLSSupport.handshakeMessage(detail: SSPILibrary.describe(status), endpoint: endpoint,
                                                                  configuration: configuration))
            }
        }
    }

    private func readStreamSizes() throws {
        let sizes = RawStruct(size: 20) // SecPkgContext_StreamSizes
        let status = library.queryAttributes(context.pointer, SSPILibrary.attributeStreamSizes, sizes.pointer)
        guard status == SSPILibrary.ok else {
            throw DockerError.tls("could not read TLS stream sizes: \(SSPILibrary.describe(status))")
        }
        header = Int(sizes.uint32(0))
        trailer = Int(sizes.uint32(4))
        maximumMessage = max(1, Int(sizes.uint32(8)))
    }

    // MARK: - Server verification

    /// SChannel runs with manual validation so dtop can trust exactly the configured CA.
    private func verifyServer() throws {
        var remote: PCCERT_CONTEXT?
        let status = withUnsafeMutablePointer(to: &remote) { pointer in
            library.queryAttributes(context.pointer, SSPILibrary.attributeRemoteCertificate, UnsafeMutableRawPointer(pointer))
        }
        guard status == SSPILibrary.ok, let remote else {
            throw DockerError.tls("\(endpoint) presented no certificate")
        }
        defer { _ = CertFreeCertificateContext(remote) }

        var rootStore: HCERTSTORE?
        var engine: HCERTCHAINENGINE?
        defer {
            if let engine { CertFreeCertificateChainEngine(engine) }
            if let rootStore { _ = CertCloseStore(rootStore, 0) }
        }
        if let caFile = configuration.caFile {
            guard let text = FileManager.default.contents(atPath: caFile).map({ String(decoding: $0, as: UTF8.self) }) else {
                throw DockerError.tls("could not read the CA certificate \(caFile)")
            }
            // CERT_STORE_PROV_MEMORY is the pseudo-pointer (LPCSTR)2.
            rootStore = CertOpenStore(UnsafePointer<CChar>(bitPattern: 2), 0, 0, 0, nil)
            guard let rootStore else { throw DockerError.tls("could not create a certificate store") }
            for der in try PEM.certificates(text, file: caFile) {
                let added = der.withUnsafeBufferPointer { bytes in
                    win32Succeeded(CertAddEncodedCertificateToStore(rootStore, DWORD(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING),
                                                                    bytes.baseAddress, DWORD(bytes.count),
                                                                    DWORD(CERT_STORE_ADD_ALWAYS), nil))
                }
                guard added else { throw DockerError.tls("\(caFile) contains a certificate Windows cannot parse") }
            }
            var config = CERT_CHAIN_ENGINE_CONFIG()
            config.cbSize = DWORD(MemoryLayout<CERT_CHAIN_ENGINE_CONFIG>.size)
            config.hExclusiveRoot = rootStore
            guard win32Succeeded(CertCreateCertificateChainEngine(&config, &engine)) else {
                throw DockerError.tls("could not create a certificate chain engine for \(caFile)")
            }
        }

        var parameters = CERT_CHAIN_PARA()
        parameters.cbSize = DWORD(MemoryLayout<CERT_CHAIN_PARA>.size)
        var chain: PCCERT_CHAIN_CONTEXT?
        guard win32Succeeded(CertGetCertificateChain(engine, remote, nil, remote.pointee.hCertStore, &parameters, 0, nil, &chain)),
              let chain else {
            throw DockerError.tls(TLSSupport.untrustedMessage(reason: "Windows could not build its chain", endpoint: endpoint,
                                                              configuration: configuration))
        }
        defer { CertFreeCertificateChain(chain) }

        // SSL_EXTRA_CERT_CHAIN_POLICY_PARA: {DWORD cbSize; DWORD dwAuthType; DWORD fdwChecks; WCHAR *pwszServerName}
        var name = serverName.wideCString
        let policyStatus: DWORD = name.withUnsafeMutableBufferPointer { serverNamePointer in
            let extra = RawStruct(size: 24)
            extra.set(0, UInt32(24))
            extra.set(4, UInt32(2)) // AUTHTYPE_SERVER
            extra.set(8, UInt32(0))
            extra.set(16, UnsafeMutableRawPointer(serverNamePointer.baseAddress))
            var policy = CERT_CHAIN_POLICY_PARA()
            policy.cbSize = DWORD(MemoryLayout<CERT_CHAIN_POLICY_PARA>.size)
            policy.pvExtraPolicyPara = extra.pointer
            var result = CERT_CHAIN_POLICY_STATUS()
            result.cbSize = DWORD(MemoryLayout<CERT_CHAIN_POLICY_STATUS>.size)
            // CERT_CHAIN_POLICY_SSL is the pseudo-pointer (LPCSTR)4.
            guard win32Succeeded(CertVerifyCertificateChainPolicy(UnsafePointer<CChar>(bitPattern: 4), chain, &policy, &result)) else {
                return DWORD.max
            }
            return result.dwError
        }
        switch policyStatus {
        case 0:
            return
        case 0x800B_010F: // CERT_E_CN_NO_MATCH
            throw DockerError.tls(TLSSupport.nameMismatchMessage(serverName: serverName, endpoint: endpoint))
        case 0x800B_0109: // CERT_E_UNTRUSTEDROOT
            throw DockerError.tls(TLSSupport.untrustedMessage(reason: "untrusted root", endpoint: endpoint, configuration: configuration))
        case 0x800B_010A: // CERT_E_CHAINING
            throw DockerError.tls(TLSSupport.untrustedMessage(reason: "no chain to a trusted root", endpoint: endpoint,
                                                              configuration: configuration))
        case 0x800B_0101: // CERT_E_EXPIRED
            throw DockerError.tls(TLSSupport.untrustedMessage(reason: "the certificate has expired", endpoint: endpoint,
                                                              configuration: configuration))
        default:
            throw DockerError.tls(TLSSupport.untrustedMessage(reason: "policy error 0x\(String(policyStatus, radix: 16))",
                                                              endpoint: endpoint, configuration: configuration))
        }
    }

    // MARK: - I/O

    func write(_ bytes: [UInt8]) throws {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { throw DockerError.cancelled }
        var offset = 0
        while offset < bytes.count {
            let chunk = min(maximumMessage, bytes.count - offset)
            var message = [UInt8](repeating: 0, count: header + chunk + trailer)
            message.replaceSubrange(header..<(header + chunk), with: bytes[offset..<(offset + chunk)])
            let buffers = SecBuffers(4)
            let (status, total): (Int32, Int) = message.withUnsafeMutableBytes { raw in
                let base = raw.baseAddress!
                buffers.set(0, type: SSPILibrary.bufferStreamHeader, length: header, data: base)
                buffers.set(1, type: SSPILibrary.bufferData, length: chunk, data: base + header)
                buffers.set(2, type: SSPILibrary.bufferStreamTrailer, length: trailer, data: base + header + chunk)
                buffers.set(3, type: SSPILibrary.bufferEmpty, length: 0, data: nil)
                let status = library.encrypt(context.pointer, 0, buffers.descriptor.pointer, 0)
                return (status, buffers.length(0) + buffers.length(1) + buffers.length(2))
            }
            guard status == SSPILibrary.ok else {
                throw DockerError.io("TLS encryption failed: \(SSPILibrary.describe(status))")
            }
            try underlying.write(Array(message[0..<total]))
            offset += chunk
        }
    }

    func read(maxLength: Int) throws -> [UInt8] {
        lock.lock()
        defer { lock.unlock() }
        guard !isClosed else { throw DockerError.cancelled }
        while plaintext.isEmpty {
            if !encrypted.isEmpty {
                let buffers = SecBuffers(4)
                var extra: [UInt8] = []
                let status: Int32 = encrypted.withUnsafeMutableBytes { raw in
                    buffers.set(0, type: SSPILibrary.bufferData, length: raw.count, data: raw.baseAddress)
                    for index in 1..<4 { buffers.set(index, type: SSPILibrary.bufferEmpty, length: 0, data: nil) }
                    let status = library.decrypt(context.pointer, buffers.descriptor.pointer, 0, nil)
                    if status == SSPILibrary.ok || status == SSPILibrary.renegotiate || status == SSPILibrary.contextExpired {
                        for index in 0..<4 {
                            guard let data = buffers.data(index) else { continue }
                            let length = buffers.length(index)
                            if buffers.type(index) == SSPILibrary.bufferData, length > 0 {
                                plaintext += UnsafeRawBufferPointer(start: data, count: length)
                            } else if buffers.type(index) == SSPILibrary.bufferExtra, length > 0 {
                                extra = Array(UnsafeRawBufferPointer(start: data, count: length))
                            }
                        }
                    }
                    return status
                }
                switch status {
                case SSPILibrary.ok:
                    encrypted = extra
                    continue
                case SSPILibrary.contextExpired:
                    // close_notify: the server finished the stream.
                    encrypted = []
                    if plaintext.isEmpty { return [] }
                    continue
                case SSPILibrary.renegotiate:
                    // Post-handshake messages (TLS 1.3 tickets, key updates): feed them back.
                    encrypted = extra
                    try handshakeContinuation()
                    continue
                case SSPILibrary.incompleteMessage:
                    break
                default:
                    throw DockerError.tls(TLSSupport.handshakeMessage(detail: SSPILibrary.describe(status), endpoint: endpoint,
                                                                      configuration: configuration))
                }
            }
            let bytes = try underlying.read(maxLength: 16 * 1024)
            if bytes.isEmpty { return [] }
            encrypted += bytes
        }
        let count = min(max(1, maxLength), plaintext.count)
        let result = Array(plaintext[0..<count])
        plaintext.removeFirst(count)
        return result
    }

    /// Resumes the handshake loop for messages DecryptMessage hands back (SEC_I_RENEGOTIATE).
    private func handshakeContinuation() throws {
        var target = serverName.wideCString
        while true {
            let output = SecBuffers(1)
            output.set(0, type: SSPILibrary.bufferToken, length: 0, data: nil)
            let input = SecBuffers(2)
            var attributes: UInt32 = 0
            let status: Int32 = encrypted.withUnsafeMutableBytes { raw in
                input.set(0, type: SSPILibrary.bufferToken, length: raw.count, data: raw.baseAddress)
                input.set(1, type: SSPILibrary.bufferEmpty, length: 0, data: nil)
                return target.withUnsafeMutableBufferPointer { name in
                    library.initializeContext(credentials.handle.pointer, context.pointer, name.baseAddress,
                                              SSPILibrary.contextRequirements, 0, SSPILibrary.nativeDataRepresentation,
                                              input.descriptor.pointer, 0, context.pointer, output.descriptor.pointer, &attributes, nil)
                }
            }
            if let token = output.data(0) {
                let bytes = Array(UnsafeRawBufferPointer(start: token, count: output.length(0)))
                _ = library.freeContextBuffer(token)
                if !bytes.isEmpty { try underlying.write(bytes) }
            }
            let extra = input.type(1) == SSPILibrary.bufferExtra ? Array(encrypted.suffix(input.length(1))) : []
            switch status {
            case SSPILibrary.ok:
                encrypted = extra
                return
            case SSPILibrary.continueNeeded, SSPILibrary.incompleteMessage:
                let bytes = try underlying.read(maxLength: 16 * 1024)
                guard !bytes.isEmpty else { return }
                encrypted = (status == SSPILibrary.incompleteMessage ? encrypted : extra) + bytes
            default:
                throw DockerError.tls(TLSSupport.handshakeMessage(detail: SSPILibrary.describe(status), endpoint: endpoint,
                                                                  configuration: configuration))
            }
        }
    }

    func interrupt() {
        underlying.interrupt()
    }

    func close() {
        underlying.close()
        lock.lock()
        isClosed = true
        lock.unlock()
    }
}
#endif
