//! Theme names and files: resolving `--theme` and saving themes from the theme menu.

use dtop_core::platform::{join_path, Platform};
use dtop_ui::theme::{parse_theme_file, serialize_theme};
use dtop_ui::Theme;

use crate::config::ConfigurationError;

/// Resolves a theme name or file path, looking in the user's themes directory.
#[derive(Debug, Clone)]
pub struct ThemeLoader {
    pub themes_directory: Option<String>,
    pub platform: Platform,
}

impl ThemeLoader {
    /// Built-in names plus `*.theme` files in the themes directory. File names keep their case,
    /// so each listed name loads its file on case-sensitive file systems; files that a built-in
    /// name (matched case-insensitively, as `load` does) would shadow are left out.
    pub fn available_names(&self, list_directory: &dyn Fn(&str) -> Vec<String>) -> Vec<String> {
        let mut names: Vec<String> = Theme::built_in().into_iter().map(|t| t.name).collect();
        if let Some(directory) = &self.themes_directory {
            let mut custom: Vec<String> = list_directory(directory)
                .into_iter()
                .filter_map(|f| f.strip_suffix(".theme").map(str::to_string))
                .filter(|n| !n.trim().is_empty() && Theme::named(n).is_none())
                .collect();
            custom.sort();
            custom.dedup();
            names.extend(custom);
        }
        names
    }

    pub fn load(
        &self,
        name_or_path: &str,
        read_file: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Theme, ConfigurationError> {
        if let Some(theme) = Theme::named(name_or_path) {
            return Ok(theme);
        }
        let looks_like_path = name_or_path.contains('/')
            || name_or_path.contains('\\')
            || name_or_path.ends_with(".theme");
        let mut candidates = Vec::new();
        if looks_like_path {
            candidates.push(name_or_path.to_string());
        }
        if let Some(directory) = &self.themes_directory {
            let file = if name_or_path.ends_with(".theme") {
                name_or_path.to_string()
            } else {
                format!("{name_or_path}.theme")
            };
            candidates.push(join_path(
                &[directory.as_str(), file.as_str()],
                self.platform,
            ));
        }
        for candidate in candidates {
            let Some(text) = read_file(&candidate) else {
                continue;
            };
            let base = base_name(&candidate, self.platform);
            return parse_theme_file(&text, &base, &Theme::named)
                .map_err(|e| ConfigurationError(format!("{candidate}: {e}")));
        }
        let built_ins: Vec<String> = Theme::built_in().into_iter().map(|t| t.name).collect();
        let location = self
            .themes_directory
            .as_ref()
            .map(|d| format!(" or a file in {d}"))
            .unwrap_or_default();
        Err(ConfigurationError(format!(
            "unknown theme '{name_or_path}' — use one of {}{location}",
            built_ins.join(", ")
        )))
    }
}

pub fn base_name(path: &str, platform: Platform) -> String {
    let separators: &[char] = if platform == Platform::Windows {
        &['/', '\\']
    } else {
        &['/']
    };
    let file = path.rsplit(separators).next().unwrap_or(path);
    file.strip_suffix(".theme").unwrap_or(file).to_string()
}

/// File operations the store needs; injected so saving is testable without a disk.
pub trait ThemeFileSystem {
    fn read(&self, path: &str) -> Option<String>;
    fn write(&self, path: &str, contents: &str) -> std::io::Result<()>;
    fn create_directory(&self, path: &str) -> std::io::Result<()>;
}

pub struct LocalFileSystem;

impl ThemeFileSystem for LocalFileSystem {
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    /// Atomic: write a sibling temp file, then rename over the target.
    fn write(&self, path: &str, contents: &str) -> std::io::Result<()> {
        let temporary = format!("{path}.tmp-{}", std::process::id());
        std::fs::write(&temporary, contents)?;
        std::fs::rename(&temporary, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })
    }

    fn create_directory(&self, path: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSaveResult {
    /// The theme file written, or None when an unmodified built-in was selected.
    pub theme_file: Option<String>,
    pub config_file: String,
}

/// Persists themes from the theme menu: `themes/<name>.theme` plus `theme = <name>` in
/// `dtop.conf`, keeping every other line of the user's config.
#[derive(Debug, Clone)]
pub struct ThemeStore {
    pub config_directory: Option<String>,
    pub config_file: Option<String>,
    pub platform: Platform,
}

impl ThemeStore {
    pub fn new(
        config_directory: Option<String>,
        config_file: Option<String>,
        platform: Platform,
    ) -> Self {
        let config_file = config_file.or_else(|| {
            config_directory
                .as_ref()
                .map(|d| join_path(&[d.as_str(), "dtop.conf"], platform))
        });
        ThemeStore {
            config_directory,
            config_file,
            platform,
        }
    }

    pub fn themes_directory(&self) -> Option<String> {
        self.config_directory
            .as_ref()
            .map(|d| join_path(&[d.as_str(), "themes"], self.platform))
    }

    pub fn save(
        &self,
        theme: &Theme,
        name: &str,
        fs: &dyn ThemeFileSystem,
    ) -> Result<ThemeSaveResult, ConfigurationError> {
        let (Some(config_directory), Some(config_file), Some(themes_directory)) = (
            &self.config_directory,
            &self.config_file,
            self.themes_directory(),
        ) else {
            return Err(ConfigurationError(
                "cannot save themes: no config directory — set HOME or XDG_CONFIG_HOME (APPDATA on Windows)".into(),
            ));
        };
        let unmodified_built_in = Theme::built_in()
            .iter()
            .any(|t| t == theme && t.name == name);
        let failed = |e: std::io::Error| {
            ConfigurationError(format!(
                "could not save theme '{name}': {e} — check permissions on {config_directory}"
            ))
        };
        let mut theme_file = None;
        if unmodified_built_in {
            fs.create_directory(config_directory).map_err(failed)?;
        } else {
            fs.create_directory(&themes_directory).map_err(failed)?;
            let path = join_path(
                &[themes_directory.as_str(), &format!("{name}.theme")],
                self.platform,
            );
            fs.write(&path, &serialize_theme(theme)).map_err(failed)?;
            theme_file = Some(path);
        }
        let existing = fs.read(config_file).unwrap_or_default();
        fs.write(config_file, &setting_value(&existing, "theme", name))
            .map_err(failed)?;
        Ok(ThemeSaveResult {
            theme_file,
            config_file: config_file.clone(),
        })
    }
}

/// Sets `key = value` in config text: replaces the first global match, drops later duplicates
/// (which would override it), keeps comments and order. When absent it is added before the
/// first `[host …]` section, because a key appended after one would belong to that host.
pub fn setting_value(text: &str, key: &str, value: &str) -> String {
    let normalized = key.to_lowercase().replace('-', "_");
    let mut lines: Vec<&str> = if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    };
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let mut replaced = false;
    let mut in_section = false;
    let mut first_section: Option<usize> = None;
    let mut output: Vec<String> = Vec::new();
    for l in lines {
        let trimmed = l.trim();
        if trimmed.starts_with('[') {
            in_section = true;
            first_section.get_or_insert(output.len());
        }
        let is_key = !in_section
            && !trimmed.starts_with('#')
            && !trimmed.starts_with(';')
            && trimmed
                .split_once('=')
                .is_some_and(|(k, _)| k.trim().to_lowercase().replace('-', "_") == normalized);
        if is_key {
            if !replaced {
                output.push(format!("{key} = {value}"));
                replaced = true;
            }
            continue;
        }
        output.push(l.to_string());
    }
    if !replaced {
        let entry = format!("{key} = {value}");
        match first_section {
            Some(index) => output.insert(index, entry),
            None => output.push(entry),
        }
    }
    output.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    #[test]
    fn replaces_first_and_drops_duplicates() {
        let text = "# hi\ntheme = old\ninterval = 2\ntheme=other\n";
        assert_eq!(
            setting_value(text, "theme", "new"),
            "# hi\ntheme = new\ninterval = 2\n"
        );
        assert_eq!(setting_value("", "theme", "x"), "theme = x\n");
        assert_eq!(
            setting_value("interval = 1\n[host a]\nendpoint = tcp://a\n", "theme", "x"),
            "interval = 1\ntheme = x\n[host a]\nendpoint = tcp://a\n"
        );
    }

    struct MemoryFs(RefCell<BTreeMap<String, String>>);
    impl ThemeFileSystem for MemoryFs {
        fn read(&self, path: &str) -> Option<String> {
            self.0.borrow().get(path).cloned()
        }
        fn write(&self, path: &str, contents: &str) -> std::io::Result<()> {
            self.0.borrow_mut().insert(path.into(), contents.into());
            Ok(())
        }
        fn create_directory(&self, _: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn saves_custom_themes_and_selects_built_ins() {
        let fs = MemoryFs(RefCell::new(BTreeMap::new()));
        let store = ThemeStore::new(Some("/c".into()), None, Platform::Linux);
        let mut theme = Theme::harbor();
        theme.name = "mine".into();
        let result = store.save(&theme, "mine", &fs).unwrap();
        assert_eq!(result.theme_file.as_deref(), Some("/c/themes/mine.theme"));
        assert_eq!(fs.read("/c/dtop.conf").as_deref(), Some("theme = mine\n"));
        let built = store.save(&Theme::harbor(), "harbor", &fs).unwrap();
        assert_eq!(built.theme_file, None);
        let loader = ThemeLoader {
            themes_directory: Some("/c/themes".into()),
            platform: Platform::Linux,
        };
        let loaded = loader.load("mine", &|p| fs.read(p)).unwrap();
        assert_eq!(loaded.name, "mine");
        assert!(loader.load("nope", &|_| None).is_err());
        assert_eq!(base_name(r"C:\t\x.theme", Platform::Windows), "x");
    }

    #[test]
    fn listed_custom_themes_keep_their_file_name_case() {
        let fs = MemoryFs(RefCell::new(BTreeMap::new()));
        let mut theme = Theme::harbor();
        theme.name = "Ocean".into();
        fs.write("/t/Ocean.theme", &serialize_theme(&theme))
            .unwrap();
        let loader = ThemeLoader {
            themes_directory: Some("/t".into()),
            platform: Platform::Linux,
        };
        let listing = |_: &str| {
            vec![
                "Ocean.theme".to_string(),
                "Harbor.theme".to_string(),
                "Default.theme".to_string(),
                "notes.txt".to_string(),
            ]
        };
        let names = loader.available_names(&listing);
        assert!(names.contains(&"Ocean".to_string()), "{names:?}");
        // Shadowed by the built-ins `harbor` and `default`, which `load` resolves first.
        assert!(!names.iter().any(|n| n == "Harbor" || n == "Default"));
        assert_eq!(names.last().map(String::as_str), Some("Ocean"));
        assert!(loader.load("Ocean", &|p| fs.read(p)).is_ok());
    }
}
