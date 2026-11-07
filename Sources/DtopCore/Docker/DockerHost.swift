/// TLS settings for one TCP Docker endpoint. Files are PEM, as Docker uses them.
public struct TLSConfiguration: Sendable, Equatable {
    /// Verify the daemon's certificate (`--tlsverify`). False encrypts without checking who the
    /// server is (`--tls`), which is what the Docker CLI does for `--tls` alone.
    public var verifyPeer: Bool
    /// CA bundle to trust. Nil means the system's trusted roots.
    public var caFile: String?
    /// Client certificate (chain) and its private key, for daemons that require them.
    public var certificateFile: String?
    public var keyFile: String?
    /// Name to verify the server certificate against. Defaults to the endpoint's host.
    public var serverName: String?

    public init(verifyPeer: Bool = true, caFile: String? = nil, certificateFile: String? = nil,
                keyFile: String? = nil, serverName: String? = nil) {
        self.verifyPeer = verifyPeer
        self.caFile = caFile
        self.certificateFile = certificateFile
        self.keyFile = keyFile
        self.serverName = serverName
    }

    public var hasClientCertificate: Bool { certificateFile != nil && keyFile != nil }

    /// Short description for headers and the host list.
    public var summary: String {
        var parts = [verifyPeer ? "TLS" : "TLS, unverified"]
        if hasClientCertificate { parts.append("client cert") }
        return parts.joined(separator: ", ")
    }
}

/// One Docker daemon dtop talks to: a name for the UI, the endpoint, and TLS when it applies.
public struct DockerHost: Sendable, Equatable, Identifiable {
    public var name: String
    public var endpoint: DockerEndpoint
    public var tls: TLSConfiguration?
    /// Where this host came from (`--host`, `DOCKER_HOST`, `context prod`, …), for messages.
    public var source: String

    public init(name: String, endpoint: DockerEndpoint, tls: TLSConfiguration? = nil, source: String = "") {
        self.name = name
        self.endpoint = endpoint
        self.tls = tls
        self.source = source
    }

    public var id: String { name }

    /// Endpoint plus TLS mode, e.g. `tcp://prod:2376 (TLS, client cert)`.
    public var description: String {
        endpoint.description + (tls.map { " (\($0.summary))" } ?? "")
    }

    /// The Docker context this host came from, if any (`default` is not a stored context).
    public var contextName: String? {
        guard source.hasPrefix("context ") else { return nil }
        let name = String(source.dropFirst("context ".count))
        return name == "default" ? nil : name
    }

    /// Global `docker` CLI flags that reach this same daemon (used for `docker compose`):
    /// `--context` for contexts, else `--host` plus the TLS flags.
    public var dockerCLIArguments: [String] {
        if let contextName { return ["--context", contextName] }
        var arguments = ["--host", endpoint.description]
        if let tls {
            arguments.append(tls.verifyPeer ? "--tlsverify" : "--tls")
            if let ca = tls.caFile { arguments += ["--tlscacert", ca] }
            if let certificate = tls.certificateFile { arguments += ["--tlscert", certificate] }
            if let key = tls.keyFile { arguments += ["--tlskey", key] }
        }
        return arguments
    }
}

/// TLS-related settings as the user gave them (flags, config file). Unset fields fall back to
/// the environment and the certificate directory.
public struct TLSRequest: Sendable, Equatable {
    /// `--tls`: encrypt (without verification unless `verify` is also set).
    public var enabled: Bool?
    /// `--tlsverify`: encrypt and verify the server (implies `enabled`).
    public var verify: Bool?
    public var caFile: String?
    public var certificateFile: String?
    public var keyFile: String?
    /// Directory holding `ca.pem`, `cert.pem` and `key.pem` (like `DOCKER_CERT_PATH`).
    public var certificateDirectory: String?

    public init(enabled: Bool? = nil, verify: Bool? = nil, caFile: String? = nil, certificateFile: String? = nil,
                keyFile: String? = nil, certificateDirectory: String? = nil) {
        self.enabled = enabled
        self.verify = verify
        self.caFile = caFile
        self.certificateFile = certificateFile
        self.keyFile = keyFile
        self.certificateDirectory = certificateDirectory
    }

    /// Fills unset fields from `defaults` (per-host settings override global ones).
    public func merged(over defaults: TLSRequest) -> TLSRequest {
        TLSRequest(enabled: enabled ?? defaults.enabled, verify: verify ?? defaults.verify,
                   caFile: caFile ?? defaults.caFile, certificateFile: certificateFile ?? defaults.certificateFile,
                   keyFile: keyFile ?? defaults.keyFile,
                   certificateDirectory: certificateDirectory ?? defaults.certificateDirectory)
    }

    public var isEmpty: Bool { self == TLSRequest() }
}

/// A host the user asked for: an address, a Docker context, or neither (the default host).
public struct HostRequest: Sendable, Equatable {
    public var name: String?
    public var address: String?
    public var context: String?
    public var tls: TLSRequest

    public init(name: String? = nil, address: String? = nil, context: String? = nil, tls: TLSRequest = TLSRequest()) {
        self.name = name
        self.address = address
        self.context = context
        self.tls = tls
    }

    /// Parses `--host` values: `endpoint` or `name=endpoint`.
    public static func parse(_ value: String) -> HostRequest {
        if let equals = value.firstIndex(of: "=") {
            let name = String(value[..<equals]).trimmingWhitespace()
            let address = String(value[value.index(after: equals)...]).trimmingWhitespace()
            if !name.isEmpty, !name.contains("/"), !name.contains(":"), !address.isEmpty {
                return HostRequest(name: name, address: address)
            }
        }
        return HostRequest(address: value)
    }
}

/// What the resolver may look at, injected so resolution is testable.
public struct HostEnvironment: Sendable {
    public var variables: [String: String]
    public var platform: Platform
    public var homeDirectory: String?
    public var fileExists: @Sendable (String) -> Bool
    public var readFile: @Sendable (String) -> String?
    public var listDirectory: @Sendable (String) -> [String]

    public init(variables: [String: String], platform: Platform = .current, homeDirectory: String?,
                fileExists: @escaping @Sendable (String) -> Bool,
                readFile: @escaping @Sendable (String) -> String?,
                listDirectory: @escaping @Sendable (String) -> [String]) {
        self.variables = variables
        self.platform = platform
        self.homeDirectory = homeDirectory
        self.fileExists = fileExists
        self.readFile = readFile
        self.listDirectory = listDirectory
    }

    func variable(_ name: String) -> String? {
        guard let value = variables[name], !value.trimmingWhitespace().isEmpty else { return nil }
        return value
    }

    func join(_ parts: [String]) -> String {
        joinPath(parts, platform: platform)
    }
}

/// Turns host requests into concrete hosts, following the Docker CLI's rules:
///
/// - Address: the request's, else `DOCKER_HOST`, else the active Docker context, else the first
///   well-known socket that exists.
/// - TLS: on with `--tls`/`--tlsverify`, `DOCKER_TLS_VERIFY` (verify) or `DOCKER_TLS`, or an
///   `https://` address. Certificates come from explicit paths, else `DOCKER_CERT_PATH`, else
///   `~/.docker` (`ca.pem`, `cert.pem`, `key.pem`, each used when present).
/// - Contexts carry their own endpoint and TLS material.
public enum DockerHostResolver {
    public struct Result: Sendable, Equatable {
        public var hosts: [DockerHost]
        /// Hosts that were skipped (e.g. ssh:// contexts), with the reason.
        public var warnings: [String]
    }

    /// Resolves every request. An empty list means "the default host".
    public static func resolve(_ requests: [HostRequest], globalTLS: TLSRequest = TLSRequest(),
                               allContexts: Bool = false, environment: HostEnvironment) throws -> Result {
        var hosts: [DockerHost] = []
        var warnings: [String] = []
        var effective = requests
        if effective.isEmpty && !allContexts { effective = [HostRequest()] }
        for request in effective {
            hosts.append(try resolve(request, globalTLS: globalTLS, environment: environment))
        }
        if allContexts {
            let store = DockerContextStore(environment: environment)
            if !hosts.contains(where: { $0.source == "context default" }) && !requests.contains(where: { $0.context == "default" }) {
                var defaultHost = try resolveAddress(HostRequest(name: "default"), globalTLS: globalTLS, environment: environment,
                                                     useEnvironmentHost: true)
                defaultHost.source = "context default"
                hosts.insert(defaultHost, at: 0)
            }
            for context in store.list() where !hosts.contains(where: { $0.source == "context \(context.name)" }) {
                do {
                    hosts.append(try host(for: context, name: nil))
                } catch {
                    warnings.append("context \(context.name): \(describe(error))")
                }
            }
        }
        return Result(hosts: uniquelyNamed(hosts), warnings: warnings)
    }

    /// Resolves one request.
    public static func resolve(_ request: HostRequest, globalTLS: TLSRequest = TLSRequest(),
                               environment: HostEnvironment) throws -> DockerHost {
        let store = DockerContextStore(environment: environment)
        if let contextName = request.context {
            if contextName == "default" {
                var host = try resolveAddress(HostRequest(name: request.name ?? "default", tls: request.tls),
                                              globalTLS: globalTLS, environment: environment, useEnvironmentHost: true)
                host.source = "context default"
                return host
            }
            guard let context = store.context(named: contextName) else {
                throw DockerError.invalidEndpoint("context '\(contextName)' (not found in \(store.metaDirectory ?? "~/.docker/contexts"))")
            }
            return try host(for: context, name: request.name)
        }
        // A bare default request follows DOCKER_HOST, then the active context.
        if request.address == nil, environment.variable("DOCKER_HOST") == nil,
           let active = store.activeContextName(), active != "default", let context = store.context(named: active) {
            var host = try host(for: context, name: request.name)
            if !request.tls.isEmpty || !globalTLS.isEmpty {
                // Explicit TLS flags still apply on top of the context.
                host = try resolveAddress(HostRequest(name: host.name, address: context.host, tls: request.tls),
                                          globalTLS: globalTLS, environment: environment, useEnvironmentHost: false)
                host.source = "context \(context.name)"
            }
            return host
        }
        return try resolveAddress(request, globalTLS: globalTLS, environment: environment, useEnvironmentHost: true)
    }

    static func resolveAddress(_ request: HostRequest, globalTLS: TLSRequest, environment: HostEnvironment,
                               useEnvironmentHost: Bool) throws -> DockerHost {
        let tlsRequest = request.tls.merged(over: globalTLS)
        let address: String?
        let source: String
        if let explicit = request.address {
            address = explicit
            source = "--host"
        } else if useEnvironmentHost, let fromEnvironment = environment.variable("DOCKER_HOST") {
            address = fromEnvironment
            source = "DOCKER_HOST"
        } else {
            address = nil
            source = "default socket"
        }

        let environmentVerify = environment.variable("DOCKER_TLS_VERIFY") != nil
        let environmentTLS = environment.variable("DOCKER_TLS") != nil
        let verify = tlsRequest.verify ?? environmentVerify
        let wantsTLS = verify || (tlsRequest.enabled ?? environmentTLS) || (address.map(DockerEndpoint.schemeImpliesTLS) ?? false)

        let endpoint: DockerEndpoint
        if let address {
            endpoint = try DockerEndpoint.parse(address, tls: wantsTLS)
        } else {
            endpoint = try DockerEndpoint.resolve(override: nil, environment: [:], platform: environment.platform,
                                                  homeDirectory: environment.homeDirectory, fileExists: environment.fileExists)
        }

        var tls: TLSConfiguration?
        if wantsTLS, case .tcp = endpoint {
            tls = try tlsFiles(tlsRequest, verify: verify, environment: environment)
        }
        let name = request.name ?? defaultName(for: endpoint)
        return DockerHost(name: name, endpoint: endpoint, tls: tls, source: source)
    }

    /// Picks the CA, certificate and key files. Explicit paths must exist; defaults are used
    /// only when present. A certificate needs its key and vice versa.
    static func tlsFiles(_ request: TLSRequest, verify: Bool, environment: HostEnvironment) throws -> TLSConfiguration {
        let directory = request.certificateDirectory ?? environment.variable("DOCKER_CERT_PATH")
            ?? environment.homeDirectory.map { environment.join([$0, ".docker"]) }
        func pick(_ explicit: String?, _ fileName: String, flag: String) throws -> String? {
            if let explicit {
                guard environment.fileExists(explicit) else {
                    throw DockerError.tls("\(flag) file \(explicit) does not exist")
                }
                return explicit
            }
            guard let directory else { return nil }
            let candidate = environment.join([directory, fileName])
            return environment.fileExists(candidate) ? candidate : nil
        }
        let ca = try pick(request.caFile, "ca.pem", flag: "--tlscacert")
        let certificate = try pick(request.certificateFile, "cert.pem", flag: "--tlscert")
        let key = try pick(request.keyFile, "key.pem", flag: "--tlskey")
        if (certificate == nil) != (key == nil) {
            let missing = certificate == nil ? "certificate (cert.pem / --tlscert)" : "key (key.pem / --tlskey)"
            throw DockerError.tls("a client certificate needs both a certificate and a key; the \(missing) is missing")
        }
        return TLSConfiguration(verifyPeer: verify, caFile: ca, certificateFile: certificate, keyFile: key)
    }

    static func host(for context: DockerContext, name: String?) throws -> DockerHost {
        guard let address = context.host else {
            throw DockerError.invalidEndpoint("context '\(context.name)' has no Docker endpoint")
        }
        let endpoint = try DockerEndpoint.parse(address, tls: context.hasTLSMaterial)
        var tls: TLSConfiguration?
        if case .tcp = endpoint, context.hasTLSMaterial || DockerEndpoint.schemeImpliesTLS(address) {
            tls = TLSConfiguration(verifyPeer: !context.skipTLSVerify, caFile: context.caFile,
                                   certificateFile: context.certificateFile, keyFile: context.keyFile)
        }
        return DockerHost(name: name ?? context.name, endpoint: endpoint, tls: tls, source: "context \(context.name)")
    }

    /// `local` for sockets and pipes, the host name for TCP.
    public static func defaultName(for endpoint: DockerEndpoint) -> String {
        switch endpoint {
        case .unixSocket, .namedPipe: return "local"
        case .tcp(let host, _): return host
        }
    }

    /// Appends `-2`, `-3`, … to repeated names so every host has a distinct name.
    static func uniquelyNamed(_ hosts: [DockerHost]) -> [DockerHost] {
        var seen: [String: Int] = [:]
        return hosts.map { host in
            var host = host
            let count = (seen[host.name] ?? 0) + 1
            seen[host.name] = count
            if count > 1 { host.name += "-\(count)" }
            return host
        }
    }

    static func describe(_ error: Error) -> String {
        (error as? DockerError)?.description ?? String(describing: error)
    }
}
