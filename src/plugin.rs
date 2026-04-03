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
        if file_path.extension().is_some_and(|e| e == "php") {
            if let Some(meta) = parse_plugin_header(&file_path)? {
                return Ok(meta);
            }
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
        "Could not find plugin '{}'. Provide a path or use -C to set the WordPress root.",
        slug_or_path
    )
}
