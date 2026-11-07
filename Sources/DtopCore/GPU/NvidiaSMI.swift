/// Builds `nvidia-smi` invocations and parses their CSV / table output. Pure, so it is tested
/// with recorded output; the collector runs the commands.
public enum NvidiaSMI {
    public static let deviceQueryArguments = [
        "--query-gpu=index,uuid,name,utilization.gpu,memory.used,memory.total,temperature.gpu",
        "--format=csv,noheader,nounits"
    ]
    public static let computeAppsArguments = [
        "--query-compute-apps=pid,gpu_uuid,used_memory",
        "--format=csv,noheader,nounits"
    ]
    /// One sample of per-process utilization (`sm` column).
    public static let processMonitorArguments = ["pmon", "-c", "1", "-s", "u"]

    public struct Device: Sendable, Equatable {
        public var index: Int
        public var uuid: String
        public var device: GPUDevice
    }

    /// `0, GPU-8f6c…, NVIDIA GeForce RTX 3080, 45, 3210, 10240, 61`
    /// Memory is reported in MiB. `[N/A]` / `[Not Supported]` become nil.
    public static func parseDevices(_ output: String) -> [Device] {
        output.split(separator: "\n").compactMap { line in
            let fields = splitCSV(line)
            guard fields.count >= 6, let index = Int(fields[0]) else { return nil }
            let uuid = fields[1]
            return Device(index: index, uuid: uuid, device: GPUDevice(
                id: "nvidia:" + uuid,
                vendor: .nvidia,
                name: fields[2],
                utilization: number(fields[3]),
                memoryUsed: number(fields[4]).map { UInt64($0 * 1_048_576) },
                memoryTotal: number(fields[5]).map { UInt64($0 * 1_048_576) },
                temperature: fields.count > 6 ? number(fields[6]) : nil
            ))
        }
    }

    /// `4242, GPU-8f6c…, 1536` → pid, device UUID, used memory (MiB).
    public static func parseComputeApps(_ output: String) -> [(pid: Int, uuid: String, memoryBytes: UInt64?)] {
        output.split(separator: "\n").compactMap { line in
            let fields = splitCSV(line)
            guard fields.count >= 3, let pid = Int(fields[0]) else { return nil }
            return (pid, fields[1], number(fields[2]).map { UInt64($0 * 1_048_576) })
        }
    }

    /// Parses `nvidia-smi pmon -c 1 -s u`:
    ///
    ///     # gpu         pid   type     sm    mem    enc    dec    command
    ///     # Idx           #    C/G      %      %      %      %    name
    ///         0       4242     C     45     12      -      -    python
    ///
    /// Returns per (gpu index, pid) the SM utilization; `-` means no sample.
    public static func parseProcessMonitor(_ output: String) -> [(index: Int, pid: Int, utilization: Double?)] {
        var columns: [String] = []
        var rows: [(Int, Int, Double?)] = []
        for rawLine in output.split(separator: "\n") {
            let line = String(rawLine).trimmingWhitespace()
            if line.hasPrefix("#") {
                if columns.isEmpty {
                    columns = line.dropFirst().split(separator: " ").map { String($0).lowercased() }
                }
                continue
            }
            let fields = line.split(separator: " ", omittingEmptySubsequences: true).map(String.init)
            let gpuColumn = columns.firstIndex(of: "gpu") ?? 0
            let pidColumn = columns.firstIndex(of: "pid") ?? 1
            let smColumn = columns.firstIndex(of: "sm") ?? 3
            guard fields.count > max(gpuColumn, pidColumn, smColumn),
                  let index = Int(fields[gpuColumn]), let pid = Int(fields[pidColumn]) else { continue }
            rows.append((index, pid, number(fields[smColumn])))
        }
        return rows
    }

    static func splitCSV(_ line: Substring) -> [String] {
        line.split(separator: ",", omittingEmptySubsequences: false).map { String($0).trimmingWhitespace() }
    }

    /// Numbers, or nil for `[N/A]`, `[Not Supported]`, `N/A`, `-` and empty fields.
    static func number(_ text: String) -> Double? {
        let value = text.trimmingWhitespace()
        guard !value.isEmpty, !value.hasPrefix("["), value != "N/A", value != "-" else { return nil }
        return Double(value)
    }
}
