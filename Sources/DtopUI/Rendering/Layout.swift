public struct Rect: Sendable, Hashable, CustomStringConvertible {
    public var x: Int
    public var y: Int
    public var width: Int
    public var height: Int

    public init(x: Int, y: Int, width: Int, height: Int) {
        self.x = x
        self.y = y
        self.width = max(0, width)
        self.height = max(0, height)
    }

    public static let zero = Rect(x: 0, y: 0, width: 0, height: 0)

    public var maxX: Int { x + width }
    public var maxY: Int { y + height }
    public var isEmpty: Bool { width <= 0 || height <= 0 }
    public var description: String { "(\(x),\(y) \(width)x\(height))" }

    public func contains(x px: Int, y py: Int) -> Bool {
        px >= x && py >= y && px < maxX && py < maxY
    }

    public func intersection(_ other: Rect) -> Rect {
        let left = max(x, other.x)
        let top = max(y, other.y)
        let right = min(maxX, other.maxX)
        let bottom = min(maxY, other.maxY)
        guard right > left, bottom > top else { return Rect(x: left, y: top, width: 0, height: 0) }
        return Rect(x: left, y: top, width: right - left, height: bottom - top)
    }

    public func inset(by amount: Int) -> Rect {
        inset(top: amount, left: amount, bottom: amount, right: amount)
    }

    public func inset(top: Int = 0, left: Int = 0, bottom: Int = 0, right: Int = 0) -> Rect {
        Rect(x: x + left, y: y + top, width: width - left - right, height: height - top - bottom)
    }

    /// A rect of the given size centered inside this one (clamped to fit).
    public func centered(width w: Int, height h: Int) -> Rect {
        let clampedWidth = min(w, width)
        let clampedHeight = min(h, height)
        return Rect(x: x + (width - clampedWidth) / 2, y: y + (height - clampedHeight) / 2,
                    width: clampedWidth, height: clampedHeight)
    }

    public func row(_ offset: Int) -> Rect {
        Rect(x: x, y: y + offset, width: width, height: offset >= 0 && offset < height ? 1 : 0)
    }

    public enum Axis: Sendable {
        case horizontal
        case vertical
    }

    /// Splits along an axis. Fixed and percent sizes are honoured first (shrinking from the last
    /// constraint if they overflow); the remainder is shared by `fill` weights.
    public func split(_ axis: Axis, _ constraints: [LayoutConstraint], spacing: Int = 0) -> [Rect] {
        guard !constraints.isEmpty else { return [] }
        let length = axis == .horizontal ? width : height
        let sizes = LayoutConstraint.solve(constraints, length: length, spacing: spacing)
        var rects: [Rect] = []
        var offset = 0
        for size in sizes {
            switch axis {
            case .horizontal:
                rects.append(Rect(x: x + offset, y: y, width: size, height: height))
            case .vertical:
                rects.append(Rect(x: x, y: y + offset, width: width, height: size))
            }
            offset += size + spacing
        }
        return rects
    }
}

public enum LayoutConstraint: Sendable, Equatable {
    case fixed(Int)
    case percent(Int)
    case fill(Int)

    public static func solve(_ constraints: [LayoutConstraint], length: Int, spacing: Int = 0) -> [Int] {
        let available = max(0, length - max(0, spacing) * max(0, constraints.count - 1))
        var sizes = constraints.map { constraint -> Int in
            switch constraint {
            case .fixed(let value): return max(0, value)
            case .percent(let value): return max(0, available * min(100, max(0, value)) / 100)
            case .fill: return 0
            }
        }
        var overflow = sizes.reduce(0, +) - available
        var index = sizes.count - 1
        while overflow > 0, index >= 0 {
            let cut = min(overflow, sizes[index])
            sizes[index] -= cut
            overflow -= cut
            index -= 1
        }

        let remaining = max(0, available - sizes.reduce(0, +))
        let weights = constraints.map { constraint -> Int in
            if case .fill(let weight) = constraint { return max(0, weight) }
            return 0
        }
        let totalWeight = weights.reduce(0, +)
        guard totalWeight > 0, remaining > 0 else { return sizes }
        var distributed = 0
        for (offset, weight) in weights.enumerated() where weight > 0 {
            let share = remaining * weight / totalWeight
            sizes[offset] += share
            distributed += share
        }
        // Hand out the rounding remainder one column at a time, first fills first.
        var leftover = remaining - distributed
        for (offset, weight) in weights.enumerated() where weight > 0 && leftover > 0 {
            sizes[offset] += 1
            leftover -= 1
        }
        return sizes
    }
}
