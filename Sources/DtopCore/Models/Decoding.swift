/// Docker omits fields or sends `null` liberally across API versions. These helpers keep model
/// initializers short while staying tolerant.
extension KeyedDecodingContainer {
    func lenient<T: Decodable>(_ key: Key, default defaultValue: T) -> T {
        (try? decodeIfPresent(T.self, forKey: key)) .flatMap { $0 } ?? defaultValue
    }

    func lenient<T: Decodable>(_ key: Key) -> T? {
        (try? decodeIfPresent(T.self, forKey: key)) ?? nil
    }

    /// Decodes integers that some daemons send as floats or strings.
    func flexibleInt(_ key: Key) -> Int64? {
        if let value = try? decodeIfPresent(Int64.self, forKey: key) { return value }
        if let value = try? decodeIfPresent(UInt64.self, forKey: key) { return Int64(clamping: value) }
        if let value = try? decodeIfPresent(Double.self, forKey: key), value.isFinite {
            return Int64(value)
        }
        if let value = try? decodeIfPresent(String.self, forKey: key) { return Int64(value) }
        return nil
    }

    func flexibleUInt(_ key: Key) -> UInt64? {
        if let value = try? decodeIfPresent(UInt64.self, forKey: key) { return value }
        if let value = try? decodeIfPresent(Int64.self, forKey: key) { return value < 0 ? nil : UInt64(value) }
        if let value = try? decodeIfPresent(Double.self, forKey: key), value.isFinite, value >= 0 {
            return UInt64(value)
        }
        return nil
    }
}

/// A coding key that accepts any string, for dictionaries keyed by dynamic names.
struct AnyCodingKey: CodingKey {
    var stringValue: String
    var intValue: Int?

    init(_ string: String) {
        stringValue = string
        intValue = nil
    }

    init?(stringValue: String) {
        self.init(stringValue)
    }

    init?(intValue: Int) {
        stringValue = String(intValue)
        self.intValue = intValue
    }
}
