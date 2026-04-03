use anyhow::{Context, Result, bail};
use log::{debug, info};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::PathBuf;
use tempfile::TempDir;

use crate::progress;

use crate::plugin::PluginMeta;

pub trait Source: Send + Sync {
    fn name(&self) -> &str;
    fn can_handle(&self, plugin: &PluginMeta) -> bool;
    fn fetch(&self, client: &reqwest::blocking::Client, plugin: &PluginMeta, version: Option<&str>) -> Result<FetchResult>;
}

pub struct FetchResult {
    pub path: PathBuf,
    pub _temp_dir: TempDir,
}

pub struct Registry {
    client: reqwest::blocking::Client,
    sources: Vec<Box<dyn Source>>,
}

impl Registry {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("failed to build HTTP client");
        let mut reg = Self {
            client,
            sources: Vec::new(),
        };
        reg.sources.push(Box::new(WpOrgSource));
        reg
    }

    pub fn fetch(&self, plugin: &PluginMeta, version: Option<&str>) -> Result<FetchResult> {
        for source in &self.sources {
            if source.can_handle(plugin) {
                return source
                    .fetch(&self.client, plugin, version)
                    .with_context(|| format!("Failed to fetch from {}", source.name()));
            }
        }
        bail!(
            "No source adapter found for plugin '{}'. It may not be available on wordpress.org.",
            plugin.slug
        )
    }
}

struct WpOrgSource;

impl Source for WpOrgSource {
    fn name(&self) -> &str {
        "wordpress.org"
    }

    fn can_handle(&self, _plugin: &PluginMeta) -> bool {
        true
    }

    fn fetch(&self, client: &reqwest::blocking::Client, plugin: &PluginMeta, version: Option<&str>) -> Result<FetchResult> {
        let ver = version.unwrap_or(&plugin.version);

        let urls = if ver.is_empty() {
            vec![format!(
                "https://downloads.wordpress.org/plugin/{}.zip",
                plugin.slug
            )]
        } else {
            vec![
                format!(
                    "https://downloads.wordpress.org/plugin/{}.{}.zip",
                    plugin.slug, ver
                ),
                format!(
                    "https://downloads.wordpress.org/plugin/{}.zip",
                    plugin.slug
                ),
            ]
        };

        let mut last_err = None;
        for url in &urls {
            match download_and_extract(client, url, &plugin.slug) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("No download URLs available")))
    }
}

fn download_and_extract(
    client: &reqwest::blocking::Client,
    url: &str,
    slug: &str,
) -> Result<FetchResult> {
    info!("Downloading {}...", url);

    let response = client.get(url).send().with_context(|| {
        format!(
            "Failed to connect to downloads.wordpress.org. Check your internet connection."
        )
    })?;

    if response.status().as_u16() == 404 {
        bail!(
            "Plugin zip not found at {}. The plugin may not exist on wordpress.org, \
             or this version may not be available.",
            url
        );
    }
    if !response.status().is_success() {
        bail!(
            "HTTP {} from wordpress.org for {}. Try again later.",
            response.status(),
            url
        );
    }

    let total_size = response.content_length().unwrap_or(0);
    let pb = if total_size > 0 {
        progress::bar(total_size, "  {spinner:.green} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec})")
    } else {
        progress::spinner("  {spinner:.green} {bytes} downloaded ({bytes_per_sec})")
    };

    let mut bytes = Vec::new();
    let mut reader = response;
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).context("Failed to read response")?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        pb.inc(n as u64);
    }
    pb.finish_and_clear();

    debug!("Downloaded {} bytes", bytes.len());

    let temp_dir = TempDir::new().context("Failed to create temp directory")?;

    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let entry_count = archive.len();
    debug!("Zip contains {} entries", entry_count);

    let extract_pb = progress::bar(
        entry_count as u64,
        "  {spinner:.green} Extracting [{bar:30.cyan/dim}] {pos}/{len}",
    );

    for i in 0..entry_count {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        let rel_path = strip_top_dir(&name);
        if rel_path.is_empty() {
            continue;
        }

        let out_path = temp_dir.path().join("extracted").join(&rel_path);

        if name.ends_with('/') {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = fs::File::create(&out_path)?;
            io::copy(&mut file, &mut out_file)?;
        }
        extract_pb.inc(1);
    }
    extract_pb.finish_and_clear();

    let extracted = temp_dir.path().join("extracted");
    if !extracted.is_dir() {
        bail!("Extraction produced no files for {}", slug);
    }

    Ok(FetchResult {
        path: extracted,
        _temp_dir: temp_dir,
    })
}

pub(crate) fn strip_top_dir(zip_path: &str) -> String {
    match zip_path.find('/') {
        Some(idx) => zip_path[idx + 1..].to_string(),
        None => String::new(),
    }
}

#[derive(Debug)]
pub struct PluginInfo {
    pub name: String,
    pub latest_version: String,
    pub versions: Vec<String>,
}

pub fn fetch_plugin_versions(slug: &str) -> Result<PluginInfo> {
    let url = format!(
        "https://api.wordpress.org/plugins/info/1.2/?action=plugin_information&slug={}&fields=versions",
        slug
    );

    debug!("Fetching plugin info from {}", url);

    let spinner = progress::spinner("  {spinner:.green} Fetching plugin info...");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client.get(&url).send().with_context(|| {
        format!("Failed to connect to api.wordpress.org. Check your internet connection.")
    })?;

    spinner.finish_and_clear();

    if !response.status().is_success() {
        bail!(
            "wordpress.org API returned HTTP {} for '{}'. Try again later.",
            response.status(),
            slug
        );
    }

    let body: serde_json::Value = response
        .json()
        .context("Failed to parse response from wordpress.org API")?;

    if body.get("error").is_some() {
        bail!(
            "Plugin '{}' not found on wordpress.org. \
             Check the slug matches the plugin's directory name.",
            slug
        );
    }

    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(slug)
        .to_string();

    let latest_version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let versions: Vec<String> = body
        .get("versions")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.keys()
                .filter(|k| *k != "trunk")
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let mut keyed: Vec<(Vec<u64>, String)> = versions
        .into_iter()
        .map(|v| {
            let key: Vec<u64> = v
                .split(|c: char| c == '.' || c == '-')
                .filter_map(|part| part.parse().ok())
                .collect();
            (key, v)
        })
        .collect();
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));
    let versions = keyed.into_iter().map(|(_, v)| v).collect();

    Ok(PluginInfo {
        name,
        latest_version,
        versions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_top_dir_normal() {
        assert_eq!(strip_top_dir("akismet/akismet.php"), "akismet.php");
    }

    #[test]
    fn strip_top_dir_nested() {
        assert_eq!(
            strip_top_dir("akismet/languages/en.po"),
            "languages/en.po"
        );
    }

    #[test]
    fn strip_top_dir_directory_entry() {
        assert_eq!(strip_top_dir("akismet/"), "");
    }

    #[test]
    fn strip_top_dir_no_slash() {
        assert_eq!(strip_top_dir("file.txt"), "");
    }

    #[test]
    fn strip_top_dir_empty() {
        assert_eq!(strip_top_dir(""), "");
    }
}
