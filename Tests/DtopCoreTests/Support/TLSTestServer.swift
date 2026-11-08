import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif
@testable import DtopCore

/// Locates the `openssl` command-line tool, which the TLS tests use to mint certificates and to
/// run a real TLS server (`openssl s_server`). Linux and macOS ship one; on Windows it comes with
/// Git for Windows and the GitHub runner images.
enum OpenSSLTool {
    static let path: String? = {
        let environment = ProcessInfo.processInfo.environment
        if let explicit = environment["DTOP_OPENSSL"], FileManager.default.isExecutableFile(atPath: explicit) {
            return explicit
        }
        if let found = ProcessCommandRunner.resolve("openssl", environment: environment, platform: .current,
                                                    fileExists: { FileManager.default.isExecutableFile(atPath: $0) }) {
            return found
        }
        let fallbacks = [
            #"C:\Program Files\Git\usr\bin\openssl.exe"#, #"C:\Program Files\OpenSSL\bin\openssl.exe"#,
            #"C:\Program Files\OpenSSL-Win64\bin\openssl.exe"#, "/opt/homebrew/bin/openssl", "/usr/local/bin/openssl",
            "/usr/bin/openssl"
        ]
        return fallbacks.first { FileManager.default.isExecutableFile(atPath: $0) }
    }()

    /// CI sets this so a missing openssl fails the run instead of silently skipping TLS tests.
    static var isRequired: Bool { ProcessInfo.processInfo.environment["DTOP_REQUIRE_TLS_TESTS"] == "1" }

    static var isAvailable: Bool { path != nil || isRequired }

    @discardableResult
    static func run(_ arguments: [String], in directory: URL) throws -> String {
        guard let path else { throw TLSTestError("openssl not found; set DTOP_OPENSSL") }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = arguments
        process.currentDirectoryURL = directory
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        try process.run()
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self)
        guard process.terminationStatus == 0 else {
            throw TLSTestError("openssl \(arguments.joined(separator: " ")) failed: \(text)")
        }
        return text
    }
}

struct TLSTestError: Error, CustomStringConvertible {
    var description: String
    init(_ description: String) { self.description = description }
}

/// A throwaway PKI: a CA, a server certificate for localhost/127.0.0.1, client certificates with
/// EC and RSA keys, and two bad server certificates (another CA, another name).
final class TLSTestPKI: @unchecked Sendable {
    let directory: URL

    init() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("dtop-tls-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try write("ca.cnf", """
        [req]
        prompt = no
        distinguished_name = dn
        [dn]
        CN = dtop test CA
        [v3_ca]
        basicConstraints = critical,CA:TRUE
        keyUsage = critical,keyCertSign,cRLSign
        subjectKeyIdentifier = hash
        """)
        for (name, commonName) in [("server", "localhost"), ("client", "dtop client"), ("wrongname", "wrong.example")] {
            try write("\(name).cnf", "[req]\nprompt = no\ndistinguished_name = dn\n[dn]\nCN = \(commonName)\n")
        }
        try write("server.ext", """
        basicConstraints = CA:FALSE
        keyUsage = critical,digitalSignature,keyEncipherment
        extendedKeyUsage = serverAuth
        subjectAltName = DNS:localhost,IP:127.0.0.1
        """)
        try write("wrongname.ext", """
        basicConstraints = CA:FALSE
        keyUsage = critical,digitalSignature,keyEncipherment
        extendedKeyUsage = serverAuth
        subjectAltName = DNS:wrong.example
        """)
        try write("client.ext", """
        basicConstraints = CA:FALSE
        keyUsage = critical,digitalSignature
        extendedKeyUsage = clientAuth
        """)

        try makeCA("ca")
        try makeCA("rogue-ca")
        try openssl(["genrsa", "-out", "server.key", "2048"])
        try issue("server", key: "server.key", config: "server.cnf", extensions: "server.ext", ca: "ca", serial: "2")
        try issue("rogue-server", key: "server.key", config: "server.cnf", extensions: "server.ext", ca: "rogue-ca", serial: "3")
        try issue("wrongname", key: "server.key", config: "wrongname.cnf", extensions: "wrongname.ext", ca: "ca", serial: "4")
        try openssl(["ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "client-ec.key"])
        try issue("client-ec", key: "client-ec.key", config: "client.cnf", extensions: "client.ext", ca: "ca", serial: "5")
        try openssl(["genrsa", "-out", "client-rsa.key", "2048"])
        try issue("client-rsa", key: "client-rsa.key", config: "client.cnf", extensions: "client.ext", ca: "ca", serial: "6")
    }

    deinit {
        try? FileManager.default.removeItem(at: directory)
    }

    func path(_ name: String) -> String { directory.appendingPathComponent(name).path }

    private func write(_ name: String, _ text: String) throws {
        try (text + "\n").write(to: directory.appendingPathComponent(name), atomically: true, encoding: .utf8)
    }

    private func openssl(_ arguments: [String]) throws {
        try OpenSSLTool.run(arguments, in: directory)
    }

    private func makeCA(_ name: String) throws {
        try openssl(["genrsa", "-out", "\(name).key", "2048"])
        try openssl(["req", "-x509", "-new", "-key", "\(name).key", "-sha256", "-days", "30", "-config", "ca.cnf",
                     "-extensions", "v3_ca", "-out", "\(name).pem"])
    }

    private func issue(_ name: String, key: String, config: String, extensions: String, ca: String, serial: String) throws {
        try openssl(["req", "-new", "-key", key, "-config", config, "-out", "\(name).csr"])
        try openssl(["x509", "-req", "-in", "\(name).csr", "-CA", "\(ca).pem", "-CAkey", "\(ca).key", "-set_serial", serial,
                     "-days", "30", "-sha256", "-extfile", extensions, "-out", "\(name).pem"])
    }
}

/// `openssl s_server -www`: answers any GET with an HTTP/1.0 status page, which is enough to
/// run dtop's HTTP client end to end over TLS.
final class OpenSSLServer: @unchecked Sendable {
    let port: Int
    private let process: Process

    /// - Parameter extraArguments: more `s_server` flags, e.g. `-tls1_3` to allow only TLS 1.3.
    init(pki: TLSTestPKI, certificate: String = "server.pem", key: String = "server.key", requireClientCertificate: Bool,
         extraArguments: [String] = []) throws {
        var lastError = "s_server did not start"
        for attempt in 0..<6 {
            // First let OpenSSL pick a free port (OpenSSL 3 reports it); LibreSSL may not report
            // it, so later attempts try random high ports.
            let requested = attempt == 0 ? 0 : Int.random(in: 20_000...60_000)
            var arguments = ["s_server", "-accept", String(requested), "-cert", pki.path(certificate),
                             "-key", pki.path(key), "-www"]
            if requireClientCertificate {
                arguments += ["-CAfile", pki.path("ca.pem"), "-Verify", "1", "-verify_return_error"]
            }
            arguments += extraArguments
            let process = Process()
            process.executableURL = URL(fileURLWithPath: OpenSSLTool.path ?? "openssl")
            process.arguments = arguments
            process.currentDirectoryURL = pki.directory
            let output = Pipe()
            process.standardOutput = output
            process.standardError = output
            process.standardInput = Pipe()
            try process.run()
            if let line = Self.waitForAccept(output.fileHandleForReading, timeout: 10) {
                let reported = line.split(separator: ":").last.flatMap { Int($0.trimmingCharacters(in: .whitespaces)) }
                if let port = reported ?? (requested == 0 ? nil : requested) {
                    self.port = port
                    self.process = process
                    return
                }
            }
            lastError = "s_server attempt \(attempt) did not report a listening port"
            Self.forceStop(process)
        }
        throw TLSTestError(lastError)
    }

    deinit {
        stop()
    }

    func stop() {
        Self.forceStop(process)
    }

    /// SIGKILL on POSIX: a server started from a process that ignores SIGTERM inherits that, and
    /// `terminate()` would then leave `waitUntilExit()` blocked forever.
    static func forceStop(_ process: Process) {
        guard process.isRunning else { return }
        #if os(Windows)
        process.terminate()
        #else
        _ = kill(process.processIdentifier, SIGKILL)
        #endif
        process.waitUntilExit()
    }

    /// Starts a reader thread that drains the server's output for its whole life (so it never
    /// blocks on a full pipe) and returns the first full line starting with ACCEPT, or nil if the
    /// server exits or stays silent past the timeout.
    private static func waitForAccept(_ handle: FileHandle, timeout: TimeInterval) -> String? {
        final class Collected: @unchecked Sendable {
            let lock = NSLock()
            var text = ""
            var line: String?
        }
        let collected = Collected()
        let ready = DispatchSemaphore(value: 0)
        let reader = Thread {
            var signalled = false
            while true {
                let data = handle.availableData
                if data.isEmpty { break }
                collected.lock.lock()
                collected.text += String(decoding: data, as: UTF8.self)
                if collected.line == nil, let range = collected.text.range(of: "ACCEPT"),
                   let end = collected.text[range.lowerBound...].firstIndex(where: \.isNewline) {
                    collected.line = String(collected.text[range.lowerBound..<end])
                }
                let found = collected.line != nil
                collected.lock.unlock()
                if found && !signalled {
                    signalled = true
                    ready.signal()
                }
            }
            if !signalled { ready.signal() }
        }
        reader.start()
        _ = ready.wait(timeout: .now() + timeout)
        collected.lock.lock()
        defer { collected.lock.unlock() }
        return collected.line
    }
}
