import Foundation

/// A Docker CLI context (`docker context ls`), as stored under `~/.docker/contexts`.
public struct DockerContext: Sendable, Equatable {
    public var name: String
    public var description: String?
    /// The context's Docker endpoint (`Endpoints.docker.Host`).
    public var host: String?
    public var skipTLSVerify: Bool
    public var caFile: String?
    public var certificateFile: String?
    public var keyFile: String?

    public init(name: String, description: String? = nil, host: String?, skipTLSVerify: Bool = false,
                caFile: String? = nil, certificateFile: String? = nil, keyFile: String? = nil) {
        self.name = name
        self.description = description
        self.host = host
        self.skipTLSVerify = skipTLSVerify
        self.caFile = caFile
        self.certificateFile = certificateFile
        self.keyFile = keyFile
    }

    /// Whether `docker context create --docker "…,ca=…,cert=…,key=…"` stored TLS files.
    public var hasTLSMaterial: Bool { caFile != nil || certificateFile != nil || keyFile != nil }
}

/// Reads contexts the way the Docker CLI stores them:
///
/// - `$DOCKER_CONFIG` (default `~/.docker`) `/config.json` names the `currentContext`;
///   `DOCKER_CONTEXT` overrides it.
/// - `contexts/meta/<sha256(name)>/meta.json` holds the name and endpoint.
/// - `contexts/tls/<sha256(name)>/docker/{ca,cert,key}.pem` holds TLS material.
public struct DockerContextStore: Sendable {
    public let environment: HostEnvironment

    public init(environment: HostEnvironment) {
        self.environment = environment
    }

    /// The Docker CLI's configuration directory.
    public var configDirectory: String? {
        if let configured = environment.variable("DOCKER_CONFIG") { return configured }
        return environment.homeDirectory.map { environment.join([$0, ".docker"]) }
    }

    public var metaDirectory: String? {
        configDirectory.map { environment.join([$0, "contexts", "meta"]) }
    }

    /// `DOCKER_CONTEXT`, else `currentContext` from config.json, else nil (the default context).
    public func activeContextName() -> String? {
        if let fromEnvironment = environment.variable("DOCKER_CONTEXT") { return fromEnvironment.trimmingWhitespace() }
        guard let directory = configDirectory,
              let text = environment.readFile(environment.join([directory, "config.json"])) else { return nil }
        struct Config: Decodable {
            var currentContext: String?
        }
        guard let config = try? JSONDecoder().decode(Config.self, from: Data(text.utf8)),
              let current = config.currentContext?.trimmingWhitespace(), !current.isEmpty else { return nil }
        return current
    }

    /// Loads one context by name, or nil when it does not exist or cannot be read.
    public func context(named name: String) -> DockerContext? {
        guard let meta = metaDirectory else { return nil }
        return load(directory: environment.join([meta, SHA256.hexDigest(name)]))
    }

    /// Every stored context, sorted by name (the built-in `default` context is not stored).
    public func list() -> [DockerContext] {
        guard let meta = metaDirectory else { return [] }
        return environment.listDirectory(meta)
            .compactMap { load(directory: environment.join([meta, $0])) }
            .sorted { $0.name < $1.name }
    }

    private func load(directory: String) -> DockerContext? {
        guard let text = environment.readFile(environment.join([directory, "meta.json"])) else { return nil }
        struct Meta: Decodable {
            struct Metadata: Decodable {
                var Description: String?
            }
            struct Endpoint: Decodable {
                var Host: String?
                var SkipTLSVerify: Bool?
            }
            var Name: String
            var Metadata: Metadata?
            var Endpoints: [String: Endpoint]?
        }
        guard let meta = try? JSONDecoder().decode(Meta.self, from: Data(text.utf8)) else { return nil }
        let endpoint = meta.Endpoints?["docker"]
        var context = DockerContext(name: meta.Name, description: meta.Metadata?.Description, host: endpoint?.Host,
                                    skipTLSVerify: endpoint?.SkipTLSVerify ?? false)
        if let configDirectory {
            let tlsDirectory = environment.join([configDirectory, "contexts", "tls", SHA256.hexDigest(meta.Name), "docker"])
            func file(_ name: String) -> String? {
                let path = environment.join([tlsDirectory, name])
                return environment.fileExists(path) ? path : nil
            }
            context.caFile = file("ca.pem")
            context.certificateFile = file("cert.pem")
            context.keyFile = file("key.pem")
        }
        return context
    }
}
