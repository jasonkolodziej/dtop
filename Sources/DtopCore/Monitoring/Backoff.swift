/// Exponential reconnect backoff with an upper bound and optional jitter. The random source is
/// injectable so behaviour is deterministic under test.
public struct Backoff: Sendable, Equatable {
    public let initialDelay: Double
    public let maximumDelay: Double
    public let multiplier: Double
    /// 0 disables jitter; 0.2 means +/-20%.
    public let jitter: Double
    public private(set) var attempt = 0

    public init(initialDelay: Double = 0.5, maximumDelay: Double = 30, multiplier: Double = 2, jitter: Double = 0.2) {
        precondition(initialDelay > 0 && maximumDelay >= initialDelay && multiplier >= 1, "invalid backoff configuration")
        self.initialDelay = initialDelay
        self.maximumDelay = maximumDelay
        self.multiplier = multiplier
        self.jitter = max(0, min(1, jitter))
    }

    /// Delay for the current attempt before jitter.
    public var baseDelay: Double {
        var delay = initialDelay
        var remaining = attempt
        while remaining > 0, delay < maximumDelay {
            delay *= multiplier
            remaining -= 1
        }
        return min(delay, maximumDelay)
    }

    /// Returns the delay to wait and advances to the next attempt. `random` must return 0..<1.
    public mutating func nextDelay(random: () -> Double = { Double.random(in: 0..<1) }) -> Double {
        let base = baseDelay
        attempt += 1
        guard jitter > 0 else { return base }
        let factor = 1 + jitter * (random() * 2 - 1)
        return min(maximumDelay, max(0, base * factor))
    }

    public mutating func reset() {
        attempt = 0
    }
}
