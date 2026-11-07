/// A process inside a container, parsed from `docker top` output.
public struct ContainerProcess: Sendable, Equatable {
    public var pid: Int
    public var parentPID: Int?
    public var user: String
    public var cpu: String
    public var command: String

    public init(pid: Int, parentPID: Int?, user: String = "", cpu: String = "", command: String) {
        self.pid = pid
        self.parentPID = parentPID
        self.user = user
        self.cpu = cpu
        self.command = command
    }
}

/// One flattened row of a rendered tree.
public struct ProcessTreeRow: Sendable, Equatable {
    public var process: ContainerProcess
    public var depth: Int
    /// For each ancestor level, whether a vertical guide continues below this row.
    public var guides: [Bool]
    public var isLast: Bool

    public init(process: ContainerProcess, depth: Int, guides: [Bool], isLast: Bool) {
        self.process = process
        self.depth = depth
        self.guides = guides
        self.isLast = isLast
    }

    /// Box-drawing prefix such as `│  ├─ `.
    public var prefix: String {
        guard depth > 0 else { return "" }
        var text = ""
        for continues in guides.dropFirst() {
            text += continues ? "│  " : "   "
        }
        text += isLast ? "└─ " : "├─ "
        return text
    }
}

public enum ProcessTree {
    /// Parses `docker top` output by column title so it works for `ps -ef` style output (Linux)
    /// and the reduced columns of Windows containers.
    public static func parse(_ top: ContainerTop) -> [ContainerProcess] {
        let titles = top.titles.map { $0.uppercased() }
        func column(_ names: [String]) -> Int? { titles.firstIndex { names.contains($0) } }
        guard let pidColumn = column(["PID"]) else { return [] }
        let ppidColumn = column(["PPID"])
        let userColumn = column(["UID", "USER"])
        let cpuColumn = column(["C", "%CPU", "CPU"])
        let commandColumn = column(["CMD", "COMMAND", "NAME", "ARGS"])

        return top.processes.compactMap { row in
            guard pidColumn < row.count, let pid = Int(row[pidColumn].trimmingWhitespace()) else { return nil }
            func value(_ index: Int?) -> String {
                guard let index, index < row.count else { return "" }
                return row[index]
            }
            let command: String
            if let commandColumn, commandColumn < row.count {
                // `ps` may split a command containing spaces across trailing cells.
                command = row[commandColumn...].joined(separator: " ")
            } else {
                command = row.last ?? ""
            }
            return ContainerProcess(
                pid: pid,
                parentPID: ppidColumn.flatMap { $0 < row.count ? Int(row[$0].trimmingWhitespace()) : nil },
                user: value(userColumn),
                cpu: value(cpuColumn),
                command: command
            )
        }
    }

    /// Builds a depth-first flattened tree. Processes whose parent is not in the list (the
    /// container's init, usually) become roots. Cycles are broken defensively.
    public static func build(_ processes: [ContainerProcess]) -> [ProcessTreeRow] {
        let pids = Set(processes.map(\.pid))
        var children: [Int: [ContainerProcess]] = [:]
        var roots: [ContainerProcess] = []
        for process in processes {
            if let parent = process.parentPID, parent != process.pid, pids.contains(parent) {
                children[parent, default: []].append(process)
            } else {
                roots.append(process)
            }
        }
        for key in children.keys { children[key]?.sort { $0.pid < $1.pid } }
        roots.sort { $0.pid < $1.pid }

        var rows: [ProcessTreeRow] = []
        var visited = Set<Int>()

        func visit(_ process: ContainerProcess, depth: Int, guides: [Bool], isLast: Bool) {
            guard visited.insert(process.pid).inserted else { return }
            rows.append(ProcessTreeRow(process: process, depth: depth, guides: guides, isLast: isLast))
            let kids = children[process.pid] ?? []
            for (index, child) in kids.enumerated() {
                visit(child, depth: depth + 1, guides: guides + [!isLast], isLast: index == kids.count - 1)
            }
        }
        for (index, root) in roots.enumerated() {
            visit(root, depth: 0, guides: [], isLast: index == roots.count - 1)
        }
        // Anything only reachable through a cycle is appended flat so no process disappears.
        for process in processes where !visited.contains(process.pid) {
            visit(process, depth: 0, guides: [], isLast: true)
        }
        return rows
    }
}
