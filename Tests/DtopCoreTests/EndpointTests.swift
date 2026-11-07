import Testing
@testable import DtopCore

@Suite("DockerEndpoint parsing")
struct EndpointParsingTests {
    @Test("parses unix sockets", arguments: [
        ("unix:///var/run/docker.sock", "/var/run/docker.sock"),
        ("unix:///Users/me/.docker/run/docker.sock", "/Users/me/.docker/run/docker.sock"),
        ("/run/user/1000/docker.sock", "/run/user/1000/docker.sock"),
        ("  unix:///tmp/d.sock  ", "/tmp/d.sock")
    ])
    func unixSockets(raw: String, path: String) throws {
        #expect(try DockerEndpoint.parse(raw) == .unixSocket(path: path))
    }

    @Test("parses Windows named pipes in both spellings")
    func namedPipes() throws {
        #expect(try DockerEndpoint.parse("npipe:////./pipe/docker_engine") == .namedPipe(path: #"\\.\pipe\docker_engine"#))
        #expect(try DockerEndpoint.parse(#"npipe://\\.\pipe\dockerDesktopLinuxEngine"#)
            == .namedPipe(path: #"\\.\pipe\dockerDesktopLinuxEngine"#))
        #expect(try DockerEndpoint.parse(#"\\.\pipe\custom"#) == .namedPipe(path: #"\\.\pipe\custom"#))
    }

    @Test("parses tcp hosts, ports and IPv6 literals", arguments: [
        ("tcp://127.0.0.1:2375", "127.0.0.1", 2375),
        ("tcp://docker.local", "docker.local", 2375),
        ("tcp://docker.local:12375/", "docker.local", 12375),
        ("http://10.0.0.5:2376", "10.0.0.5", 2376),
        ("tcp://[::1]:2375", "::1", 2375),
        ("tcp://[fe80::1]", "fe80::1", 2375),
        ("tcp://:2375", "127.0.0.1", 2375)
    ])
    func tcp(raw: String, host: String, port: Int) throws {
        #expect(try DockerEndpoint.parse(raw) == .tcp(host: host, port: port))
    }

    @Test("rejects malformed endpoints", arguments: [
        "", "   ", "unix://", "npipe://", "npipe:///", "tcp://host:0", "tcp://host:70000", "tcp://host:abc",
        "tcp://host/path", "tcp://[::1", "tcp://[::1]x", "ftp://host", "docker.sock", "relative/path"
    ])
    func rejectsInvalid(raw: String) {
        #expect(throws: DockerError.self) { try DockerEndpoint.parse(raw) }
    }

    @Test("reports ssh endpoints as unsupported rather than invalid; https is TLS")
    func unsupported() throws {
        #expect(throws: DockerError.unsupportedEndpoint("ssh://user@host")) { try DockerEndpoint.parse("ssh://user@host") }
        #expect(try DockerEndpoint.parse("https://host:2376") == .tcp(host: "host", port: 2376))
    }

    @Test("description round-trips through parse")
    func descriptionRoundTrip() throws {
        let endpoints: [DockerEndpoint] = [
            .unixSocket(path: "/var/run/docker.sock"),
            .namedPipe(path: #"\\.\pipe\docker_engine"#),
            .tcp(host: "example.com", port: 2375),
            .tcp(host: "::1", port: 2376)
        ]
        for endpoint in endpoints {
            #expect(try DockerEndpoint.parse(endpoint.description) == endpoint)
        }
        #expect(DockerEndpoint.namedPipe(path: #"\\.\pipe\docker_engine"#).description == "npipe:////./pipe/docker_engine")
    }

    @Test("Host header is synthetic for local transports and host:port for tcp")
    func hostHeader() {
        #expect(DockerEndpoint.unixSocket(path: "/x").hostHeader == "docker")
        #expect(DockerEndpoint.namedPipe(path: #"\\.\pipe\x"#).hostHeader == "docker")
        #expect(DockerEndpoint.tcp(host: "h", port: 1).hostHeader == "h:1")
        #expect(DockerEndpoint.tcp(host: "::1", port: 2375).hostHeader == "[::1]:2375")
    }
}

@Suite("DockerEndpoint resolution")
struct EndpointResolutionTests {
    @Test("explicit override beats DOCKER_HOST")
    func overrideWins() throws {
        let endpoint = try DockerEndpoint.resolve(
            override: "tcp://a:1", environment: ["DOCKER_HOST": "tcp://b:2"], platform: .linux,
            homeDirectory: "/home/me", fileExists: { _ in true })
        #expect(endpoint == .tcp(host: "a", port: 1))
    }

    @Test("DOCKER_HOST is used when there is no override")
    func dockerHost() throws {
        let endpoint = try DockerEndpoint.resolve(
            override: "  ", environment: ["DOCKER_HOST": "unix:///custom.sock"], platform: .macOS,
            homeDirectory: "/Users/me", fileExists: { _ in false })
        #expect(endpoint == .unixSocket(path: "/custom.sock"))
    }

    @Test("an invalid DOCKER_HOST is an error, not silently ignored")
    func invalidDockerHost() {
        #expect(throws: DockerError.self) {
            try DockerEndpoint.resolve(environment: ["DOCKER_HOST": "bogus"], platform: .linux, homeDirectory: nil,
                                       fileExists: { _ in true })
        }
    }

    @Test("Windows defaults to the Docker Desktop engine pipe")
    func windowsDefault() throws {
        let endpoint = try DockerEndpoint.resolve(environment: [:], platform: .windows, homeDirectory: #"C:\Users\me"#,
                                                  fileExists: { _ in false })
        #expect(endpoint == .namedPipe(path: DockerEndpoint.defaultWindowsPipe))
    }

    @Test("macOS falls back to Docker Desktop's per-user socket")
    func macOSDesktopSocket() throws {
        let endpoint = try DockerEndpoint.resolve(environment: [:], platform: .macOS, homeDirectory: "/Users/me",
                                                  fileExists: { $0 == "/Users/me/.docker/run/docker.sock" })
        #expect(endpoint == .unixSocket(path: "/Users/me/.docker/run/docker.sock"))
    }

    @Test("macOS finds Colima and OrbStack sockets")
    func macOSAlternatives() throws {
        let colima = try DockerEndpoint.resolve(environment: [:], platform: .macOS, homeDirectory: "/Users/me",
                                                fileExists: { $0 == "/Users/me/.colima/default/docker.sock" })
        #expect(colima == .unixSocket(path: "/Users/me/.colima/default/docker.sock"))
        let orb = try DockerEndpoint.resolve(environment: [:], platform: .macOS, homeDirectory: "/Users/me",
                                             fileExists: { $0.contains("orbstack") })
        #expect(orb == .unixSocket(path: "/Users/me/.orbstack/run/docker.sock"))
    }

    @Test("Linux prefers the system socket, then rootless XDG_RUNTIME_DIR")
    func linuxOrder() throws {
        let system = try DockerEndpoint.resolve(environment: ["XDG_RUNTIME_DIR": "/run/user/1000"], platform: .linux,
                                                homeDirectory: "/home/me", fileExists: { _ in true })
        #expect(system == .unixSocket(path: "/var/run/docker.sock"))
        let rootless = try DockerEndpoint.resolve(environment: ["XDG_RUNTIME_DIR": "/run/user/1000"], platform: .linux,
                                                  homeDirectory: "/home/me",
                                                  fileExists: { $0 == "/run/user/1000/docker.sock" })
        #expect(rootless == .unixSocket(path: "/run/user/1000/docker.sock"))
    }

    @Test("with nothing found, the conventional socket is returned so the error names it")
    func nothingFound() throws {
        let endpoint = try DockerEndpoint.resolve(environment: [:], platform: .linux, homeDirectory: nil,
                                                  fileExists: { _ in false })
        #expect(endpoint == .unixSocket(path: DockerEndpoint.defaultUnixSocket))
    }

    @Test("candidate list is empty on Windows and starts with the system socket elsewhere")
    func candidates() {
        #expect(DockerEndpoint.candidateSockets(environment: [:], platform: .windows, homeDirectory: "C:").isEmpty)
        for platform in [Platform.macOS, .linux, .other] {
            #expect(DockerEndpoint.candidateSockets(environment: [:], platform: platform, homeDirectory: "/h").first
                == DockerEndpoint.defaultUnixSocket)
        }
    }
}

@Suite("Transport factory")
struct TransportFactoryTests {
    @Test("builds the transport native to this platform and rejects foreign ones")
    func platformTransports() throws {
        #if os(Windows)
        #expect(try TransportFactory.make(for: .namedPipe(path: #"\\.\pipe\x"#)) is NamedPipeTransport)
        #expect(throws: DockerError.self) { try TransportFactory.make(for: .unixSocket(path: "/x")) }
        #else
        #expect(try TransportFactory.make(for: .unixSocket(path: "/x")) is UnixSocketTransport)
        #expect(try TransportFactory.make(for: .tcp(host: "h", port: 1)) is TCPTransport)
        #expect(throws: DockerError.self) { try TransportFactory.make(for: .namedPipe(path: #"\\.\pipe\x"#)) }
        #endif
    }

    @Test("transports report the endpoint they were built for")
    func endpoints() throws {
        #if !os(Windows)
        let transport = try TransportFactory.make(for: .unixSocket(path: "/tmp/x.sock"))
        #expect(transport.endpoint == .unixSocket(path: "/tmp/x.sock"))
        #endif
    }
}

@Suite("Platform helpers")
struct PlatformTests {
    @Test("current platform matches the compile target")
    func current() {
        #if os(macOS)
        #expect(Platform.current == .macOS)
        #elseif os(Linux)
        #expect(Platform.current == .linux)
        #elseif os(Windows)
        #expect(Platform.current == .windows)
        #endif
    }

    @Test("joinPath uses the platform separator and collapses duplicate separators")
    func join() {
        #expect(joinPath(["/home/me", ".config", "dtop"], platform: .linux) == "/home/me/.config/dtop")
        #expect(joinPath(["/home/me/", "/.config/"], platform: .macOS) == "/home/me/.config/")
        #expect(joinPath([#"C:\Users\me"#, "AppData", "dtop"], platform: .windows) == #"C:\Users\me\AppData\dtop"#)
        #expect(joinPath([#"C:\Users\"#, "x"], platform: .windows) == #"C:\Users\x"#)
        #expect(joinPath(["", "a", "", "b"], platform: .linux) == "a/b")
        #expect(joinPath([], platform: .linux) == "")
    }
}

@Suite("DockerError")
struct DockerErrorTests {
    @Test("descriptions are actionable")
    func actionable() {
        let connection = DockerError.connectionFailed(endpoint: "unix:///var/run/docker.sock", reason: "No such file")
        #expect(connection.description.contains("DOCKER_HOST"))
        #expect(connection.description.contains("--host"))
        #expect(DockerError.invalidEndpoint("x").description.contains("unix://"))
        #expect(DockerError.unsupportedEndpoint("ssh://x").description.contains("forward"))
        #expect(DockerError.http(status: 404, message: "No such container").description.contains("404"))
        #expect(DockerError.socketPathTooLong("/very/long").description.contains("symlink"))
    }

    @Test("classifies transient errors for retry decisions")
    func transient() {
        #expect(DockerError.connectionFailed(endpoint: "x", reason: "y").isTransient)
        #expect(DockerError.io("reset").isTransient)
        #expect(DockerError.timedOut("read").isTransient)
        #expect(DockerError.http(status: 503, message: "").isTransient)
        #expect(!DockerError.http(status: 404, message: "").isTransient)
        #expect(!DockerError.decoding("x").isTransient)
        #expect(DockerError.http(status: 404, message: "").isNotFound)
        #expect(!DockerError.http(status: 409, message: "").isNotFound)
    }
}
