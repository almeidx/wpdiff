use anyhow::{Context, Result, bail};
use log::debug;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static PLUGIN_NAME: LazyLock<Regex> = LazyLock::new(|| header_field("Plugin Name"));
static VERSION: LazyLock<Regex> = LazyLock::new(|| header_field("Version"));

fn header_field(label: &str) -> Regex {
    Regex::new(&format!(r"(?mi)^\s*\*?\s*{label}\s*:\s*(.+)$")).expect("header pattern is valid")
}

#[derive(Debug)]
pub struct PluginMeta {
    /// Directory name, which is also the plugin's wordpress.org identifier.
    pub slug: String,
    pub name: String,
    /// Empty when the header has no `Version:` field.
    pub version: String,
    pub dir: PathBuf,
}

pub fn locate(slug_or_path: &str, base_dir: Option<&Path>) -> Result<PluginMeta> {
    debug!("Resolving plugin: {slug_or_path}");
    discover_plugin(&resolve_plugin_path(slug_or_path, base_dir)?)
}

fn parse_plugin_header(dir: &Path, file: &Path) -> Result<Option<PluginMeta>> {
    // Older plugins ship PHP files in legacy encodings such as ISO-8859-1.
    let bytes = fs::read(file).with_context(|| format!("Failed to read {}", file.display()))?;
    let content = String::from_utf8_lossy(&bytes);

    let Some(name) = extract_match(&PLUGIN_NAME, &content) else {
        return Ok(None);
    };
    debug!("Main file: {}", file.display());
    let slug = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(Some(PluginMeta {
        slug,
        name,
        version: extract_match(&VERSION, &content).unwrap_or_default(),
        dir: dir.to_path_buf(),
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
        let file_path = entry?.path();
        if file_path.extension().is_some_and(|e| e == "php")
            && let Some(meta) = parse_plugin_header(&path, &file_path)?
        {
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

/// Discovers every directory under the installation's plugins directory; the
/// ones that are not plugins come back as errors.
pub fn discover_all(base_dir: Option<&Path>) -> Result<Vec<Result<PluginMeta>>> {
    let plugins_dir = plugins_dir(&resolve_base(base_dir)?);
    debug!("Scanning all plugins in {}", plugins_dir.display());
    let mut results = Vec::new();

    for entry in fs::read_dir(&plugins_dir)? {
        let path = entry?.path();
        if path.is_dir() {
            results.push(discover_plugin(&path));
        }
    }

    Ok(results)
}

/// Canonical form of `-C`, or of the current directory when `-C` is absent. A
/// relative path such as `.` has no ancestors to search until it is canonical.
fn resolve_base(base_dir: Option<&Path>) -> Result<PathBuf> {
    let base = match base_dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir().context(
            "Could not determine the current directory. \
             Use -C to specify the WordPress root directory.",
        )?,
    };

    let canonical = base.canonicalize().with_context(|| {
        format!(
            "Could not open directory {}.\n\
             Use -C to specify the WordPress root directory.",
            base.display()
        )
    })?;
    if !canonical.is_dir() {
        bail!(
            "{} is not a directory.\n\
             Use -C to specify the WordPress root directory.",
            base.display()
        );
    }

    Ok(canonical)
}

/// The plugins directory that `base` (canonical) belongs to: the nearest
/// `wp-content/plugins` at or above it, else `base/plugins` (`base` is
/// `wp-content`), else `base` itself (`base` is the plugins directory).
fn plugins_dir(base: &Path) -> PathBuf {
    base.ancestors()
        .map(|dir| dir.join("wp-content/plugins"))
        .chain(std::iter::once(base.join("plugins")))
        .find(|candidate| candidate.is_dir())
        .unwrap_or_else(|| base.to_path_buf())
}

pub fn resolve_plugin_path(slug_or_path: &str, base_dir: Option<&Path>) -> Result<PathBuf> {
    let base = resolve_base(base_dir)?;

    let relative = base.join(slug_or_path);
    if relative.is_dir() {
        return Ok(relative);
    }

    let installed = plugins_dir(&base).join(slug_or_path);
    if installed.is_dir() {
        return Ok(installed);
    }

    bail!(
        "Could not find plugin '{slug_or_path}'. Provide a path or use -C to set the WordPress root."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_dir(root: &Path, slug: &str, main_file: &str, header: &str) -> PathBuf {
        let dir = root.join(slug);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(main_file), header).unwrap();
        dir
    }

    #[test]
    fn discover_reads_header_fields() {
        let root = tempfile::tempdir().unwrap();
        let dir = plugin_dir(
            root.path(),
            "my-plugin",
            "main.php",
            "<?php\n/*\nPlugin Name: My Plugin\nVersion: 2.1.0\nText Domain: my-plugin\n*/\n",
        );

        let meta = discover_plugin(&dir).unwrap();
        assert_eq!(meta.name, "My Plugin");
        assert_eq!(meta.version, "2.1.0");
        assert_eq!(meta.slug, "my-plugin");
        assert_eq!(meta.dir, dir.canonicalize().unwrap());
    }

    #[test]
    fn discover_allows_missing_version() {
        let root = tempfile::tempdir().unwrap();
        let dir = plugin_dir(
            root.path(),
            "test",
            "test.php",
            "<?php\n/*\nPlugin Name: Test\n*/\n",
        );

        let meta = discover_plugin(&dir).unwrap();
        assert_eq!(meta.name, "Test");
        assert_eq!(meta.version, "");
    }

    #[test]
    fn discover_reads_docblock_style_header() {
        let root = tempfile::tempdir().unwrap();
        let dir = plugin_dir(
            root.path(),
            "starred",
            "starred.php",
            "<?php\n/**\n * Plugin Name: Starred Plugin\n * Version: 1.0\n */\n",
        );

        let meta = discover_plugin(&dir).unwrap();
        assert_eq!(meta.name, "Starred Plugin");
        assert_eq!(meta.version, "1.0");
    }

    #[test]
    fn discover_picks_the_file_with_the_header() {
        let root = tempfile::tempdir().unwrap();
        let dir = plugin_dir(
            root.path(),
            "akismet",
            "akismet.php",
            "<?php\n/*\nPlugin Name: Akismet\nVersion: 5.0\n*/\n",
        );
        fs::write(dir.join("helper.php"), "<?php\nfunction helper() {}\n").unwrap();
        fs::write(dir.join("notes.txt"), "Plugin Name: Not PHP\n").unwrap();

        let meta = discover_plugin(&dir).unwrap();
        assert_eq!(meta.name, "Akismet");
    }

    #[test]
    fn discover_reads_php_files_that_are_not_utf8() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("legacy");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("a-helper.php"), b"<?php\n// caf\xe9\n").unwrap();
        fs::write(
            dir.join("legacy.php"),
            b"<?php\n/*\nPlugin Name: Legacy\nDescription: caf\xe9\nVersion: 1.2\n*/\n",
        )
        .unwrap();

        let meta = discover_plugin(&dir).unwrap();
        assert_eq!(meta.name, "Legacy");
        assert_eq!(meta.version, "1.2");
    }

    #[test]
    fn discover_no_plugin_header() {
        let root = tempfile::tempdir().unwrap();
        let dir = plugin_dir(root.path(), "empty", "index.php", "<?php\necho 'hello';\n");

        assert!(discover_plugin(&dir).is_err());
    }

    #[test]
    fn resolve_absolute_path() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("test-plugin");
        fs::create_dir(&dir).unwrap();

        let resolved = resolve_plugin_path(dir.to_str().unwrap(), Some(root.path())).unwrap();
        assert_eq!(resolved, dir);
    }

    #[test]
    fn resolve_slug_with_base_dir() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("wp-content/plugins/akismet");
        fs::create_dir_all(&dir).unwrap();

        let resolved = resolve_plugin_path("akismet", Some(root.path())).unwrap();
        assert_eq!(resolved, dir.canonicalize().unwrap());
    }

    #[test]
    fn resolve_slug_from_inside_the_installation() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("wp-content/plugins/akismet");
        let nested = root.path().join("wp-content/themes/mine");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let resolved = resolve_plugin_path("akismet", Some(&nested)).unwrap();
        assert_eq!(resolved, dir.canonicalize().unwrap());
    }

    #[test]
    fn resolve_not_found() {
        let root = tempfile::tempdir().unwrap();
        assert!(resolve_plugin_path("nonexistent", Some(root.path())).is_err());
    }

    #[test]
    fn plugins_dir_standard() {
        let root = tempfile::tempdir().unwrap();
        let plugins = root.path().join("wp-content/plugins");
        fs::create_dir_all(&plugins).unwrap();

        let base = resolve_base(Some(root.path())).unwrap();
        assert_eq!(plugins_dir(&base), plugins.canonicalize().unwrap());
    }

    #[test]
    fn plugins_dir_inside_wp_content() {
        let root = tempfile::tempdir().unwrap();
        let plugins = root.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        fs::create_dir_all(root.path().join("themes")).unwrap();

        let base = resolve_base(Some(root.path())).unwrap();
        assert_eq!(plugins_dir(&base), plugins.canonicalize().unwrap());
    }

    #[test]
    fn plugins_dir_accepts_the_plugins_directory_itself() {
        let root = tempfile::tempdir().unwrap();
        let base = resolve_base(Some(root.path())).unwrap();
        assert_eq!(plugins_dir(&base), root.path().canonicalize().unwrap());
    }

    #[test]
    fn resolve_base_rejects_missing_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(resolve_base(Some(&root.path().join("does-not-exist"))).is_err());
    }

    #[test]
    fn discover_all_reports_plugins_and_non_plugins() {
        let root = tempfile::tempdir().unwrap();
        let plugins = root.path().join("wp-content/plugins");
        plugin_dir(
            &plugins,
            "akismet",
            "akismet.php",
            "<?php\n/*\nPlugin Name: Akismet\nVersion: 5.0\n*/\n",
        );
        plugin_dir(
            &plugins,
            "hello-dolly",
            "hello.php",
            "<?php\n/*\nPlugin Name: Hello Dolly\nVersion: 1.7\n*/\n",
        );
        plugin_dir(&plugins, "not-a-plugin", "readme.txt", "not a plugin");
        fs::write(plugins.join("index.php"), "<?php // Silence is golden.\n").unwrap();

        let results = discover_all(Some(root.path())).unwrap();
        let mut slugs: Vec<&str> = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|m| m.slug.as_str())
            .collect();
        slugs.sort_unstable();

        assert_eq!(slugs, ["akismet", "hello-dolly"]);
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
    }

    #[test]
    fn discover_all_from_themes_directory_lists_plugins() {
        let root = tempfile::tempdir().unwrap();
        plugin_dir(
            &root.path().join("wp-content/plugins"),
            "akismet",
            "akismet.php",
            "<?php\n/*\nPlugin Name: Akismet\nVersion: 5.0\n*/\n",
        );
        let themes = root.path().join("wp-content/themes");
        fs::create_dir_all(themes.join("mine")).unwrap();

        let results = discover_all(Some(&themes)).unwrap();
        let slugs: Vec<&str> = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .map(|m| m.slug.as_str())
            .collect();

        assert_eq!(slugs, ["akismet"]);
        assert_eq!(results.len(), 1);
    }
}
