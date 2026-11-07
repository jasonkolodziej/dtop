import DtopCore
import DtopUI

/// Resolves a theme name or file path to a `Theme`, looking in the user's themes directory.
public struct ThemeLoader: Sendable {
    public var themesDirectory: String?
    public var platform: Platform

    public init(themesDirectory: String?, platform: Platform = .current) {
        self.themesDirectory = themesDirectory
        self.platform = platform
    }

    /// Built-in names plus `*.theme` files found in the themes directory.
    public func availableNames(listDirectory: (String) -> [String]) -> [String] {
        var names = Theme.builtIn.map(\.name)
        if let themesDirectory {
            let custom = listDirectory(themesDirectory)
                .filter { $0.hasSuffix(".theme") }
                .map { String($0.dropLast(6)).lowercased() }
                .filter { !names.contains($0) }
                .sorted()
            names += custom
        }
        return names
    }

    public func load(_ nameOrPath: String, readFile: (String) -> String?) throws -> Theme {
        if let builtIn = Theme.named(nameOrPath) { return builtIn }
        let looksLikePath = nameOrPath.contains("/") || nameOrPath.contains("\\") || nameOrPath.hasSuffix(".theme")
        var candidates: [String] = []
        if looksLikePath { candidates.append(nameOrPath) }
        if let themesDirectory {
            let file = nameOrPath.hasSuffix(".theme") ? nameOrPath : nameOrPath + ".theme"
            candidates.append(joinPath([themesDirectory, file], platform: platform))
        }
        for candidate in candidates {
            guard let text = readFile(candidate) else { continue }
            let baseName = Self.baseName(of: candidate, platform: platform)
            do {
                return try ThemeFile.parse(text, fallbackName: baseName)
            } catch let error as ThemeParseError {
                throw ConfigurationError("\(candidate): \(error)")
            }
        }
        let builtIns = Theme.builtIn.map(\.name).joined(separator: ", ")
        let location = themesDirectory.map { " or a file in \($0)" } ?? ""
        throw ConfigurationError("unknown theme '\(nameOrPath)' — use one of \(builtIns)\(location)")
    }

    static func baseName(of path: String, platform: Platform) -> String {
        let separators: Set<Character> = platform == .windows ? ["/", "\\"] : ["/"]
        let file = path.split(whereSeparator: { separators.contains($0) }).last.map(String.init) ?? path
        return file.hasSuffix(".theme") ? String(file.dropLast(6)) : file
    }
}
