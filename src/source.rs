use anyhow::{Context, Result, bail};
use log::{debug, info};
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

use crate::plugin;
use crate::progress;

const DOWNLOADS: &str = "https://downloads.wordpress.org/plugin";
const PLUGIN_INFO_API: &str = "https://api.wordpress.org/plugins/info/1.2/";

pub struct WpOrg {
    client: reqwest::blocking::Client,
}

/// An upstream plugin package extracted to a temporary directory that is
/// removed when this value is dropped.
pub struct Upstream {
    path: PathBuf,
    _temp_dir: TempDir,
}

impl Upstream {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub struct PluginInfo {
    pub name: String,
    pub latest_version: String,
    /// Released versions, ordered by their numeric components.
    pub versions: Vec<String>,
}

impl WpOrg {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_mins(2))
            .build()
            .context("Failed to set up the HTTP client")?;
        Ok(Self { client })
    }

    /// Downloads the package of exactly `version`. An empty `version` (the
    /// plugin header has none) asks for the current release.
    pub fn download(&self, slug: &str, version: &str) -> Result<Upstream> {
        self.find_package(slug, version)
            .context("Failed to fetch from wordpress.org")
    }

    fn find_package(&self, slug: &str, version: &str) -> Result<Upstream> {
        if !version.is_empty()
            && let Some(package) =
                self.download_zip(&format!("{DOWNLOADS}/{slug}.{version}.zip"))?
        {
            return Ok(package);
        }

        // wordpress.org only builds versioned zips for tagged releases;
        // plugins served from trunk have just the unversioned one, which is
        // always the current release.
        let url = format!("{DOWNLOADS}/{slug}.zip");
        let current = self.download_zip(&url)?.with_context(|| {
            format!(
                "Plugin zip not found at {url}. The plugin may not exist on wordpress.org, \
                 or this version may not be available."
            )
        })?;
        if version.is_empty() {
            Ok(current)
        } else {
            require_version(current, slug, version)
        }
    }

    /// Returns `None` when wordpress.org has no package at `url`.
    fn download_zip(&self, url: &str) -> Result<Option<Upstream>> {
        info!("Downloading {url}...");

        let response = self.client.get(url).send().context(
            "Failed to connect to downloads.wordpress.org. Check your internet connection.",
        )?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!(
                "HTTP {} from wordpress.org for {}. Try again later.",
                response.status(),
                url
            );
        }

        #[allow(clippy::literal_string_with_formatting_args)]
        let pb = match response.content_length() {
            Some(total_size) if total_size > 0 => progress::bar(
                total_size,
                "  {spinner:.green} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({bytes_per_sec})",
            ),
            _ => progress::spinner("  {spinner:.green} {bytes} downloaded ({bytes_per_sec})"),
        };

        let mut bytes = Vec::new();
        pb.wrap_read(response)
            .read_to_end(&mut bytes)
            .context("Failed to read response")?;
        pb.finish_and_clear();

        debug!("Downloaded {} bytes", bytes.len());

        extract_plugin_zip(bytes)
            .map(Some)
            .with_context(|| format!("Failed to extract {url}"))
    }

    pub fn plugin_info(&self, slug: &str) -> Result<PluginInfo> {
        let url =
            format!("{PLUGIN_INFO_API}?action=plugin_information&slug={slug}&fields=versions");

        debug!("Fetching plugin info from {url}");

        let spinner = progress::spinner("  {spinner:.green} Fetching plugin info...");
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(30))
            .send();
        spinner.finish_and_clear();
        let response = response
            .context("Failed to connect to api.wordpress.org. Check your internet connection.")?;

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

        parse_plugin_info(slug, &body)
    }
}

/// Extracts a wordpress.org plugin zip, whose entries all live under a single
/// top-level `<slug>/` directory, without that directory.
fn extract_plugin_zip(bytes: Vec<u8>) -> Result<Upstream> {
    let temp_dir = TempDir::new().context("Failed to create temp directory")?;
    let extracted = temp_dir.path().join("extracted");

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    let entry_count = archive.len();
    debug!("Zip contains {entry_count} entries");

    let pb = progress::bar(
        entry_count as u64,
        "  {spinner:.green} Extracting [{bar:30.cyan/dim}] {pos}/{len}",
    );

    for i in 0..entry_count {
        let mut file = archive.by_index(i)?;
        let name = file.name()?.into_owned();

        let rel_path = strip_top_dir(&name);
        if rel_path.is_empty() {
            continue;
        }
        // Entry names are archive-controlled: `..` or a rooted path would be
        // written outside the extraction directory.
        let stays_inside = Path::new(rel_path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
        if !stays_inside {
            bail!("Refusing to extract `{name}`: the entry points outside the package directory");
        }
        let out_path = extracted.join(rel_path);

        if name.ends_with('/') {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = fs::File::create(&out_path)?;
            io::copy(&mut file, &mut out_file)?;
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    if !extracted.is_dir() {
        bail!("The archive contains no plugin files");
    }

    Ok(Upstream {
        path: extracted,
        _temp_dir: temp_dir,
    })
}

fn require_version(package: Upstream, slug: &str, version: &str) -> Result<Upstream> {
    let found = plugin::discover_plugin(package.path())
        .context("Failed to read the version of the downloaded package")?
        .version;
    if found != version {
        bail!(
            "wordpress.org has no package for {slug} v{version}; the only one it serves is the \
             current release, v{found}. Run `wpdiff versions {slug}` to list available versions."
        );
    }
    Ok(package)
}

fn strip_top_dir(zip_path: &str) -> &str {
    zip_path.split_once('/').map_or("", |(_, rest)| rest)
}

fn parse_plugin_info(slug: &str, body: &serde_json::Value) -> Result<PluginInfo> {
    if body.get("error").is_some() {
        bail!(
            "Plugin '{slug}' not found on wordpress.org. \
             Check the slug matches the plugin's directory name."
        );
    }

    let name = body["name"].as_str().unwrap_or(slug).to_string();
    let latest_version = body["version"]
        .as_str()
        .with_context(|| format!("wordpress.org API reported no current version for '{slug}'"))?
        .to_string();

    // A plugin without tagged releases has `"versions": []` instead of an object.
    let mut versions: Vec<String> = body["versions"]
        .as_object()
        .map(|tags| tags.keys().filter(|tag| *tag != "trunk").cloned().collect())
        .unwrap_or_default();
    versions.sort_by_cached_key(|v| version_sort_key(v));

    Ok(PluginInfo {
        name,
        latest_version,
        versions,
    })
}

fn version_sort_key(version: &str) -> Vec<u64> {
    version
        .split(['.', '-'])
        .filter_map(|part| part.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, options).unwrap();
            } else {
                zip.start_file(*name, options).unwrap();
                zip.write_all(content.as_bytes()).unwrap();
            }
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn extract_drops_the_top_level_directory() {
        let upstream = extract_plugin_zip(zip_of(&[
            ("akismet/", ""),
            ("akismet/akismet.php", "<?php\n"),
            ("akismet/views/", ""),
            ("akismet/views/a.php", "view\n"),
            ("akismet/_inc/deep/b.js", "js\n"),
        ]))
        .unwrap();

        let read = |rel: &str| fs::read_to_string(upstream.path().join(rel)).unwrap();
        assert_eq!(read("akismet.php"), "<?php\n");
        assert_eq!(read("views/a.php"), "view\n");
        assert_eq!(read("_inc/deep/b.js"), "js\n");
    }

    #[test]
    fn extract_rejects_archive_without_plugin_files() {
        assert!(extract_plugin_zip(zip_of(&[("stray.txt", "x")])).is_err());
        assert!(extract_plugin_zip(b"not a zip".to_vec()).is_err());
    }

    #[test]
    fn extract_refuses_entries_that_escape_the_package_directory() {
        for name in [
            "akismet/../../evil.php",
            "akismet/inc/../../../evil.php",
            "akismet//tmp/evil.php",
        ] {
            let archive = zip_of(&[("akismet/akismet.php", "<?php\n"), (name, "evil\n")]);
            let Err(err) = extract_plugin_zip(archive) else {
                panic!("{name} was extracted");
            };
            assert!(err.to_string().contains(name), "{err}");
        }
    }

    #[test]
    fn current_release_only_stands_in_for_its_own_version() {
        let current = || {
            extract_plugin_zip(zip_of(&[(
                "hello/hello.php",
                "<?php\n/*\nPlugin Name: Hello\nVersion: 1.7.2\n*/\n",
            )]))
            .unwrap()
        };

        assert!(require_version(current(), "hello", "1.7.2").is_ok());
        let Err(err) = require_version(current(), "hello", "9.9.9") else {
            panic!("the current release was accepted as v9.9.9");
        };
        assert!(err.to_string().contains("no package for hello v9.9.9"));
        assert!(err.to_string().contains("v1.7.2"));
    }

    #[test]
    fn extracted_files_are_removed_on_drop() {
        let upstream = extract_plugin_zip(zip_of(&[("p/p.php", "<?php\n")])).unwrap();
        let path = upstream.path().to_path_buf();
        assert!(path.join("p.php").is_file());
        drop(upstream);
        assert!(!path.exists());
    }

    #[test]
    fn plugin_info_sorts_versions_numerically_and_drops_trunk() {
        let info = parse_plugin_info(
            "akismet",
            &json!({
                "name": "Akismet",
                "version": "2.0.0",
                "versions": {
                    "1.10.0": "u", "1.9.0": "u", "2.0.0": "u", "1.0.0": "u", "trunk": "u",
                },
            }),
        )
        .unwrap();

        assert_eq!(info.name, "Akismet");
        assert_eq!(info.latest_version, "2.0.0");
        assert_eq!(info.versions, ["1.0.0", "1.9.0", "1.10.0", "2.0.0"]);
    }

    #[test]
    fn plugin_info_tolerates_missing_name_and_empty_versions() {
        let info =
            parse_plugin_info("my-plugin", &json!({ "version": "1.0", "versions": [] })).unwrap();
        assert_eq!(info.name, "my-plugin");
        assert_eq!(info.versions, Vec::<String>::new());
    }

    #[test]
    fn plugin_info_reports_unknown_plugin() {
        let err = parse_plugin_info("nope", &json!({ "error": "Plugin not found." })).unwrap_err();
        assert!(err.to_string().contains("'nope' not found"));
    }

    #[test]
    fn plugin_info_requires_a_current_version() {
        assert!(parse_plugin_info("x", &json!({ "name": "X" })).is_err());
    }
}
