import Foundation
import Testing
@testable import DtopCore

private func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }

@Suite("SHA-256")
struct SHA256Tests {
    @Test("matches the FIPS 180-4 test vectors", arguments: [
        ("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        ("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
         "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"),
        ("prod", "6754af9632a2745e85c293e5aac0863370d9bd3330b9938c00cadfd215227d77")
    ])
    func vectors(input: String, digest: String) {
        #expect(SHA256.hexDigest(input) == digest)
    }

    @Test("handles inputs spanning many blocks")
    func longInputs() {
        #expect(hex(SHA256.hash([UInt8](repeating: UInt8(ascii: "a"), count: 1_000_000)))
            == "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0")
        let bytes = (0..<3).flatMap { _ in (0...255).map(UInt8.init) }
        #expect(hex(SHA256.hash(bytes)) == "f3a25aa93aa2fbba28d79260535bbd6a5eb0fc1c24a8b0f04e12b484c1dfe363")
        // Lengths around the 55/56/64-byte padding boundaries must all differ.
        let digests = Set((54...66).map { SHA256.hexDigest(String(repeating: "x", count: $0)) })
        #expect(digests.count == 13)
    }
}

@Suite("PEM, DER and private keys")
struct PEMTests {
    @Test("PEM blocks decode with labels and headers; junk outside blocks is ignored")
    func pemBlocks() throws {
        let text = "junk\r\n" + KeyFixtures.ec256SEC1WithParameters + "\n" + KeyFixtures.rsaLegacyEncrypted
        let blocks = try PEM.parse(text)
        #expect(blocks.map(\.label) == ["EC PARAMETERS", "EC PRIVATE KEY", "RSA PRIVATE KEY"])
        #expect(blocks[0].der == [0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07])
        #expect(blocks[2].headers["Proc-Type"] == "4,ENCRYPTED")
        #expect(try PEM.parse("no pem here").isEmpty)
        #expect(throws: DockerError.self) { try PEM.parse("-----BEGIN CERTIFICATE-----\nAAAA\n") }
        #expect(throws: DockerError.self) { try PEM.parse("-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----") }
        #expect(throws: DockerError.self) { try PEM.certificates(KeyFixtures.rsaPKCS1, file: "cert.pem") }
    }

    @Test("PEM parses with CRLF and lone-CR line endings exactly as with LF")
    func lineEndings() throws {
        let lf = try PrivateKey.parse(pem: KeyFixtures.rsaPKCS1, file: "key.pem")
        let crlf = KeyFixtures.rsaPKCS1.replacingOccurrences(of: "\n", with: "\r\n")
        let cr = KeyFixtures.rsaPKCS1.replacingOccurrences(of: "\n", with: "\r")
        #expect(try PrivateKey.parse(pem: crlf, file: "key.pem") == lf)
        #expect(try PrivateKey.parse(pem: cr, file: "key.pem") == lf)
        #expect(try PEM.parse(KeyFixtures.rsaLegacyEncrypted.replacingOccurrences(of: "\n", with: "\r\n"))[0]
            .headers["Proc-Type"] == "4,ENCRYPTED")
    }

    @Test("DER round-trips short and long lengths")
    func der() throws {
        for size in [0, 1, 127, 128, 255, 256, 70_000] {
            let content = [UInt8](repeating: 7, count: size)
            let encoded = DER.encode(tag: DER.octetString, content)
            #expect(try DER.parse(encoded) == DER.Element(tag: DER.octetString, content: content))
        }
        #expect(throws: DockerError.self) { try DER.parse([0x30, 0x05, 0x01]) }
        #expect(throws: DockerError.self) { try DER.parse([0x30]) }
        #expect(throws: DockerError.self) { try DER.parse([0x04, 0x00, 0x04, 0x00]) }
        #expect(throws: DockerError.self) { try DER.parse([0x1F, 0x01, 0x00]) }
    }

    @Test("an RSA key in PKCS#1 converts to exactly OpenSSL's PKCS#8")
    func rsa() throws {
        let pkcs1 = try PrivateKey.parse(pem: KeyFixtures.rsaPKCS1, file: "key.pem")
        let pkcs8 = try PrivateKey.parse(pem: KeyFixtures.rsaPKCS8, file: "key.pem")
        #expect(pkcs1.kind == .rsa)
        #expect(pkcs1 == pkcs8, "both spellings yield the same PKCS#1 key")
        let openSSLPKCS8 = try PEM.parse(KeyFixtures.rsaPKCS8)[0].der
        #expect(pkcs1.pkcs8DER == openSSLPKCS8)
        #expect(pkcs1.x963Representation == nil)
    }

    @Test("EC keys give the same X9.63 form from SEC1 and PKCS#8, matching OpenSSL's public point",
          arguments: [
            (KeyFixtures.ec256SEC1, KeyFixtures.ec256PKCS8, ECCurve.p256, KeyFixtures.ec256PublicPoint),
            (KeyFixtures.ec256SEC1WithParameters, KeyFixtures.ec256PKCS8, ECCurve.p256, KeyFixtures.ec256PublicPoint),
            (KeyFixtures.ec384SEC1, KeyFixtures.ec384PKCS8, ECCurve.p384, KeyFixtures.ec384PublicPoint)
          ])
    func ec(sec1: String, pkcs8: String, curve: ECCurve, point: String) throws {
        let fromSEC1 = try PrivateKey.parse(pem: sec1, file: "key.pem")
        let fromPKCS8 = try PrivateKey.parse(pem: pkcs8, file: "key.pem")
        #expect(fromSEC1.kind == .ec(curve))
        #expect(fromPKCS8.kind == .ec(curve))
        let x963 = try #require(fromSEC1.x963Representation)
        #expect(x963 == fromPKCS8.x963Representation)
        #expect(x963.count == 1 + 3 * curve.byteCount)
        #expect(hex(fromSEC1.ecPublicPoint ?? []) == point)
        // Our PKCS#8 wrapping parses back to the same key.
        let rewrapped = try PrivateKey.pkcs8(fromSEC1.pkcs8DER)
        #expect(rewrapped.x963Representation == x963)
    }

    @Test("encrypted keys and non-keys are rejected with instructions")
    func rejected() {
        for (text, fragment) in [(KeyFixtures.rsaEncryptedPKCS8, "encrypted"), (KeyFixtures.rsaLegacyEncrypted, "encrypted"),
                                 ("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----", "no private key")] {
            do {
                _ = try PrivateKey.parse(pem: text, file: "key.pem")
                Issue.record("expected an error")
            } catch {
                #expect(String(describing: error).contains(fragment))
            }
        }
    }
}

@Suite("Endpoint parsing with TLS")
struct TLSEndpointTests {
    @Test("https:// is TLS on port 2376 by default; tcp:// picks the port by TLS mode")
    func ports() throws {
        #expect(try DockerEndpoint.parse("https://docker.example.com") == .tcp(host: "docker.example.com", port: 2376))
        #expect(try DockerEndpoint.parse("https://10.0.0.5:3376") == .tcp(host: "10.0.0.5", port: 3376))
        #expect(try DockerEndpoint.parse("tcp://h", tls: true) == .tcp(host: "h", port: 2376))
        #expect(try DockerEndpoint.parse("tcp://h") == .tcp(host: "h", port: 2375))
        #expect(try DockerEndpoint.parse("tcp://h:9", tls: true) == .tcp(host: "h", port: 9))
        #expect(DockerEndpoint.schemeImpliesTLS(" HTTPS://x"))
        #expect(!DockerEndpoint.schemeImpliesTLS("tcp://x"))
        #expect(DockerEndpoint.tcp(host: "h", port: 1).tcpHost == "h")
        #expect(DockerEndpoint.unixSocket(path: "/x").tcpHost == nil)
    }

    @Test("ssh:// stays unsupported and says how to forward it")
    func ssh() {
        #expect(throws: DockerError.unsupportedEndpoint("ssh://me@h")) { try DockerEndpoint.parse("ssh://me@h") }
        #expect(DockerError.unsupportedEndpoint("ssh://me@h").description.contains("ssh -NL"))
    }

    @Test("--host values may carry a name")
    func namedHosts() {
        #expect(HostRequest.parse("prod=tcp://p:2376") == HostRequest(name: "prod", address: "tcp://p:2376"))
        #expect(HostRequest.parse("tcp://p:2376") == HostRequest(address: "tcp://p:2376"))
        #expect(HostRequest.parse("unix:///a=b") == HostRequest(address: "unix:///a=b"), "an = inside the path is not a name")
        #expect(HostRequest.parse("=tcp://p") == HostRequest(address: "=tcp://p"))
    }
}

/// An in-memory file system for resolver tests.
final class FakeFiles: @unchecked Sendable {
    private let lock = NSLock()
    private var files: [String: String] = [:]

    init(_ files: [String: String] = [:]) { self.files = files }

    func add(_ path: String, _ text: String = "") {
        lock.lock()
        files[path] = text
        lock.unlock()
    }

    func environment(_ variables: [String: String] = [:], home: String? = "/home/me", platform: Platform = .linux) -> HostEnvironment {
        HostEnvironment(
            variables: variables, platform: platform, homeDirectory: home,
            fileExists: { path in self.lock.lock(); defer { self.lock.unlock() }; return self.files[path] != nil },
            readFile: { path in self.lock.lock(); defer { self.lock.unlock() }; return self.files[path] },
            listDirectory: { directory in
                self.lock.lock()
                defer { self.lock.unlock() }
                let prefix = directory.hasSuffix("/") ? directory : directory + "/"
                return Array(Set(self.files.keys.filter { $0.hasPrefix(prefix) }
                    .compactMap { $0.dropFirst(prefix.count).split(separator: "/").first.map(String.init) })).sorted()
            })
    }

    /// Writes a context the way `docker context create` does.
    func addContext(_ name: String, host: String, skipVerify: Bool = false, tlsFiles: [String] = [], configDirectory: String = "/home/me/.docker") {
        let digest = SHA256.hexDigest(name)
        add("\(configDirectory)/contexts/meta/\(digest)/meta.json", """
        {"Name":"\(name)","Metadata":{"Description":"\(name) daemon"},"Endpoints":{"docker":{"Host":"\(host)","SkipTLSVerify":\(skipVerify)}}}
        """)
        for file in tlsFiles {
            add("\(configDirectory)/contexts/tls/\(digest)/docker/\(file)", "pem")
        }
    }
}

@Suite("Host resolution")
struct HostResolutionTests {
    @Test("with nothing set, the default socket is the single local host")
    func defaults() throws {
        let files = FakeFiles(["/var/run/docker.sock": ""])
        let result = try DockerHostResolver.resolve([], environment: files.environment())
        #expect(result.hosts == [DockerHost(name: "local", endpoint: .unixSocket(path: "/var/run/docker.sock"), source: "default socket")])
    }

    @Test("DOCKER_TLS_VERIFY with DOCKER_CERT_PATH enables verified TLS with client certificates on 2376")
    func environmentTLS() throws {
        let files = FakeFiles(["/certs/ca.pem": "", "/certs/cert.pem": "", "/certs/key.pem": ""])
        let host = try DockerHostResolver.resolve(HostRequest(), environment: files.environment([
            "DOCKER_HOST": "tcp://prod.example.com", "DOCKER_TLS_VERIFY": "1", "DOCKER_CERT_PATH": "/certs"
        ]))
        #expect(host.endpoint == .tcp(host: "prod.example.com", port: 2376))
        #expect(host.tls == TLSConfiguration(verifyPeer: true, caFile: "/certs/ca.pem", certificateFile: "/certs/cert.pem",
                                              keyFile: "/certs/key.pem"))
        #expect(host.name == "prod.example.com")
        #expect(host.source == "DOCKER_HOST")
        #expect(host.description == "tcp://prod.example.com:2376 (TLS, client cert)")
    }

    @Test("certificates default to ~/.docker and missing ones are simply not used")
    func homeCertificates() throws {
        let files = FakeFiles(["/home/me/.docker/ca.pem": ""])
        let host = try DockerHostResolver.resolve(HostRequest(address: "tcp://h", tls: TLSRequest(verify: true)),
                                                  environment: files.environment())
        #expect(host.tls == TLSConfiguration(verifyPeer: true, caFile: "/home/me/.docker/ca.pem"))
        #expect(host.tls?.summary == "TLS")
    }

    @Test("--tls alone encrypts without verifying; DOCKER_TLS does the same")
    func tlsWithoutVerify() throws {
        let files = FakeFiles()
        let flag = try DockerHostResolver.resolve(HostRequest(address: "tcp://h", tls: TLSRequest(enabled: true)),
                                                  environment: files.environment())
        #expect(flag.tls == TLSConfiguration(verifyPeer: false))
        #expect(flag.tls?.summary == "TLS, unverified")
        let variable = try DockerHostResolver.resolve(HostRequest(address: "tcp://h"),
                                                      environment: files.environment(["DOCKER_TLS": "1"]))
        #expect(variable.tls == TLSConfiguration(verifyPeer: false))
        #expect(variable.endpoint == .tcp(host: "h", port: 2376))
    }

    @Test("https:// turns on TLS, verified only when asked (like --tls)")
    func httpsScheme() throws {
        let host = try DockerHostResolver.resolve(HostRequest(address: "https://h"), environment: FakeFiles().environment())
        #expect(host.endpoint == .tcp(host: "h", port: 2376))
        #expect(host.tls == TLSConfiguration(verifyPeer: false))
        let verified = try DockerHostResolver.resolve(HostRequest(address: "https://h"),
                                                      globalTLS: TLSRequest(verify: true), environment: FakeFiles().environment())
        #expect(verified.tls?.verifyPeer == true)
    }

    @Test("plain tcp stays plain; TLS settings never apply to sockets or pipes")
    func plain() throws {
        let files = FakeFiles(["/home/me/.docker/ca.pem": ""])
        #expect(try DockerHostResolver.resolve(HostRequest(address: "tcp://h"), environment: files.environment()).tls == nil)
        let socket = try DockerHostResolver.resolve(HostRequest(address: "unix:///s.sock", tls: TLSRequest(verify: true)),
                                                    environment: files.environment())
        #expect(socket.tls == nil)
        #expect(socket.name == "local")
    }

    @Test("explicit certificate paths must exist, and a certificate needs its key")
    func validation() throws {
        let files = FakeFiles(["/c/cert.pem": "", "/c/ca.pem": ""])
        let environment = files.environment()
        #expect(throws: DockerError.self) {
            try DockerHostResolver.resolve(HostRequest(address: "tcp://h", tls: TLSRequest(verify: true, caFile: "/nope.pem")),
                                           environment: environment)
        }
        do {
            _ = try DockerHostResolver.resolve(HostRequest(address: "tcp://h", tls: TLSRequest(verify: true, certificateDirectory: "/c")),
                                               environment: environment)
            Issue.record("expected a missing-key error")
        } catch {
            #expect(String(describing: error).contains("key (key.pem / --tlskey) is missing"))
        }
        let explicit = try DockerHostResolver.resolve(
            HostRequest(address: "tcp://h", tls: TLSRequest(verify: true, caFile: "/c/ca.pem", certificateDirectory: "/empty")),
            environment: environment)
        #expect(explicit.tls == TLSConfiguration(verifyPeer: true, caFile: "/c/ca.pem"))
    }

    @Test("per-host TLS settings override global ones")
    func merging() {
        let global = TLSRequest(verify: true, caFile: "/g/ca.pem", certificateDirectory: "/g")
        let local = TLSRequest(verify: false, certificateDirectory: "/l")
        #expect(local.merged(over: global) == TLSRequest(verify: false, caFile: "/g/ca.pem", certificateDirectory: "/l"))
        #expect(TLSRequest().isEmpty)
        #expect(!local.isEmpty)
    }

    @Test("several hosts keep their order and get distinct names")
    func severalHosts() throws {
        let files = FakeFiles(["/var/run/docker.sock": ""])
        let result = try DockerHostResolver.resolve([
            HostRequest(), HostRequest(address: "tcp://build:2375"), HostRequest(name: "build", address: "tcp://10.0.0.9"),
            HostRequest(address: "unix:///other.sock")
        ], environment: files.environment())
        #expect(result.hosts.map(\.name) == ["local", "build", "build-2", "local-2"])
        #expect(result.hosts[1].endpoint == .tcp(host: "build", port: 2375))
    }
}

@Suite("Docker contexts")
struct DockerContextTests {
    @Test("the current context from config.json supplies the endpoint and its TLS files")
    func currentContext() throws {
        let files = FakeFiles()
        files.add("/home/me/.docker/config.json", #"{"auths":{},"currentContext":"prod"}"#)
        files.addContext("prod", host: "tcp://prod.example.com:2376", tlsFiles: ["ca.pem", "cert.pem", "key.pem"])
        let host = try DockerHostResolver.resolve(HostRequest(), environment: files.environment())
        let tlsDirectory = "/home/me/.docker/contexts/tls/\(SHA256.hexDigest("prod"))/docker"
        #expect(host.name == "prod")
        #expect(host.source == "context prod")
        #expect(host.endpoint == .tcp(host: "prod.example.com", port: 2376))
        #expect(host.tls == TLSConfiguration(verifyPeer: true, caFile: tlsDirectory + "/ca.pem",
                                              certificateFile: tlsDirectory + "/cert.pem", keyFile: tlsDirectory + "/key.pem"))
    }

    @Test("DOCKER_HOST beats the current context; DOCKER_CONTEXT beats config.json")
    func precedence() throws {
        let files = FakeFiles()
        files.add("/home/me/.docker/config.json", #"{"currentContext":"prod"}"#)
        files.addContext("prod", host: "tcp://prod:2375")
        files.addContext("staging", host: "tcp://staging:2375")
        let viaHost = try DockerHostResolver.resolve(HostRequest(), environment: files.environment(["DOCKER_HOST": "tcp://x:1"]))
        #expect(viaHost.endpoint == .tcp(host: "x", port: 1))
        let viaContext = try DockerHostResolver.resolve(HostRequest(), environment: files.environment(["DOCKER_CONTEXT": "staging"]))
        #expect(viaContext.name == "staging")
        #expect(viaContext.tls == nil)
    }

    @Test("DOCKER_CONFIG moves the context store; SkipTLSVerify turns verification off")
    func dockerConfig() throws {
        let files = FakeFiles()
        files.addContext("lab", host: "tcp://lab:2376", skipVerify: true, tlsFiles: ["ca.pem"], configDirectory: "/cfg")
        let host = try DockerHostResolver.resolve(HostRequest(context: "lab"), environment: files.environment(["DOCKER_CONFIG": "/cfg"]))
        #expect(host.tls?.verifyPeer == false)
        #expect(host.tls?.caFile?.hasPrefix("/cfg/contexts/tls/") == true)
    }

    @Test("--context names a context; unknown names fail; 'default' means the environment's host")
    func named() throws {
        let files = FakeFiles(["/var/run/docker.sock": ""])
        files.addContext("prod", host: "tcp://prod:2375")
        let environment = files.environment(["DOCKER_HOST": "tcp://envhost:2375"])
        #expect(try DockerHostResolver.resolve(HostRequest(context: "prod"), environment: environment).endpoint == .tcp(host: "prod", port: 2375))
        let fallback = try DockerHostResolver.resolve(HostRequest(context: "default"), environment: environment)
        #expect(fallback.endpoint == .tcp(host: "envhost", port: 2375))
        #expect(fallback.name == "default")
        #expect(throws: DockerError.self) { try DockerHostResolver.resolve(HostRequest(context: "missing"), environment: environment) }
    }

    @Test("--all-contexts lists default first, then every stored context; ssh contexts become warnings")
    func allContexts() throws {
        let files = FakeFiles(["/var/run/docker.sock": ""])
        files.addContext("zeta", host: "tcp://z:2375")
        files.addContext("alpha", host: "tcp://a:2376", tlsFiles: ["ca.pem"])
        files.addContext("remote", host: "ssh://me@box")
        files.add("/home/me/.docker/contexts/meta/garbage/meta.json", "{not json")
        let result = try DockerHostResolver.resolve([], allContexts: true, environment: files.environment())
        #expect(result.hosts.map(\.name) == ["default", "alpha", "zeta"])
        #expect(result.hosts[0].endpoint == .unixSocket(path: "/var/run/docker.sock"))
        #expect(result.hosts[1].tls?.caFile != nil)
        #expect(result.warnings.count == 1)
        #expect(result.warnings.first?.contains("context remote") == true)
        let store = DockerContextStore(environment: files.environment())
        #expect(store.list().map(\.name) == ["alpha", "remote", "zeta"])
        #expect(store.context(named: "alpha")?.description == "alpha daemon")
        #expect(store.activeContextName() == nil)
    }

    @Test("an explicit host plus --all-contexts keeps both, without duplicating contexts")
    func mixed() throws {
        let files = FakeFiles(["/var/run/docker.sock": ""])
        files.addContext("prod", host: "tcp://prod:2375")
        let result = try DockerHostResolver.resolve([HostRequest(context: "prod"), HostRequest(name: "lab", address: "tcp://lab")],
                                                    allContexts: true, environment: files.environment())
        #expect(result.hosts.map(\.name) == ["default", "prod", "lab"])
    }
}
