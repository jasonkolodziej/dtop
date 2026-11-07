/// Apple Silicon (and Intel Mac) GPU statistics from the IORegistry `IOAccelerator` entries.
/// `PerformanceStatistics` is undocumented but readable without root; key names vary by GPU
/// generation, so every value is optional.
public enum AppleGPUStatistics {
    public static let utilizationKeys = ["Device Utilization %", "GPU Activity(%)"]
    /// Apple Silicon reports unified-memory use; discrete GPUs in Intel Macs report VRAM.
    public static let memoryKeys = ["In use system memory", "vramUsedBytes"]

    /// Builds a device from one accelerator's registry properties (pure: tested with fixtures).
    public static func device(index: Int, properties: [String: Any]) -> GPUDevice? {
        guard let statistics = properties["PerformanceStatistics"] as? [String: Any] else { return nil }
        let utilization = utilizationKeys.lazy.compactMap { number(statistics[$0]) }.first
        let memory = memoryKeys.lazy.compactMap { number(statistics[$0]) }.first.map { UInt64(max(0, $0)) }
        let name = (properties["model"] as? String)
            ?? (properties["IOGLBundleName"] as? String)
            ?? (properties["IOClass"] as? String)
            ?? "Apple GPU"
        return GPUDevice(id: "apple:\(index)", vendor: .apple, name: name,
                         utilization: utilization.map { min(100, max(0, $0)) }, memoryUsed: memory)
    }

    static func number(_ value: Any?) -> Double? {
        switch value {
        case let value as Double: return value
        case let value as Int: return Double(value)
        case let value as Int64: return Double(value)
        case let value as UInt64: return Double(value)
        case let value as Float: return Double(value)
        default: return nil
        }
    }
}

#if canImport(IOKit)
import IOKit
import Foundation

/// Polls `IOAccelerator` services through IOKit (macOS only, no root required).
public struct AppleGPUCollector: GPUCollecting {
    public init() {}

    public func sample(now: Double) async -> GPUSample {
        var devices: [GPUDevice] = []
        var iterator: io_iterator_t = 0
        guard IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOAccelerator"), &iterator) == KERN_SUCCESS else {
            return GPUSample(issues: ["could not query IOAccelerator services"])
        }
        defer { IOObjectRelease(iterator) }
        var index = 0
        while case let service = IOIteratorNext(iterator), service != 0 {
            defer { IOObjectRelease(service) }
            var unmanaged: Unmanaged<CFMutableDictionary>?
            guard IORegistryEntryCreateCFProperties(service, &unmanaged, kCFAllocatorDefault, 0) == KERN_SUCCESS,
                  let properties = unmanaged?.takeRetainedValue() as? [String: Any] else { continue }
            if let device = AppleGPUStatistics.device(index: index, properties: properties) {
                devices.append(device)
                index += 1
            }
        }
        // Containers on macOS run inside Docker Desktop's Linux VM, which has no GPU access, so
        // there is nothing to attribute per container.
        return GPUSample(devices: devices, processAttributionComplete: false)
    }
}
#endif
