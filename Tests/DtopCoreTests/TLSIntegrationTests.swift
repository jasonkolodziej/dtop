import Foundation
import Testing
@testable import DtopCore

/// End-to-end TLS through the platform backend (OpenSSL on Linux, Network.framework on macOS,
/// SChannel on Windows) against a real `openssl s_server`. Skipped when no openssl tool exists
/// unless DTOP_REQUIRE_TLS_TESTS=1 (CI sets it so they can never silently disappear).
@Suite("TLS against a real server", .serialized, .enabled(if: OpenSSLTool.isAvailable))
struct TLSIntegrationTests {
    static let pki: Result<TLSTestPKI, Error> = Result { try TLSTestPKI() }

    func pki() throws -> TLSTestPKI { try Self.pki.get() }

    func get(_ transport: TLSTransport) throws -> HTTPResponse {
        try HTTPClient(transport: transport).send(HTTPRequest(method: .get, target: "/_ping"))
    }

    func transport(_ server: OpenSSLServer, host: String = "127.0.0.1", _ configuration: TLSConfiguration) -> TLSTransport {
        TLSTransport(host: host, port: server.port, configuration: configuration, timeoutSeconds: 10)
    }

    func clientConfiguration(_ pki: TLSTestPKI, key: String = "client-ec", verify: Bool = true) -> TLSConfiguration {
        TLSConfiguration(verifyPeer: verify, caFile: pki.path("ca.pem"), certificateFile: pki.path("\(key).pem"),
                         keyFile: pki.path("\(key).key"))
    }

    func expectFailure(_ fragments: [String], _ body: () throws -> Void, sourceLocation: SourceLocation = #_sourceLocation) {
        do {
            try body()
            Issue.record("expected a TLS failure", sourceLocation: sourceLocation)
        } catch {
            let text = String(describing: error)
            #expect(fragments.contains { text.contains($0) }, "unexpected error: \(text)", sourceLocation: sourceLocation)
        }
    }

    @Test("mutual TLS with an EC client key: verified server, accepted client")
    func mutualEC() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        let response = try get(transport(server, clientConfiguration(pki)))
        #expect(response.status == 200)
        #expect(response.bodyText.contains("s_server") || response.bodyText.contains("Cipher"), "s_server's status page")
    }

    @Test("mutual TLS with an RSA client key")
    func mutualRSA() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        #expect(try get(transport(server, clientConfiguration(pki, key: "client-rsa"))).status == 200)
    }

    @Test("the certificate name can be checked against localhost while connecting to 127.0.0.1")
    func serverNameOverride() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, certificate: "wrongname.pem", requireClientCertificate: false)
        var configuration = TLSConfiguration(verifyPeer: true, caFile: pki.path("ca.pem"))
        configuration.serverName = "wrong.example"
        #expect(try get(transport(server, configuration)).status == 200)
    }

    @Test("DockerClient pings over TLS, with the host resolved like the Docker CLI would")
    func dockerClient() async throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        let host = DockerHost(name: "tls", endpoint: .tcp(host: "localhost", port: server.port), tls: clientConfiguration(pki))
        let client = try DockerClient.make(host: host, timeoutSeconds: 10)
        // s_server sends no API-Version header, so ping succeeds with nil.
        #expect(try await client.ping() == nil)
    }

    @Test("several requests in a row each complete their own handshake")
    func repeated() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        let tls = transport(server, clientConfiguration(pki))
        for _ in 0..<5 {
            #expect(try get(tls).status == 200)
        }
    }

    @Test("a server certificate from another CA is rejected")
    func untrusted() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, certificate: "rogue-server.pem", requireClientCertificate: false)
        expectFailure(["could not verify the certificate"]) {
            _ = try get(transport(server, TLSConfiguration(verifyPeer: true, caFile: pki.path("ca.pem"))))
        }
    }

    @Test("a certificate for another name is rejected")
    func nameMismatch() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, certificate: "wrongname.pem", requireClientCertificate: false)
        expectFailure(["is not valid for '127.0.0.1'", "could not verify"]) {
            _ = try get(transport(server, TLSConfiguration(verifyPeer: true, caFile: pki.path("ca.pem"))))
        }
    }

    @Test("without verification (--tls) any server certificate is accepted")
    func insecure() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, certificate: "rogue-server.pem", requireClientCertificate: false)
        #expect(try get(transport(server, TLSConfiguration(verifyPeer: false))).status == 200)
    }

    @Test("a daemon that requires a client certificate explains how to provide one")
    func missingClientCertificate() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        expectFailure(["client certificate", "--tlscert"]) {
            _ = try get(transport(server, TLSConfiguration(verifyPeer: true, caFile: pki.path("ca.pem"))))
        }
    }

    @Test("a client certificate from an untrusted CA is refused by the daemon")
    func rejectedClient() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        // The rogue CA signed nothing for the client, so present the rogue server pair instead.
        let configuration = TLSConfiguration(verifyPeer: true, caFile: pki.path("ca.pem"),
                                             certificateFile: pki.path("rogue-server.pem"), keyFile: pki.path("server.key"))
        expectFailure(["rejected the client certificate", "client certificate", "TLS"]) {
            _ = try get(transport(server, configuration))
        }
    }

    @Test("a missing or mismatched client key fails before connecting, naming the file")
    func badKeyFiles() throws {
        let pki = try pki()
        let server = try OpenSSLServer(pki: pki, requireClientCertificate: true)
        let mismatched = TLSConfiguration(verifyPeer: true, caFile: pki.path("ca.pem"),
                                          certificateFile: pki.path("client-ec.pem"), keyFile: pki.path("client-rsa.key"))
        expectFailure(["do not belong together", "client-rsa.key", "client"]) { _ = try get(transport(server, mismatched)) }
        let missingCA = TLSConfiguration(verifyPeer: true, caFile: pki.path("nope.pem"))
        expectFailure(["nope.pem"]) { _ = try get(transport(server, missingCA)) }
    }
}

@Suite("TLS support helpers")
struct TLSSupportTests {
    @Test("IP literals are told apart from host names", arguments: [
        ("127.0.0.1", true), ("10.0.0.255", true), ("::1", true), ("fe80::1%en0", true), ("2001:db8::8a2e:370:7334", true),
        ("localhost", false), ("docker.example.com", false), ("256.1.1.1", false), ("1.2.3", false), ("1.2.3.4.5", false),
        ("a1.b2.c3.d4", false)
    ])
    func addresses(name: String, isAddress: Bool) {
        #expect(TLSSupport.isIPAddress(name) == isAddress)
    }

    @Test("handshake failures are explained in terms of what to change")
    func messages() {
        let plain = TLSConfiguration()
        let withClient = TLSConfiguration(certificateFile: "c", keyFile: "k")
        #expect(TLSSupport.handshakeMessage(detail: "wrong version number", endpoint: "tcp://h:2375", configuration: plain)
            .contains("does not speak TLS"))
        #expect(TLSSupport.handshakeMessage(detail: "tlsv13 alert certificate required", endpoint: "e", configuration: plain)
            .contains("--tlscert"))
        #expect(TLSSupport.handshakeMessage(detail: "sslv3 alert handshake failure", endpoint: "e", configuration: plain)
            .contains("requires a client certificate"))
        #expect(TLSSupport.handshakeMessage(detail: "tlsv1 alert unknown ca", endpoint: "e", configuration: withClient)
            .contains("rejected the client certificate"))
        #expect(TLSSupport.handshakeMessage(detail: "something else", endpoint: "e", configuration: withClient)
            == "TLS handshake with e failed: something else")
        #expect(TLSSupport.untrustedMessage(reason: "self-signed", endpoint: "e", configuration: plain).contains("--tlscacert"))
        #expect(TLSSupport.untrustedMessage(reason: "x", endpoint: "e", configuration: TLSConfiguration(caFile: "/c/ca.pem"))
            .contains("/c/ca.pem did not issue it"))
        #expect(TLSSupport.nameMismatchMessage(serverName: "10.0.0.1", endpoint: "e").contains("subjectAltName"))
        #expect(DockerError.tls("x").description == "TLS error: x")
    }

    @Test("hosts with TLS get a TLS transport; others keep their plain transport")
    func factory() throws {
        let tlsHost = DockerHost(name: "a", endpoint: .tcp(host: "h", port: 2376), tls: TLSConfiguration())
        #expect(try TransportFactory.make(for: tlsHost) is TLSTransport)
        let transport = try #require(try TransportFactory.make(for: tlsHost) as? TLSTransport)
        #expect(transport.serverName == "h")
        #expect(transport.endpoint == .tcp(host: "h", port: 2376))
        #if !os(Windows)
        let plainHost = DockerHost(name: "b", endpoint: .tcp(host: "h", port: 2375))
        #expect(!(try TransportFactory.make(for: plainHost) is TLSTransport))
        #endif
    }
}
