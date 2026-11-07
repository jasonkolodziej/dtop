public enum TextAlignment: Sendable, Equatable {
    case left
    case right
    case center
}

/// Terminal column widths for text. This is a compact wcwidth: combining marks and control
/// characters take no space, East Asian wide/fullwidth characters and emoji take two.
public enum TextWidth {
    public static func of(_ scalar: Unicode.Scalar) -> Int {
        let value = scalar.value
        if value == 0 { return 0 }
        if value < 0x20 || (0x7F..<0xA0).contains(value) { return 0 }
        if isZeroWidth(value) { return 0 }
        if isWide(value) { return 2 }
        return 1
    }

    public static func of(_ character: Character) -> Int {
        let scalars = character.unicodeScalars
        guard let first = scalars.first else { return 0 }
        // Emoji presentation selector or a ZWJ sequence renders as a single wide glyph.
        if scalars.count > 1, scalars.contains(where: { $0.value == 0xFE0F || $0.value == 0x200D }) {
            return 2
        }
        let base = of(first)
        if base == 0 {
            return scalars.dropFirst().map(of).max() ?? 0
        }
        return base
    }

    public static func of(_ text: String) -> Int {
        text.reduce(0) { $0 + of($1) }
    }

    static func isZeroWidth(_ value: UInt32) -> Bool {
        switch value {
        case 0x0300...0x036F, 0x0483...0x0489, 0x0591...0x05BD, 0x0610...0x061A, 0x064B...0x065F,
             0x0E31, 0x0E34...0x0E3A, 0x1AB0...0x1AFF, 0x1DC0...0x1DFF, 0x200B...0x200F, 0x2028...0x202E,
             0x2060...0x2064, 0x20D0...0x20FF, 0xFE00...0xFE0F, 0xFE20...0xFE2F, 0xFEFF, 0xE0100...0xE01EF:
            return true
        default:
            return false
        }
    }

    static func isWide(_ value: UInt32) -> Bool {
        switch value {
        case 0x1100...0x115F, 0x231A...0x231B, 0x2329...0x232A, 0x23E9...0x23EC, 0x23F0, 0x23F3,
             0x25FD...0x25FE, 0x2614...0x2615, 0x2648...0x2653, 0x267F, 0x2693, 0x26A1, 0x26AA...0x26AB,
             0x26BD...0x26BE, 0x26C4...0x26C5, 0x26CE, 0x26D4, 0x26EA, 0x26F2...0x26F3, 0x26F5, 0x26FA,
             0x26FD, 0x2705, 0x270A...0x270B, 0x2728, 0x274C, 0x274E, 0x2753...0x2755, 0x2757,
             0x2795...0x2797, 0x27B0, 0x27BF, 0x2B1B...0x2B1C, 0x2B50, 0x2B55,
             0x2E80...0x303E, 0x3041...0x33FF, 0x3400...0x4DBF, 0x4E00...0x9FFF, 0xA000...0xA4CF,
             0xA960...0xA97F, 0xAC00...0xD7A3, 0xF900...0xFAFF, 0xFE10...0xFE19, 0xFE30...0xFE6F,
             0xFF00...0xFF60, 0xFFE0...0xFFE6, 0x1F004, 0x1F0CF, 0x1F18E, 0x1F191...0x1F19A,
             0x1F200...0x1F251, 0x1F300...0x1F64F, 0x1F680...0x1F6FF, 0x1F7E0...0x1F7EB,
             0x1F900...0x1F9FF, 0x1FA70...0x1FAFF, 0x20000...0x2FFFD, 0x30000...0x3FFFD:
            return true
        default:
            return false
        }
    }

    /// Cuts text to fit `width` columns, appending `ellipsis` when something was removed.
    public static func truncate(_ text: String, to width: Int, ellipsis: String = "…") -> String {
        guard width > 0 else { return "" }
        if of(text) <= width { return text }
        let ellipsisWidth = of(ellipsis)
        let budget = width - ellipsisWidth
        guard budget >= 0 else { return prefix(ellipsis, width: width) }
        return prefix(text, width: budget) + ellipsis
    }

    /// Longest prefix that fits in `width` columns (never splits a wide character).
    public static func prefix(_ text: String, width: Int) -> String {
        var used = 0
        var result = ""
        for character in text {
            let w = of(character)
            if used + w > width { break }
            used += w
            result.append(character)
        }
        return result
    }

    /// Truncates or pads to exactly `width` columns.
    public static func fit(_ text: String, to width: Int, alignment: TextAlignment = .left, ellipsis: String = "…") -> String {
        guard width > 0 else { return "" }
        let cut = truncate(text, to: width, ellipsis: ellipsis)
        let padding = max(0, width - of(cut))
        switch alignment {
        case .left:
            return cut + String(repeating: " ", count: padding)
        case .right:
            return String(repeating: " ", count: padding) + cut
        case .center:
            let left = padding / 2
            return String(repeating: " ", count: left) + cut + String(repeating: " ", count: padding - left)
        }
    }

    /// Greedy word wrap; words longer than the width are hard-broken.
    public static func wrap(_ text: String, width: Int) -> [String] {
        guard width > 0 else { return [] }
        var lines: [String] = []
        for paragraph in text.split(separator: "\n", omittingEmptySubsequences: false) {
            var line = ""
            var lineWidth = 0
            for word in paragraph.split(separator: " ", omittingEmptySubsequences: true) {
                var remaining = String(word)
                while !remaining.isEmpty {
                    let wordWidth = of(remaining)
                    let needed = lineWidth == 0 ? wordWidth : lineWidth + 1 + wordWidth
                    if needed <= width {
                        line += (lineWidth == 0 ? "" : " ") + remaining
                        lineWidth = needed
                        remaining = ""
                    } else if lineWidth > 0 {
                        lines.append(line)
                        line = ""
                        lineWidth = 0
                    } else {
                        // Hard-break a word longer than the line; the piece becomes the current
                        // line so a final fragment is not followed by a spurious empty line.
                        let head = prefix(remaining, width: width)
                        let safeHead = head.isEmpty ? String(remaining.prefix(1)) : head
                        line = safeHead
                        lineWidth = max(width, of(safeHead))
                        remaining = String(remaining.dropFirst(safeHead.count))
                    }
                }
            }
            lines.append(line)
        }
        return lines
    }
}
