//! The operating system family dtop runs on, as a value so platform-dependent decisions (default
//! endpoints, path joining) can be unit tested on every OS.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    MacOS,
    Linux,
    Windows,
    Other,
}

impl Platform {
    pub const ALL: [Platform; 4] = [
        Platform::MacOS,
        Platform::Linux,
        Platform::Windows,
        Platform::Other,
    ];

    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOS
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Other
        }
    }

    pub fn path_separator(self) -> char {
        if self == Platform::Windows {
            '\\'
        } else {
            '/'
        }
    }
}

/// Joins path components with the platform separator without touching the file system.
pub fn join_path<S: AsRef<str>>(components: &[S], platform: Platform) -> String {
    let separator = platform.path_separator();
    let is_sep = |c: char| c == separator || (platform == Platform::Windows && c == '/');
    let mut result = String::new();
    for component in components
        .iter()
        .map(AsRef::as_ref)
        .filter(|c| !c.is_empty())
    {
        if result.is_empty() {
            result = component.to_string();
            continue;
        }
        if result.ends_with(is_sep) {
            result.pop();
        }
        result.push(separator);
        result.push_str(component.trim_start_matches(is_sep));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_without_doubling_separators() {
        assert_eq!(join_path(&["/a/", "/b", "c"], Platform::Linux), "/a/b/c");
        assert_eq!(join_path(&["C:\\x", "y"], Platform::Windows), "C:\\x\\y");
        assert_eq!(join_path(&["", "a"], Platform::MacOS), "a");
    }
}
