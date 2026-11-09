import Foundation
import DtopCore
import DtopUI

/// Process entry point logic, kept in the library so the executable stays a one-liner.
public enum DtopMain {
    public static func run(arguments: [String]) async -> Int32 {
        let environment = ProcessInfo.processInfo.environment
        let platform = Platform.current
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let configDirectory = AppConfiguration.configDirectory(environment: environment, platform: platform,
                                                               homeDirectory: home)

        var configuration = AppConfiguration()
        var explicitConfig: String?
        do {
            explicitConfig = try AppConfiguration.configPathArgument(arguments)
            let defaultConfig = configDirectory.map { joinPath([$0, "dtop.conf"], platform: platform) }
            if let path = explicitConfig ?? defaultConfig {
                if let text = readTextFile(path) {
                    try configuration.apply(configFile: text)
                } else if explicitConfig != nil {
                    throw ConfigurationError("config file '\(path)' could not be read — check the path and permissions")
                }
            }
            try configuration.apply(arguments: arguments)
        } catch {
            printError("dtop: \(error)")
            return 2
        }

        let themeLoader = ThemeLoader(
            themesDirectory: configDirectory.map { joinPath([$0, "themes"], platform: platform) },
            platform: platform
        )

        switch configuration.mode {
        case .help:
            print(AppConfiguration.usage)
            return 0
        case .version:
            print("\(AppProfile.current.name) \(AppProfile.current.version)")
            return 0
        case .listThemes:
            for name in themeLoader.availableNames(listDirectory: listDirectory) { print(name) }
            return 0
        case .printTheme(let name):
            do {
                print(ThemeFile.serialize(try themeLoader.load(name, readFile: readTextFile)), terminator: "")
                return 0
            } catch {
                printError("dtop: \(error)")
                return 2
            }
        case .once, .interactive:
            break
        }

        let hostEnvironment = HostEnvironment(
            variables: environment, platform: platform, homeDirectory: home,
            fileExists: { FileManager.default.fileExists(atPath: $0) },
            readFile: { readTextFile($0) },
            listDirectory: { listDirectory($0) }
        )
        let connections: [HostConnection]
        let warnings: [String]
        do {
            let resolved = try DockerHostResolver.resolve(configuration.hosts, globalTLS: configuration.globalTLS,
                                                          allContexts: configuration.allContexts, environment: hostEnvironment)
            warnings = resolved.warnings
            guard !resolved.hosts.isEmpty else {
                throw ConfigurationError("no Docker hosts to monitor — pass --host or --context")
            }
            let gpu = configuration.gpu ? SystemGPUCollector.makeDefault(environment: environment, platform: platform) : nil
            // GPU usage is measured on this machine, so it belongs to the first local daemon.
            let gpuHost = resolved.hosts.firstIndex { $0.endpoint.isLocal }
            connections = try resolved.hosts.enumerated().map { index, host in
                let client = try DockerClient.make(host: host)
                let refresher = DataRefresher(
                    api: client,
                    endpointDescription: host.description,
                    // Compose commands name the host unless it is the docker CLI's own default.
                    host: host.source == "default socket" ? nil : host,
                    gpuCollector: index == gpuHost ? gpu : nil,
                    historySize: configuration.historySize,
                    clock: { Date().timeIntervalSince1970 }
                )
                return HostConnection(host: host, api: client, refresher: refresher)
            }
        } catch {
            printError("dtop: \(error)")
            return 2
        }

        if configuration.mode == .once {
            for warning in warnings { printError("dtop: skipped \(warning)") }
            return await runOnce(hosts: connections, environment: environment)
        }

        guard Terminal.isInteractive else {
            printError("dtop: \(TerminalError.notATerminal)")
            return 2
        }
        let theme: Theme
        do {
            theme = try themeLoader.load(configuration.themeName, readFile: readTextFile)
        } catch {
            printError("dtop: \(error)")
            return 2
        }
        let controller = await AppController(
            configuration: configuration,
            terminal: Terminal(),
            hosts: connections,
            notice: warnings.isEmpty ? nil : "Skipped " + warnings.joined(separator: "; "),
            theme: theme,
            colorMode: configuration.colorMode ?? ColorMode.detect(environment: environment),
            themeLoader: themeLoader,
            themeNames: themeLoader.availableNames(listDirectory: listDirectory),
            readFile: readTextFile,
            themeStore: ThemeStore(configDirectory: configDirectory, configFile: explicitConfig, platform: platform)
        )
        do {
            try await controller.run()
        } catch {
            printError("dtop: \(error)")
            return 1
        }
        return 0
    }

    /// Two refreshes one second apart so CPU and I/O rates have a baseline. Every host is polled
    /// concurrently; the exit status is 1 if any host could not be reached.
    static func runOnce(hosts: [HostConnection], environment: [String: String]) async -> Int32 {
        let snapshots = await withTaskGroup(of: (Int, DockerSnapshot).self) { group in
            for (index, host) in hosts.enumerated() {
                group.addTask {
                    var snapshot = await host.refresher.refresh(includeInventory: true)
                    if snapshot.connection.isConnected {
                        try? await Task.sleep(nanoseconds: 1_000_000_000)
                        snapshot = await host.refresher.refresh(includeInventory: false)
                    }
                    return (index, snapshot)
                }
            }
            var results: [Int: DockerSnapshot] = [:]
            for await (index, snapshot) in group { results[index] = snapshot }
            return hosts.indices.map { results[$0]! }
        }
        let width = max(40, environment["COLUMNS"].flatMap(Int.init) ?? 120)
        let report = OnceReport.render(hosts: zip(hosts, snapshots).map { ($0.host.name, $1) }, width: width)
        let failed = snapshots.contains { if case .disconnected = $0.connection { return true } else { return false } }
        if failed && snapshots.count == 1 {
            printError(report.trimmingSuffixNewline())
            return 1
        }
        print(report, terminator: "")
        return failed ? 1 : 0
    }

    @Sendable static func readTextFile(_ path: String) -> String? {
        guard let data = FileManager.default.contents(atPath: path) else { return nil }
        return String(decoding: data, as: UTF8.self)
    }

    static func listDirectory(_ path: String) -> [String] {
        (try? FileManager.default.contentsOfDirectory(atPath: path)) ?? []
    }

    static func printError(_ message: String) {
        FileHandle.standardError.write(Data((message + "\n").utf8))
    }
}

extension String {
    func trimmingSuffixNewline() -> String {
        var text = self
        while text.hasSuffix("\n") { text.removeLast() }
        return text
    }
}
