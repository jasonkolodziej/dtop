import Foundation
import DtopCore
import DtopUI

/// File operations the theme store needs; injected so saving can be tested without a disk.
public struct ThemeFileSystem: Sendable {
    public var read: @Sendable (String) -> String?
    public var write: @Sendable (_ path: String, _ contents: String) throws -> Void
    public var createDirectory: @Sendable (String) throws -> Void

    public init(
        read: @escaping @Sendable (String) -> String?,
        write: @escaping @Sendable (String, String) throws -> Void,
        createDirectory: @escaping @Sendable (String) throws -> Void
    ) {
        self.read = read
        self.write = write
        self.createDirectory = createDirectory
    }

    public static let local = ThemeFileSystem(
        read: { path in FileManager.default.contents(atPath: path).map { String(decoding: $0, as: UTF8.self) } },
        write: { path, contents in try Data(contents.utf8).write(to: URL(fileURLWithPath: path), options: .atomic) },
        createDirectory: { path in try FileManager.default.createDirectory(atPath: path, withIntermediateDirectories: true) }
    )
}

public struct ThemeSaveResult: Sendable, Equatable {
    /// The theme file written, or nil when an unmodified built-in theme was selected.
    public var themeFile: String?
    public var configFile: String
}

/// Persists themes chosen in the theme editor: `themes/<name>.theme` plus `theme = <name>` in
/// `dtop.conf`, preserving every other line of the user's config.
public struct ThemeStore: Sendable {
    public var configDirectory: String?
    public var configFile: String?
    public var platform: Platform

    public init(configDirectory: String?, configFile: String? = nil, platform: Platform = .current) {
        self.configDirectory = configDirectory
        self.configFile = configFile ?? configDirectory.map { joinPath([$0, "dtop.conf"], platform: platform) }
        self.platform = platform
    }

    public var themesDirectory: String? {
        configDirectory.map { joinPath([$0, "themes"], platform: platform) }
    }

    public func save(_ theme: Theme, name: String, fileSystem: ThemeFileSystem) throws -> ThemeSaveResult {
        guard let configDirectory, let configFile, let themesDirectory else {
            throw ConfigurationError("cannot save themes: no config directory — set HOME or XDG_CONFIG_HOME (APPDATA on Windows)")
        }
        var themeFile: String?
        let isUnmodifiedBuiltIn = Theme.builtIn.contains { $0 == theme && $0.name == name }
        do {
            if !isUnmodifiedBuiltIn {
                try fileSystem.createDirectory(themesDirectory)
                let path = joinPath([themesDirectory, name + ".theme"], platform: platform)
                try fileSystem.write(path, ThemeFile.serialize(theme))
                themeFile = path
            } else {
                try fileSystem.createDirectory(configDirectory)
            }
            let existing = fileSystem.read(configFile) ?? ""
            try fileSystem.write(configFile, Self.settingValue(in: existing, key: "theme", to: name))
        } catch let error as ConfigurationError {
            throw error
        } catch {
            throw ConfigurationError("could not save theme '\(name)': \(error.localizedDescription) — check permissions on \(configDirectory)")
        }
        return ThemeSaveResult(themeFile: themeFile, configFile: configFile)
    }

    /// Sets `key = value` in config text: replaces the first matching line, drops later duplicates
    /// (which would otherwise override it), keeps comments and order, or appends when absent.
    public static func settingValue(in text: String, key: String, to value: String) -> String {
        let normalizedKey = key.lowercased().replacingDashes()
        var lines = text.isEmpty ? [] : text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        if lines.last == "" { lines.removeLast() }
        var replaced = false
        var output: [String] = []
        for line in lines {
            let trimmed = line.trimmingSpaces()
            if !trimmed.hasPrefix("#"), !trimmed.hasPrefix(";"), let equals = trimmed.firstIndex(of: "="),
               String(trimmed[..<equals]).trimmingSpaces().lowercased().replacingDashes() == normalizedKey {
                if !replaced {
                    output.append("\(key) = \(value)")
                    replaced = true
                }
                continue
            }
            output.append(line)
        }
        if !replaced { output.append("\(key) = \(value)") }
        return output.joined(separator: "\n") + "\n"
    }
}
