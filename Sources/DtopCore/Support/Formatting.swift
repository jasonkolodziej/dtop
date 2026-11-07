/// Compact, deterministic number formatting for narrow terminal columns (no locale dependence).
public enum Format {
    private static let binaryUnits = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]

    /// `1536` -> `1.5KiB`; values under 10 keep one decimal, larger ones are rounded.
    public static func bytes(_ value: Double) -> String {
        guard value.isFinite, value > 0 else { return "0B" }
        var scaled = value
        var unit = 0
        while scaled >= 1024, unit < binaryUnits.count - 1 {
            scaled /= 1024
            unit += 1
        }
        // Rounding 1023.6 up to 1024 would read oddly; carry into the next unit instead.
        if scaled.rounded() >= 1024, unit < binaryUnits.count - 1 {
            scaled /= 1024
            unit += 1
        }
        if unit == 0 { return "\(Int(scaled.rounded()))B" }
        return (scaled < 10 ? fixed(scaled, decimals: 1) : String(Int(scaled.rounded()))) + binaryUnits[unit]
    }

    public static func bytes<T: BinaryInteger>(_ value: T) -> String {
        bytes(Double(value))
    }

    /// Narrow form for dense columns: `0`, `512B`, `1.5K`, `12M`, `3.2G`.
    public static func compactBytes(_ value: Double) -> String {
        guard value.isFinite, value >= 1 else { return "0" }
        let units = ["B", "K", "M", "G", "T", "P"]
        var scaled = value
        var unit = 0
        while scaled >= 1024, unit < units.count - 1 {
            scaled /= 1024
            unit += 1
        }
        if scaled.rounded() >= 1024, unit < units.count - 1 {
            scaled /= 1024
            unit += 1
        }
        if unit == 0 { return "\(Int(scaled.rounded()))B" }
        return (scaled < 10 ? fixed(scaled, decimals: 1) : String(Int(scaled.rounded()))) + units[unit]
    }

    public static func rate(_ bytesPerSecond: Double) -> String {
        bytes(bytesPerSecond) + "/s"
    }

    public static func percent(_ value: Double, decimals: Int = 1) -> String {
        guard value.isFinite else { return "0%" }
        return fixed(value, decimals: decimals) + "%"
    }

    /// Locale-independent fixed-point formatting.
    public static func fixed(_ value: Double, decimals: Int) -> String {
        guard value.isFinite else { return "0" }
        let factor = pow10(decimals)
        let rounded = (abs(value) * factor).rounded() / factor
        let sign = value < 0 && rounded > 0 ? "-" : ""
        let integer = Int(rounded)
        guard decimals > 0 else { return sign + String(integer) }
        let fraction = Int(((rounded - Double(integer)) * factor).rounded())
        var fractionText = String(fraction)
        while fractionText.count < decimals { fractionText = "0" + fractionText }
        return sign + String(integer) + "." + fractionText
    }

    private static func pow10(_ exponent: Int) -> Double {
        var result = 1.0
        for _ in 0..<max(0, exponent) { result *= 10 }
        return result
    }

    /// `3725` -> `1h 2m`; picks the two most significant units.
    public static func duration(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "0s" }
        let total = Int(seconds)
        let days = total / 86_400
        let hours = (total % 86_400) / 3600
        let minutes = (total % 3600) / 60
        let secs = total % 60
        if days > 0 { return hours > 0 ? "\(days)d \(hours)h" : "\(days)d" }
        if hours > 0 { return minutes > 0 ? "\(hours)h \(minutes)m" : "\(hours)h" }
        if minutes > 0 { return secs > 0 ? "\(minutes)m \(secs)s" : "\(minutes)m" }
        return "\(secs)s"
    }

    /// Relative age such as `5m ago` given explicit times (keeps callers testable).
    public static func age(since timestamp: Double, now: Double) -> String {
        let delta = now - timestamp
        guard delta >= 0 else { return "just now" }
        if delta < 1 { return "just now" }
        return duration(delta) + " ago"
    }
}
