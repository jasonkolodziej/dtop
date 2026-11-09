//! Compact, deterministic number formatting for narrow terminal columns (no locale).

const BINARY_UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
const COMPACT_UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];

fn scaled(value: f64, units: &[&str]) -> String {
    let mut scaled = value;
    let mut unit = 0;
    while scaled >= 1024.0 && unit < units.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    // Rounding 1023.6 up to 1024 would read oddly; carry into the next unit instead.
    if scaled.round() >= 1024.0 && unit < units.len() - 1 {
        scaled /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{}B", scaled.round() as i64);
    }
    let number = if scaled < 10.0 {
        fixed(scaled, 1)
    } else {
        (scaled.round() as i64).to_string()
    };
    format!("{number}{}", units[unit])
}

/// `1536` → `1.5KiB`; values under 10 keep one decimal, larger ones are rounded.
pub fn bytes(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "0B".into();
    }
    scaled(value, &BINARY_UNITS)
}

pub fn bytes_u64(value: u64) -> String {
    bytes(value as f64)
}

/// Narrow form for dense columns: `0`, `512B`, `1.5K`, `12M`, `3.2G`.
pub fn compact_bytes(value: f64) -> String {
    if !value.is_finite() || value < 1.0 {
        return "0".into();
    }
    scaled(value, &COMPACT_UNITS)
}

pub fn rate(bytes_per_second: f64) -> String {
    format!("{}/s", bytes(bytes_per_second))
}

pub fn percent(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "0%".into();
    }
    format!("{}%", fixed(value, decimals))
}

/// Locale-independent fixed point (half away from zero, like Swift's `rounded()`).
pub fn fixed(value: f64, decimals: usize) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let factor = 10f64.powi(decimals as i32);
    let rounded = (value.abs() * factor).round() / factor;
    let sign = if value < 0.0 && rounded > 0.0 {
        "-"
    } else {
        ""
    };
    let integer = rounded.trunc() as i64;
    if decimals == 0 {
        return format!("{sign}{integer}");
    }
    let fraction = ((rounded - integer as f64) * factor).round() as i64;
    format!("{sign}{integer}.{fraction:0width$}", width = decimals)
}

/// `3725` → `1h 2m`: the two most significant units.
pub fn duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "0s".into();
    }
    let total = seconds as i64;
    let (days, hours, minutes, secs) = (
        total / 86_400,
        (total % 86_400) / 3600,
        (total % 3600) / 60,
        total % 60,
    );
    if days > 0 {
        return if hours > 0 {
            format!("{days}d {hours}h")
        } else {
            format!("{days}d")
        };
    }
    if hours > 0 {
        return if minutes > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{hours}h")
        };
    }
    if minutes > 0 {
        return if secs > 0 {
            format!("{minutes}m {secs}s")
        } else {
            format!("{minutes}m")
        };
    }
    format!("{secs}s")
}

/// Relative age such as `5m ago`.
pub fn age(since: f64, now: f64) -> String {
    let delta = now - since;
    if delta < 1.0 {
        return "just now".into();
    }
    format!("{} ago", duration(delta))
}

/// Seconds since the Unix epoch.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(bytes(0.0), "0B");
        assert_eq!(bytes(512.0), "512B");
        assert_eq!(bytes(1536.0), "1.5KiB");
        assert_eq!(bytes(1023.6 * 1024.0), "1.0MiB");
        assert_eq!(bytes(50.0 * 1024.0 * 1024.0), "50MiB");
        assert_eq!(compact_bytes(0.5), "0");
        assert_eq!(compact_bytes(3.2 * 1024.0 * 1024.0 * 1024.0), "3.2G");
        assert_eq!(rate(2048.0), "2.0KiB/s");
    }

    #[test]
    fn formats_fixed_and_percent() {
        assert_eq!(fixed(1.25, 1), "1.3");
        assert_eq!(fixed(-0.04, 1), "0.0");
        assert_eq!(fixed(-1.5, 0), "-2");
        assert_eq!(fixed(9.999, 2), "10.00");
        assert_eq!(percent(12.345, 1), "12.3%");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(3725.0), "1h 2m");
        assert_eq!(duration(90_000.0), "1d 1h");
        assert_eq!(duration(60.0), "1m");
        assert_eq!(duration(5.0), "5s");
        assert_eq!(age(0.0, 300.0), "5m ago");
        assert_eq!(age(10.0, 5.0), "just now");
    }
}
