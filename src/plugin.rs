use anyhow::{Context, Result, bail};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

struct HeaderPatterns {
    plugin_name: Regex,
    version: Regex,
    text_domain: Regex,
}

fn header_patterns() -> &'static HeaderPatterns {
    static PATTERNS: OnceLock<HeaderPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| HeaderPatterns {
        plugin_name: Regex::new(r"(?mi)^\s*\*?\s*Plugin Name\s*:\s*(.+)$").unwrap(),
        version: Regex::new(r"(?mi)^\s*\*?\s*Version\s*:\s*(.+)$").unwrap(),
        text_domain: Regex::new(r"(?mi)^\s*\*?\s*Text Domain\s*:\s*(.+)$").unwrap(),
    })
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PluginMeta {
    pub slug: String,
    pub name: String,
    pub version: String,
    pub text_domain: String,
    pub dir: PathBuf,
    pub main_file: PathBuf,
}

pub fn parse_plugin_header(path: &Path) -> Result<Option<PluginMeta>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let patterns = header_patterns();

    let name = extract_match(&patterns.plugin_name, &content);
    if name.is_none() {
        return Ok(None);
    }

    let dir = path.parent().unwrap().to_path_buf();
    let slug = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(Some(PluginMeta {
        slug,
        name: name.unwrap_or_default(),
        version: extract_match(&patterns.version, &content).unwrap_or_default(),
        text_domain: extract_match(&patterns.text_domain, &content).unwrap_or_default(),
        dir,
        main_file: path.to_path_buf(),
    }))
}

fn extract_match(re: &Regex, content: &str) -> Option<String> {
    re.captures(content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn discover_plugin(path: &Path) -> Result<PluginMeta> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("Plugin path not found: {}", path.display()))?;

    if !path.is_dir() {
        bail!("Not a directory: {}", path.display());
    }

    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let file_path = entry.path();
        if file_path.extension().is_some_and(|e| e == "php")
            && let Some(meta) = parse_plugin_header(&file_path)? {
                return Ok(meta);
            }
    }

    bail!(
        "No WordPress plugin header found in {}.\n\
         Expected a PHP file with a 'Plugin Name:' header.\n\
         Make sure you're pointing at a plugin directory (e.g. wp-content/plugins/akismet).",
        path.display()
    )
}

pub fn discover_all(wp_path: &Path) -> Result<Vec<Result<PluginMeta>>> {
    let plugins_dir = find_plugins_dir(wp_path)?;
    let mut results = Vec::new();

    for entry in fs::read_dir(&plugins_dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            results.push(discover_plugin(&entry.path()));
        }
    }

    Ok(results)
}

fn find_plugins_dir(path: &Path) -> Result<PathBuf> {
    let candidates = [
        path.join("wp-content/plugins"),
        path.join("plugins"),
        path.to_path_buf(),
    ];

    for candidate in &candidates {
        if candidate.is_dir() {
            return Ok(candidate.clone());
        }
    }

    bail!(
        "Could not find wp-content/plugins/ in {}.\n\
         Use -C to specify the WordPress root directory.",
        path.display()
    )
}

pub fn resolve_plugin_path(slug_or_path: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    let path = Path::new(slug_or_path);

    if path.is_absolute() && path.is_dir() {
        return Ok(path.to_path_buf());
    }

    let base = match base_dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir()?,
    };

    let relative = base.join(slug_or_path);
    if relative.is_dir() {
        return Ok(relative);
    }

    let mut search = base.as_path();
    loop {
        let candidate = search.join("wp-content/plugins").join(slug_or_path);
        if candidate.is_dir() {
            return Ok(candidate);
        }
        match search.parent() {
            Some(parent) => search = parent,
            None => break,
        }
    }

    bail!(
        "Could not find plugin '{slug_or_path}'. Provide a path or use -C to set the WordPress root."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_header_full() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("my-plugin");
        fs::create_dir(&plugin_dir).unwrap();
        let php = plugin_dir.join("my-plugin.php");
        fs::write(
            &php,
            "<?php\n/*\nPlugin Name: My Plugin\nVersion: 2.1.0\nText Domain: my-plugin\n*/\n",
        )
        .unwrap();

        let meta = parse_plugin_header(&php).unwrap().unwrap();
        assert_eq!(meta.name, "My Plugin");
        assert_eq!(meta.version, "2.1.0");
        assert_eq!(meta.text_domain, "my-plugin");
        assert_eq!(meta.slug, "my-plugin");
    }

    #[test]
    fn parse_header_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("test");
        fs::create_dir(&plugin_dir).unwrap();
        let php = plugin_dir.join("test.php");
        fs::write(&php, "<?php\n/*\nPlugin Name: Test\n*/\n").unwrap();

        let meta = parse_plugin_header(&php).unwrap().unwrap();
        assert_eq!(meta.name, "Test");
        assert_eq!(meta.version, "");
    }

    #[test]
    fn parse_header_no_header() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("nope");
        fs::create_dir(&plugin_dir).unwrap();
        let php = plugin_dir.join("nope.php");
        fs::write(&php, "<?php\necho 'hello';\n").unwrap();

        assert!(parse_plugin_header(&php).unwrap().is_none());
    }

    #[test]
    fn parse_header_with_stars() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("starred");
        fs::create_dir(&plugin_dir).unwrap();
        let php = plugin_dir.join("starred.php");
        fs::write(
            &php,
            "<?php\n/**\n * Plugin Name: Starred Plugin\n * Version: 1.0\n */\n",
        )
        .unwrap();

        let meta = parse_plugin_header(&php).unwrap().unwrap();
        assert_eq!(meta.name, "Starred Plugin");
        assert_eq!(meta.version, "1.0");
    }

    #[test]
    fn discover_finds_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("akismet");
        fs::create_dir(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("akismet.php"),
            "<?php\n/*\nPlugin Name: Akismet\nVersion: 5.0\n*/\n",
        )
        .unwrap();
        fs::write(plugin_dir.join("helper.php"), "<?php\nfunction helper() {}\n").unwrap();

        let meta = discover_plugin(&plugin_dir).unwrap();
        assert_eq!(meta.slug, "akismet");
        assert_eq!(meta.version, "5.0");
    }

    #[test]
    fn discover_no_plugin_header() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("empty");
        fs::create_dir(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("index.php"), "<?php\n").unwrap();

        assert!(discover_plugin(&plugin_dir).is_err());
    }

    #[test]
    fn resolve_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_dir = dir.path().join("test-plugin");
        fs::create_dir(&plugin_dir).unwrap();

        let resolved = resolve_plugin_path(
            plugin_dir.to_str().unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(resolved, plugin_dir);
    }

    #[test]
    fn resolve_slug_with_base_dir() {
        let dir = tempfile::tempdir().unwrap();
        let wp = dir.path().join("wp-content/plugins/akismet");
        fs::create_dir_all(&wp).unwrap();

        let resolved = resolve_plugin_path("akismet", Some(dir.path())).unwrap();
        assert_eq!(resolved, wp);
    }

    #[test]
    fn resolve_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_plugin_path("nonexistent", Some(dir.path())).is_err());
    }

    #[test]
    fn find_plugins_dir_standard() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("wp-content/plugins");
        fs::create_dir_all(&plugins).unwrap();

        let found = find_plugins_dir(dir.path()).unwrap();
        assert_eq!(found, plugins);
    }

    #[test]
    fn find_plugins_dir_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("empty-site");
        fs::create_dir(&subdir).unwrap();
        // empty-site has no wp-content/plugins, no plugins/, and isn't itself a dir with subdirs
        let deep = subdir.join("nope");
        fs::create_dir(&deep).unwrap();
        // find_plugins_dir checks candidates: path/wp-content/plugins, path/plugins, path itself
        // path itself (deep) is a dir, so it returns Ok. Use a file instead.
        let leaf = subdir.join("leaf-file");
        fs::write(&leaf, "").unwrap();
        // find_plugins_dir(subdir) - subdir has no wp-content/plugins or plugins/ subdir,
        // but subdir itself IS a dir so it matches the third candidate.
        // The function returns the first candidate that is_dir, which includes path itself.
        // So it never errors for a real directory. Test with a nonexistent path.
        let nonexistent = dir.path().join("does-not-exist");
        assert!(find_plugins_dir(&nonexistent).is_err());
    }
}
