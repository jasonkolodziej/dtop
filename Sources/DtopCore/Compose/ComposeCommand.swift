import Foundation

/// Compose stack operations. Operations the Engine API can express (start/stop/restart/pause)
/// are applied container by container; ones that need the compose file (up/down/pull) shell out
/// to `docker compose` using the working directory and config files recorded in the labels.
public enum ComposeOperation: String, Sendable, Equatable, CaseIterable {
    case up
    case down
    case start
    case stop
    case restart
    case pause
    case unpause
    case pull

    /// Whether the operation can run against the Engine API without the compose CLI.
    public var isEngineNative: Bool {
        switch self {
        case .start, .stop, .restart, .pause, .unpause: return true
        case .up, .down, .pull: return false
        }
    }

    public var isDestructive: Bool { self == .down }

    /// The equivalent per-container action and which containers it applies to.
    public func containerAction(stopTimeout: Int? = nil) -> ContainerAction? {
        switch self {
        case .start: return .start
        case .stop: return .stop(timeoutSeconds: stopTimeout)
        case .restart: return .restart(timeoutSeconds: stopTimeout)
        case .pause: return .pause
        case .unpause: return .unpause
        default: return nil
        }
    }

    public func applies(to container: Container) -> Bool {
        switch self {
        case .start: return container.state == .exited || container.state == .created || container.state == .dead
        case .stop: return container.state == .running || container.state == .restarting || container.state == .paused
        case .restart: return container.state != .removing
        case .pause: return container.state == .running
        case .unpause: return container.state == .paused
        default: return false
        }
    }
}

/// A fully specified external command, kept as data so construction is testable.
public struct CommandInvocation: Sendable, Equatable {
    public var executable: String
    public var arguments: [String]
    public var workingDirectory: String?

    public init(executable: String, arguments: [String], workingDirectory: String? = nil) {
        self.executable = executable
        self.arguments = arguments
        self.workingDirectory = workingDirectory
    }

    /// Shell-quoted rendering for display (never executed through a shell).
    public var displayString: String {
        ([executable] + arguments).map(Self.quote).joined(separator: " ")
    }

    static func quote(_ argument: String) -> String {
        let safe = argument.allSatisfy { $0.isLetter || $0.isNumber || "-_./:=@,+".contains($0) }
        if safe && !argument.isEmpty { return argument }
        return "'" + argument.split(separator: "'", omittingEmptySubsequences: false).joined(separator: #"'\''"#) + "'"
    }
}

public enum ComposeCommandBuilder {
    /// Builds `docker compose -p <name> [--project-directory dir] [-f file]... <op> [flags]`.
    public static func invocation(
        for operation: ComposeOperation,
        project: ComposeProject,
        dockerExecutable: String = "docker",
        endpoint: DockerEndpoint? = nil
    ) -> CommandInvocation {
        var arguments: [String] = []
        if let endpoint {
            arguments += ["--host", endpoint.description]
        }
        arguments += ["compose", "--project-name", project.name]
        if let directory = project.workingDirectory, !directory.isEmpty {
            arguments += ["--project-directory", directory]
        }
        for file in project.configFiles {
            arguments += ["--file", file]
        }
        arguments.append(operation.rawValue)
        switch operation {
        case .up:
            arguments += ["--detach"]
        case .down:
            arguments += ["--remove-orphans"]
        default:
            break
        }
        return CommandInvocation(
            executable: dockerExecutable,
            arguments: arguments,
            workingDirectory: project.workingDirectory
        )
    }
}

public struct CommandResult: Sendable, Equatable {
    public var exitCode: Int32
    public var output: String

    public init(exitCode: Int32, output: String) {
        self.exitCode = exitCode
        self.output = output
    }

    public var succeeded: Bool { exitCode == 0 }
}

/// Runs external commands; abstracted so the app can be tested without spawning processes.
public protocol CommandRunning: Sendable {
    func run(_ invocation: CommandInvocation) async throws -> CommandResult
}

/// Spawns processes with Foundation's `Process`, resolving the executable via `PATH`.
public struct ProcessCommandRunner: CommandRunning {
    public let environment: [String: String]
    public let platform: Platform

    public init(environment: [String: String] = ProcessInfo.processInfo.environment, platform: Platform = .current) {
        self.environment = environment
        self.platform = platform
    }

    public func run(_ invocation: CommandInvocation) async throws -> CommandResult {
        guard let executable = Self.resolve(invocation.executable, environment: environment, platform: platform,
                                            fileExists: { FileManager.default.isExecutableFile(atPath: $0) }) else {
            throw DockerError.io("'\(invocation.executable)' was not found on PATH — install the Docker CLI with "
                + "the compose plugin to use compose up/down/pull")
        }
        return try await runBlocking {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: executable)
            process.arguments = invocation.arguments
            if let directory = invocation.workingDirectory, FileManager.default.fileExists(atPath: directory) {
                process.currentDirectoryURL = URL(fileURLWithPath: directory)
            }
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            process.standardInput = FileHandle.nullDevice
            do {
                try process.run()
            } catch {
                throw DockerError.io("failed to launch \(invocation.displayString): \(error)")
            }
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return CommandResult(exitCode: process.terminationStatus, output: String(decoding: data, as: UTF8.self))
        }
    }

    /// Finds an executable on PATH (adding `.exe` on Windows).
    public static func resolve(
        _ name: String,
        environment: [String: String],
        platform: Platform,
        fileExists: (String) -> Bool
    ) -> String? {
        if name.contains("/") || (platform == .windows && name.contains("\\")) {
            return fileExists(name) ? name : nil
        }
        let pathValue = environment["PATH"] ?? environment["Path"] ?? ""
        let separator: Character = platform == .windows ? ";" : ":"
        let suffixes = platform == .windows && !name.lowercased().hasSuffix(".exe") ? [".exe", ""] : [""]
        for directory in pathValue.split(separator: separator) where !directory.isEmpty {
            for suffix in suffixes {
                let candidate = joinPath([String(directory), name + suffix], platform: platform)
                if fileExists(candidate) { return candidate }
            }
        }
        return nil
    }
}
