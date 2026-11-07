/// Parses Docker's RFC 3339 timestamps (`2024-05-01T12:34:56.123456789Z`, optional offsets)
/// into seconds since the Unix epoch, keeping nanosecond precision that `ISO8601DateFormatter`
/// drops. Returns nil for Docker's zero time (`0001-01-01T00:00:00Z`).
public enum DockerTimestamp {
    public static func parse(_ text: String) -> Double? {
        let bytes = Array(text.utf8)
        guard bytes.count >= 20 else { return nil }

        func number(_ start: Int, _ length: Int) -> Int? {
            guard start + length <= bytes.count else { return nil }
            var value = 0
            for index in start..<(start + length) {
                let byte = bytes[index]
                guard byte >= 48 && byte <= 57 else { return nil }
                value = value * 10 + Int(byte - 48)
            }
            return value
        }

        guard let year = number(0, 4), bytes[4] == 45,
              let month = number(5, 2), bytes[7] == 45,
              let day = number(8, 2), bytes[10] == 84 || bytes[10] == 116 || bytes[10] == 32,
              let hour = number(11, 2), bytes[13] == 58,
              let minute = number(14, 2), bytes[16] == 58,
              let second = number(17, 2) else { return nil }
        guard (1...12).contains(month), (1...daysInMonth(month, year: year)).contains(day), hour < 24, minute < 60, second < 61 else {
            return nil
        }
        if year <= 1 { return nil }

        var index = 19
        var fraction = 0.0
        if index < bytes.count, bytes[index] == 46 {
            index += 1
            var scale = 0.1
            while index < bytes.count, bytes[index] >= 48, bytes[index] <= 57 {
                fraction += Double(bytes[index] - 48) * scale
                scale /= 10
                index += 1
            }
        }

        var offsetSeconds = 0
        guard index < bytes.count else { return nil }
        switch bytes[index] {
        case 90, 122: // Z z
            break
        case 43, 45: // + -
            guard let offsetHour = number(index + 1, 2), index + 3 < bytes.count, bytes[index + 3] == 58,
                  let offsetMinute = number(index + 4, 2) else { return nil }
            let sign = bytes[index] == 43 ? 1 : -1
            offsetSeconds = sign * (offsetHour * 3600 + offsetMinute * 60)
        default:
            return nil
        }

        let days = daysFromCivil(year: year, month: month, day: day)
        let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offsetSeconds
        return Double(seconds) + fraction
    }

    /// Days in `month` (1–12) of `year` in the proleptic Gregorian calendar.
    static func daysInMonth(_ month: Int, year: Int) -> Int {
        switch month {
        case 2:
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
            return leap ? 29 : 28
        case 4, 6, 9, 11:
            return 30
        default:
            return 31
        }
    }

    /// Howard Hinnant's days-from-civil algorithm (proleptic Gregorian calendar).
    static func daysFromCivil(year: Int, month: Int, day: Int) -> Int {
        let y = month <= 2 ? year - 1 : year
        let era = (y >= 0 ? y : y - 399) / 400
        let yoe = y - era * 400
        let mp = (month + 9) % 12
        let doy = (153 * mp + 2) / 5 + day - 1
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
        return era * 146_097 + doe - 719_468
    }
}
