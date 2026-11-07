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

        let endpoint: DockerEndpoint
        let client: DockerClient
        do {
            endpoint = try DockerEndpoint.resolve(
                override: configuration.host,
                environment: environment,
                platform: platform,
                homeDirectory: home,
                fileExists: { FileManager.default.fileExists(atPath: $0) }
            )
            client = try DockerClient.make(endpoint: endpoint)
        } catch {
            printError("dtop: \(error)")
            return 2
        }

        let refresher = DataRefresher(
            api: client,
            endpointDescription: endpoint.description,
            endpoint: configuration.host == nil && environment["DOCKER_HOST"] == nil ? nil : endpoint,
            historySize: configuration.historySize,
            clock: { Date().timeIntervalSince1970 }
        )

        if configuration.mode == .once {
            return await runOnce(refresher: refresher, environment: environment)
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
            api: client,
            refresher: refresher,
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

    /// Two refreshes one second apart so CPU and I/O rates have a baseline.
    static func runOnce(refresher: DataRefresher, environment: [String: String]) async -> Int32 {
        var snapshot = await refresher.refresh(includeInventory: true)
        if snapshot.connection.isConnected {
            try? await Task.sleep(nanoseconds: 1_000_000_000)
            snapshot = await refresher.refresh(includeInventory: false)
        }
        let width = environment["COLUMNS"].flatMap(Int.init) ?? 120
        let report = OnceReport.render(snapshot: snapshot, width: max(40, width))
        if case .disconnected = snapshot.connection {
            printError(report.trimmingSuffixNewline())
            return 1
        }
        print(report, terminator: "")
        return 0
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
