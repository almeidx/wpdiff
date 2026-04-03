use crate::progress;
use anyhow::Result;
use log::{debug, trace};
use serde::Serialize;
use similar::TextDiff;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize)]
pub struct DiffResult {
    pub plugin_slug: String,
    pub plugin_version: String,
    pub files: Vec<FileDiff>,
    pub skipped_dirs: SkippedDirs,
    pub summary: DiffSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedDirs {
    pub local: Vec<String>,
    pub upstream: Vec<String>,
}

impl DiffResult {
    fn with_files(&self, files: Vec<FileDiff>) -> Self {
        let mut by_category = BTreeMap::new();
        let mut added = 0;
        let mut removed = 0;
        let mut modified = 0;

        for f in &files {
            CategorySummary::tally(&mut by_category, f.category, f.status);
            match f.status {
                FileStatus::Added => added += 1,
                FileStatus::Removed => removed += 1,
                FileStatus::Modified => modified += 1,
            }
        }

        Self {
            plugin_slug: self.plugin_slug.clone(),
            plugin_version: self.plugin_version.clone(),
            files,
            skipped_dirs: self.skipped_dirs.clone(),
            summary: DiffSummary {
                added,
                removed,
                modified,
                unchanged: self.summary.unchanged,
                by_category,
            },
        }
    }

    pub fn apply(
        &self,
        categories: &HashSet<FileCategory>,
        exclude_patterns: &[String],
    ) -> Self {
        let expanded: Vec<String> = exclude_patterns
            .iter()
            .flat_map(|p| {
                if p.contains('*') || p.contains('?') || p.contains('[') {
                    vec![p.clone()]
                } else {
                    let trimmed = p.trim_end_matches('/');
                    vec![
                        trimmed.to_string(),
                        format!("{trimmed}/**"),
                        format!("**/{trimmed}"),
                        format!("**/{trimmed}/**"),
                    ]
                }
            })
            .collect();

        let files = self
            .files
            .iter()
            .filter(|f| {
                categories.contains(&f.category)
                    && (expanded.is_empty()
                        || !expanded
                            .iter()
                            .any(|p| glob_match::glob_match(p, &f.path)))
            })
            .cloned()
            .collect();
        self.with_files(files)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffSummary {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub unchanged: usize,
    pub by_category: BTreeMap<FileCategory, CategorySummary>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CategorySummary {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
}

impl CategorySummary {
    fn tally(map: &mut BTreeMap<FileCategory, Self>, category: FileCategory, status: FileStatus) {
        let entry = map.entry(category).or_default();
        match status {
            FileStatus::Added => entry.added += 1,
            FileStatus::Removed => entry.removed += 1,
            FileStatus::Modified => entry.modified += 1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub status: FileStatus,
    pub category: FileCategory,
    pub insertions: usize,
    pub deletions: usize,
    pub diff_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileCategory {
    Source,
    Artifact,
    Asset,
    Metadata,
}

impl std::fmt::Display for FileCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source => write!(f, "source"),
            Self::Artifact => write!(f, "artifact"),
            Self::Asset => write!(f, "asset"),
            Self::Metadata => write!(f, "metadata"),
        }
    }
}

pub fn diff_directories(
    local_dir: &Path,
    upstream_dir: &Path,
    slug: &str,
    version: &str,
    strict_whitespace: bool,
) -> Result<DiffResult> {
    let local_result = collect_files(local_dir)?;
    let upstream_result = collect_files(upstream_dir)?;

    let local_files = local_result.files;
    let upstream_files = upstream_result.files;

    let skipped_dirs = SkippedDirs {
        local: local_result.skipped,
        upstream: upstream_result.skipped,
    };

    debug!(
        "Comparing {} local files against {} upstream files",
        local_files.len(),
        upstream_files.len()
    );

    if !skipped_dirs.local.is_empty() {
        debug!("Skipped local dirs: {}", skipped_dirs.local.join(", "));
    }
    if !skipped_dirs.upstream.is_empty() {
        debug!("Skipped upstream dirs: {}", skipped_dirs.upstream.join(", "));
    }

    let mut files = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut modified = 0usize;
    let mut unchanged = 0usize;
    let mut by_category: BTreeMap<FileCategory, CategorySummary> = BTreeMap::new();

    let total_files = upstream_files.len() + local_files.len();
    let pb = progress::bar(
        total_files as u64,
        "  {spinner:.green} Comparing [{bar:30.cyan/dim}] {pos}/{len} files",
    );

    for rel_path in &upstream_files {
        if !local_files.contains(rel_path) {
            trace!("Removed from local: {rel_path}");
            let category = categorize_file(rel_path);
            let upstream_raw = read_file_lossy(&upstream_dir.join(rel_path));
            let upstream_content = prepare_content(&upstream_raw, strict_whitespace);
            let diff_text = make_unified_diff(
                &upstream_content,
                "",
                &format!("a/{rel_path}"),
                &format!("b/{rel_path}"),
            );

            let (ins, del) = count_changes(&diff_text);
            files.push(FileDiff {
                path: rel_path.clone(),
                status: FileStatus::Removed,
                category,
                insertions: ins,
                deletions: del,
                diff_text,
            });
            removed += 1;
            CategorySummary::tally(&mut by_category, category, FileStatus::Removed);
        }
        pb.inc(1);
    }

    for rel_path in &local_files {
        let category = categorize_file(rel_path);

        if upstream_files.contains(rel_path) {
            let local_path = local_dir.join(rel_path);
            let upstream_path = upstream_dir.join(rel_path);

            let local_raw = read_file_lossy(&local_path);
            let upstream_raw = read_file_lossy(&upstream_path);

            let local_norm;
            let upstream_norm;
            let (local_cmp, upstream_cmp) = if strict_whitespace {
                (local_raw.as_str(), upstream_raw.as_str())
            } else {
                local_norm = normalize_whitespace(&local_raw);
                upstream_norm = normalize_whitespace(&upstream_raw);
                (local_norm.as_str(), upstream_norm.as_str())
            };

            if local_cmp == upstream_cmp {
                unchanged += 1;
                pb.inc(1);
                continue;
            }

            let diff_text = make_unified_diff(
                upstream_cmp,
                local_cmp,
                &format!("a/{rel_path}"),
                &format!("b/{rel_path}"),
            );

            if diff_text.is_empty() {
                unchanged += 1;
                pb.inc(1);
                continue;
            }

            let (ins, del) = count_changes(&diff_text);
            files.push(FileDiff {
                path: rel_path.clone(),
                status: FileStatus::Modified,
                category,
                insertions: ins,
                deletions: del,
                diff_text,
            });
            modified += 1;
            CategorySummary::tally(&mut by_category, category, FileStatus::Modified);
        } else {
            trace!("Added locally: {rel_path}");
            let local_raw = read_file_lossy(&local_dir.join(rel_path));
            let local_content = prepare_content(&local_raw, strict_whitespace);
            let diff_text = make_unified_diff(
                "",
                &local_content,
                &format!("a/{rel_path}"),
                &format!("b/{rel_path}"),
            );

            let (ins, del) = count_changes(&diff_text);
            files.push(FileDiff {
                path: rel_path.clone(),
                status: FileStatus::Added,
                category,
                insertions: ins,
                deletions: del,
                diff_text,
            });
            added += 1;
            CategorySummary::tally(&mut by_category, category, FileStatus::Added);
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    files.sort_by(|a, b| a.path.cmp(&b.path));

    debug!(
        "Diff complete: {added} added, {removed} removed, {modified} modified, {unchanged} unchanged"
    );

    Ok(DiffResult {
        plugin_slug: slug.to_string(),
        plugin_version: version.to_string(),
        files,
        skipped_dirs,
        summary: DiffSummary {
            added,
            removed,
            modified,
            unchanged,
            by_category,
        },
    })
}

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "external",
    ".git",
    ".svn",
    ".hg",
    "vendor",
    ".DS_Store",
];

pub fn should_skip_dir(name: &std::ffi::OsStr) -> bool {
    let s = name.to_string_lossy();
    SKIP_DIRS.iter().any(|&d| s == d)
}

struct CollectResult {
    files: BTreeSet<String>,
    skipped: Vec<String>,
}

#[allow(clippy::unnecessary_wraps)]
fn collect_files(dir: &Path) -> Result<CollectResult> {
    use std::cell::RefCell;

    let files = RefCell::new(BTreeSet::new());
    let skipped = RefCell::new(Vec::new());

    let walker = WalkDir::new(dir).into_iter().filter_entry(|e| {
        if e.file_type().is_dir() && e.depth() > 0
            && let Some(name) = e.path().file_name()
                && should_skip_dir(name) {
                    if e.depth() == 1 {
                        skipped.borrow_mut().push(name.to_string_lossy().to_string());
                    }
                    return false;
                }
        true
    });

    for entry in walker.filter_map(std::result::Result::ok) {
        if entry.file_type().is_file() {
            let rel = entry
                .path()
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .to_string();
            files.borrow_mut().insert(rel);
        }
    }

    let mut skipped = skipped.into_inner();
    skipped.sort();
    Ok(CollectResult {
        files: files.into_inner(),
        skipped,
    })
}

fn read_file_lossy(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

fn normalize_whitespace(content: &str) -> String {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    if old == new {
        return String::new();
    }

    let diff = TextDiff::from_lines(old, new);
    diff.unified_diff()
        .header(old_label, new_label)
        .context_radius(3)
        .to_string()
}

fn prepare_content(raw: &str, strict_whitespace: bool) -> String {
    if strict_whitespace {
        raw.to_string()
    } else {
        normalize_whitespace(raw)
    }
}



fn count_changes(diff_text: &str) -> (usize, usize) {
    let mut insertions = 0;
    let mut deletions = 0;
    for line in diff_text.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            insertions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions += 1;
        }
    }
    (insertions, deletions)
}

pub fn categorize_file(path: &str) -> FileCategory {
    let lower = path.to_lowercase();

    let artifact_dirs = [
        "node_modules/",
        "vendor/",
        "dist/",
        "build/",
        ".git/",
    ];
    for dir in &artifact_dirs {
        if lower.contains(dir) {
            return FileCategory::Artifact;
        }
    }

    if lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".map")
        || lower.ends_with(".bundle.js")
        || lower.ends_with(".chunk.js")
    {
        return FileCategory::Artifact;
    }

    let asset_exts = [
        ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".webp", ".bmp", ".tiff",
        ".woff", ".woff2", ".ttf", ".eot", ".otf",
        ".mp3", ".mp4", ".wav", ".ogg", ".webm",
        ".zip", ".tar", ".gz",
    ];
    for ext in &asset_exts {
        if lower.ends_with(ext) {
            return FileCategory::Asset;
        }
    }

    let metadata_files = [
        "readme.txt",
        "readme.md",
        "changelog.md",
        "changelog.txt",
        "license.txt",
        "license.md",
        "license",
    ];
    let filename = Path::new(&lower)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    for mf in &metadata_files {
        if filename == *mf {
            return FileCategory::Metadata;
        }
    }

    if lower.ends_with(".pot") || lower.ends_with(".po") || lower.ends_with(".mo") {
        return FileCategory::Metadata;
    }

    FileCategory::Source
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorize_source_files() {
        assert_eq!(categorize_file("src/foo.php"), FileCategory::Source);
        assert_eq!(categorize_file("includes/class-wc.php"), FileCategory::Source);
        assert_eq!(categorize_file("style.css"), FileCategory::Source);
        assert_eq!(categorize_file("app.js"), FileCategory::Source);
        assert_eq!(categorize_file("template.html"), FileCategory::Source);
    }

    #[test]
    fn categorize_artifacts_by_dir() {
        assert_eq!(categorize_file("node_modules/lodash/index.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("vendor/autoload.php"), FileCategory::Artifact);
        assert_eq!(categorize_file("dist/bundle.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("build/output.css"), FileCategory::Artifact);
        assert_eq!(categorize_file(".git/config"), FileCategory::Artifact);
    }

    #[test]
    fn categorize_artifacts_by_extension() {
        assert_eq!(categorize_file("app.min.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("style.min.css"), FileCategory::Artifact);
        assert_eq!(categorize_file("app.bundle.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("vendor.chunk.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("app.js.map"), FileCategory::Artifact);
    }

    #[test]
    fn categorize_assets() {
        assert_eq!(categorize_file("logo.png"), FileCategory::Asset);
        assert_eq!(categorize_file("photo.jpg"), FileCategory::Asset);
        assert_eq!(categorize_file("icon.svg"), FileCategory::Asset);
        assert_eq!(categorize_file("font.woff2"), FileCategory::Asset);
        assert_eq!(categorize_file("video.mp4"), FileCategory::Asset);
    }

    #[test]
    fn categorize_metadata() {
        assert_eq!(categorize_file("readme.txt"), FileCategory::Metadata);
        assert_eq!(categorize_file("README.md"), FileCategory::Metadata);
        assert_eq!(categorize_file("changelog.txt"), FileCategory::Metadata);
        assert_eq!(categorize_file("LICENSE.txt"), FileCategory::Metadata);
        assert_eq!(categorize_file("license"), FileCategory::Metadata);
        assert_eq!(categorize_file("languages/plugin.pot"), FileCategory::Metadata);
        assert_eq!(categorize_file("languages/en.po"), FileCategory::Metadata);
        assert_eq!(categorize_file("languages/en.mo"), FileCategory::Metadata);
    }

    #[test]
    fn categorize_artifact_dir_mid_path() {
        assert_eq!(categorize_file("src/dist/output.js"), FileCategory::Artifact);
    }

    #[test]
    fn normalize_crlf() {
        assert_eq!(normalize_whitespace("a\r\nb\r\n"), "a\nb");
        assert_eq!(normalize_whitespace("a\rb\r"), "a\nb");
    }

    #[test]
    fn normalize_trailing_spaces() {
        assert_eq!(normalize_whitespace("hello   \nworld\t\n"), "hello\nworld");
    }

    #[test]
    fn normalize_empty() {
        assert_eq!(normalize_whitespace(""), "");
    }

    #[test]
    fn count_changes_basic() {
        let diff = "--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n context\n-old line\n+new line\n context\n";
        let (ins, del) = count_changes(diff);
        assert_eq!(ins, 1);
        assert_eq!(del, 1);
    }

    #[test]
    fn count_changes_excludes_headers() {
        let diff = "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let (ins, del) = count_changes(diff);
        assert_eq!(ins, 1);
        assert_eq!(del, 1);
    }

    #[test]
    fn count_changes_empty() {
        assert_eq!(count_changes(""), (0, 0));
    }

    #[test]
    fn make_diff_identical() {
        assert_eq!(make_unified_diff("hello\n", "hello\n", "a", "b"), "");
    }

    #[test]
    fn make_diff_added() {
        let diff = make_unified_diff("", "new line\n", "a/f", "b/f");
        assert!(diff.contains("+new line"));
        assert!(diff.contains("--- a/f"));
        assert!(diff.contains("+++ b/f"));
    }

    #[test]
    fn make_diff_removed() {
        let diff = make_unified_diff("old line\n", "", "a/f", "b/f");
        assert!(diff.contains("-old line"));
    }

    #[test]
    fn skip_dir_known() {
        assert!(should_skip_dir(std::ffi::OsStr::new("node_modules")));
        assert!(should_skip_dir(std::ffi::OsStr::new("vendor")));
        assert!(should_skip_dir(std::ffi::OsStr::new(".git")));
        assert!(should_skip_dir(std::ffi::OsStr::new("external")));
    }

    #[test]
    fn skip_dir_not_prefix() {
        assert!(!should_skip_dir(std::ffi::OsStr::new("vendor_custom")));
        assert!(!should_skip_dir(std::ffi::OsStr::new("src")));
        assert!(!should_skip_dir(std::ffi::OsStr::new("includes")));
    }

    #[test]
    fn tally_increments_correctly() {
        let mut map = BTreeMap::new();
        CategorySummary::tally(&mut map, FileCategory::Source, FileStatus::Added);
        CategorySummary::tally(&mut map, FileCategory::Source, FileStatus::Added);
        CategorySummary::tally(&mut map, FileCategory::Source, FileStatus::Modified);
        CategorySummary::tally(&mut map, FileCategory::Asset, FileStatus::Removed);

        assert_eq!(map[&FileCategory::Source].added, 2);
        assert_eq!(map[&FileCategory::Source].modified, 1);
        assert_eq!(map[&FileCategory::Source].removed, 0);
        assert_eq!(map[&FileCategory::Asset].removed, 1);
    }

    #[test]
    fn prepare_content_strict_preserves_crlf() {
        let raw = "hello\r\nworld\r\n";
        assert_eq!(prepare_content(raw, true), raw);
    }

    #[test]
    fn prepare_content_nonstrict_normalizes() {
        assert_eq!(prepare_content("hello\r\n", false), "hello");
    }

    #[test]
    fn apply_filters_categories() {
        let result = make_test_result(vec![
            ("src/a.php", FileCategory::Source, FileStatus::Modified),
            ("app.min.js", FileCategory::Artifact, FileStatus::Modified),
            ("logo.png", FileCategory::Asset, FileStatus::Added),
        ]);

        let cats: HashSet<FileCategory> = [FileCategory::Source].into_iter().collect();
        let filtered = result.apply(&cats, &[]);
        assert_eq!(filtered.files.len(), 1);
        assert_eq!(filtered.files[0].path, "src/a.php");
    }

    #[test]
    fn apply_filters_exclude_bare_name() {
        let result = make_test_result(vec![
            ("assets/js/app.js", FileCategory::Source, FileStatus::Modified),
            ("includes/foo.php", FileCategory::Source, FileStatus::Modified),
        ]);

        let all_cats: HashSet<FileCategory> = [FileCategory::Source, FileCategory::Metadata].into_iter().collect();
        let filtered = result.apply(&all_cats, &["assets".to_string()]);
        assert_eq!(filtered.files.len(), 1);
        assert_eq!(filtered.files[0].path, "includes/foo.php");
    }

    #[test]
    fn apply_filters_exclude_glob() {
        let result = make_test_result(vec![
            ("src/a.php", FileCategory::Source, FileStatus::Modified),
            ("src/b.js", FileCategory::Source, FileStatus::Modified),
        ]);

        let all_cats: HashSet<FileCategory> = [FileCategory::Source].into_iter().collect();
        let filtered = result.apply(&all_cats, &["**/*.js".to_string()]);
        assert_eq!(filtered.files.len(), 1);
        assert_eq!(filtered.files[0].path, "src/a.php");
    }

    fn make_test_result(files: Vec<(&str, FileCategory, FileStatus)>) -> DiffResult {
        DiffResult {
            plugin_slug: "test".to_string(),
            plugin_version: "1.0".to_string(),
            files: files
                .into_iter()
                .map(|(path, category, status)| FileDiff {
                    path: path.to_string(),
                    status,
                    category,
                    insertions: 0,
                    deletions: 0,
                    diff_text: String::new(),
                })
                .collect(),
            skipped_dirs: SkippedDirs {
                local: vec![],
                upstream: vec![],
            },
            summary: DiffSummary {
                added: 0,
                removed: 0,
                modified: 0,
                unchanged: 0,
                by_category: BTreeMap::new(),
            },
        }
    }
}
