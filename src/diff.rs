use crate::progress;
use anyhow::{Context, Result};
use log::{debug, trace};
use serde::Serialize;
use similar::{DiffTag, TextDiff};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Debug, Serialize)]
pub struct DirDiff {
    pub files: Vec<FileDiff>,
    pub skipped_dirs: SkippedDirs,
    pub summary: DiffSummary,
}

impl DirDiff {
    pub fn new(files: Vec<FileDiff>, skipped_dirs: SkippedDirs, unchanged: usize) -> Self {
        let summary = DiffSummary::of(&files, unchanged);
        Self {
            files,
            skipped_dirs,
            summary,
        }
    }

    /// `summary.unchanged` keeps counting every identical file, whatever its
    /// category.
    #[must_use]
    pub fn filtered(mut self, filter: &Filter) -> Self {
        self.files.retain(|f| filter.accepts(f));
        self.summary = DiffSummary::of(&self.files, self.summary.unchanged);
        self
    }

    /// All file diffs as one unified patch (upstream → local).
    pub fn patch(&self) -> String {
        self.files.iter().map(|f| f.diff_text.as_str()).collect()
    }

    /// Total `(insertions, deletions)` across all files.
    pub fn line_changes(&self) -> (usize, usize) {
        self.files.iter().fold((0, 0), |(ins, del), f| {
            (ins + f.insertions, del + f.deletions)
        })
    }
}

#[derive(Debug, Default, Serialize)]
pub struct SkippedDirs {
    pub local: Vec<String>,
    pub upstream: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct DiffSummary {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub unchanged: usize,
    pub by_category: BTreeMap<FileCategory, CategorySummary>,
}

impl DiffSummary {
    fn of(files: &[FileDiff], unchanged: usize) -> Self {
        let mut summary = Self {
            unchanged,
            ..Self::default()
        };
        for file in files {
            let category = summary.by_category.entry(file.category).or_default();
            let (total, in_category) = match file.status {
                FileStatus::Added => (&mut summary.added, &mut category.added),
                FileStatus::Removed => (&mut summary.removed, &mut category.removed),
                FileStatus::Modified => (&mut summary.modified, &mut category.modified),
            };
            *total += 1;
            *in_category += 1;
        }
        summary
    }
}

#[derive(Debug, Default, Serialize)]
pub struct CategorySummary {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
}

#[derive(Debug, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub status: FileStatus,
    pub category: FileCategory,
    pub insertions: usize,
    pub deletions: usize,
    pub diff_text: String,
}

impl FileDiff {
    fn new(path: &str, status: FileStatus, upstream: &str, local: &str) -> Self {
        let diff = TextDiff::from_lines(upstream, local);
        let mut insertions = 0;
        let mut deletions = 0;
        for op in diff.ops() {
            match op.tag() {
                DiffTag::Equal => {}
                DiffTag::Insert => insertions += op.new_range().len(),
                DiffTag::Delete => deletions += op.old_range().len(),
                DiffTag::Replace => {
                    insertions += op.new_range().len();
                    deletions += op.old_range().len();
                }
            }
        }

        Self {
            path: path.to_string(),
            status,
            category: categorize_file(path),
            insertions,
            deletions,
            diff_text: format_unified(&diff, &format!("a/{path}"), &format!("b/{path}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Removed,
    Modified,
}

/// Declared in the order categories are listed in output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileCategory {
    Source,
    Artifact,
    Asset,
    Metadata,
}

#[derive(Debug, Default)]
pub struct Filter {
    include_artifacts: bool,
    include_assets: bool,
    exclude_globs: Vec<String>,
}

impl Filter {
    /// `exclude` entries are globs; an entry without glob syntax names a file
    /// or directory to drop at any depth.
    pub fn new(include_artifacts: bool, include_assets: bool, exclude: &[String]) -> Self {
        let exclude_globs = exclude
            .iter()
            .flat_map(|pattern| {
                if pattern.contains(['*', '?', '[']) {
                    return vec![pattern.clone()];
                }
                let name = pattern.trim_end_matches('/');
                vec![
                    name.to_string(),
                    format!("{name}/**"),
                    format!("**/{name}"),
                    format!("**/{name}/**"),
                ]
            })
            .collect();

        Self {
            include_artifacts,
            include_assets,
            exclude_globs,
        }
    }

    fn accepts(&self, file: &FileDiff) -> bool {
        let category_included = match file.category {
            FileCategory::Source | FileCategory::Metadata => true,
            FileCategory::Artifact => self.include_artifacts,
            FileCategory::Asset => self.include_assets,
        };
        category_included
            && !self
                .exclude_globs
                .iter()
                .any(|glob| glob_match::glob_match(glob, &file.path))
    }
}

pub fn diff_directories(
    local_dir: &Path,
    upstream_dir: &Path,
    strict_whitespace: bool,
) -> Result<DirDiff> {
    let local = collect_files(local_dir)?;
    let upstream = collect_files(upstream_dir)?;

    debug!(
        "Comparing {} local files against {} upstream files",
        local.files.len(),
        upstream.files.len()
    );
    if !local.skipped_dirs.is_empty() {
        debug!("Skipped local dirs: {}", local.skipped_dirs.join(", "));
    }
    if !upstream.skipped_dirs.is_empty() {
        debug!(
            "Skipped upstream dirs: {}",
            upstream.skipped_dirs.join(", ")
        );
    }

    let paths: Vec<&String> = local.files.union(&upstream.files).collect();
    let pb = progress::bar(
        paths.len() as u64,
        "  {spinner:.green} Comparing [{bar:30.cyan/dim}] {pos}/{len} files",
    );

    let mut files = Vec::new();
    let mut unchanged = 0usize;

    for rel_path in paths {
        let status = match (
            upstream.files.contains(rel_path),
            local.files.contains(rel_path),
        ) {
            (true, true) => FileStatus::Modified,
            (true, false) => FileStatus::Removed,
            (false, _) => FileStatus::Added,
        };

        let upstream_text = if status == FileStatus::Added {
            trace!("Added locally: {rel_path}");
            String::new()
        } else {
            read_text(&upstream_dir.join(rel_path), strict_whitespace)?
        };
        let local_text = if status == FileStatus::Removed {
            trace!("Removed from local: {rel_path}");
            String::new()
        } else {
            read_text(&local_dir.join(rel_path), strict_whitespace)?
        };
        pb.inc(1);

        if status == FileStatus::Modified && upstream_text == local_text {
            unchanged += 1;
            continue;
        }
        files.push(FileDiff::new(rel_path, status, &upstream_text, &local_text));
    }
    pb.finish_and_clear();

    let result = DirDiff::new(
        files,
        SkippedDirs {
            local: local.skipped_dirs,
            upstream: upstream.skipped_dirs,
        },
        unchanged,
    );
    debug!(
        "Diff complete: {} added, {} removed, {} modified, {} unchanged",
        result.summary.added,
        result.summary.removed,
        result.summary.modified,
        result.summary.unchanged
    );
    Ok(result)
}

/// Lines of unchanged context around each change in a unified diff.
pub const CONTEXT_RADIUS: usize = 3;

/// Unified diff of two texts, or an empty string when they are equal.
pub fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    format_unified(&TextDiff::from_lines(old, new), old_label, new_label)
}

fn format_unified(diff: &TextDiff<'_, '_, str>, old_label: &str, new_label: &str) -> String {
    diff.unified_diff()
        .header(old_label, new_label)
        .context_radius(CONTEXT_RADIUS)
        .to_string()
}

const SKIP_DIRS: &[&str] = &["node_modules", "external", ".git", ".svn", ".hg", "vendor"];

struct Tree {
    /// Paths relative to the root.
    files: BTreeSet<String>,
    /// Top-level directories that were not walked.
    skipped_dirs: Vec<String>,
}

fn collect_files(dir: &Path) -> Result<Tree> {
    let mut files = BTreeSet::new();
    let mut skipped_dirs = Vec::new();

    let mut walker = WalkDir::new(dir).min_depth(1).into_iter();
    while let Some(entry) = walker.next() {
        let entry = entry.with_context(|| format!("Failed to read {}", dir.display()))?;
        let file_type = entry.file_type();

        if file_type.is_dir() {
            let name = entry.file_name();
            if SKIP_DIRS.iter().any(|skip| name == *skip) {
                if entry.depth() == 1 {
                    skipped_dirs.push(name.to_string_lossy().into_owned());
                }
                walker.skip_current_dir();
            }
        } else if file_type.is_file() {
            let rel = entry.path().strip_prefix(dir)?;
            files.insert(rel.to_string_lossy().into_owned());
        }
    }

    skipped_dirs.sort();
    Ok(Tree {
        files,
        skipped_dirs,
    })
}

fn read_text(path: &Path, strict_whitespace: bool) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(if strict_whitespace {
        text.into_owned()
    } else {
        normalize_whitespace(&text)
    })
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

const ARTIFACT_DIRS: &[&str] = &["node_modules/", "vendor/", "dist/", "build/", ".git/"];
const ARTIFACT_SUFFIXES: &[&str] = &[".min.js", ".min.css", ".map", ".bundle.js", ".chunk.js"];
const ASSET_SUFFIXES: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".svg", ".ico", ".webp", ".bmp", ".tiff", ".woff", ".woff2",
    ".ttf", ".eot", ".otf", ".mp3", ".mp4", ".wav", ".ogg", ".webm", ".zip", ".tar", ".gz",
];
const METADATA_FILES: &[&str] = &[
    "readme.txt",
    "readme.md",
    "changelog.md",
    "changelog.txt",
    "license.txt",
    "license.md",
    "license",
];
const METADATA_SUFFIXES: &[&str] = &[".pot", ".po", ".mo"];

fn categorize_file(path: &str) -> FileCategory {
    let lower = path.to_lowercase();
    let ends_with_any = |suffixes: &[&str]| suffixes.iter().any(|s| lower.ends_with(s));
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);

    if ARTIFACT_DIRS.iter().any(|dir| lower.contains(dir)) || ends_with_any(ARTIFACT_SUFFIXES) {
        FileCategory::Artifact
    } else if ends_with_any(ASSET_SUFFIXES) {
        FileCategory::Asset
    } else if METADATA_FILES.contains(&file_name) || ends_with_any(METADATA_SUFFIXES) {
        FileCategory::Metadata
    } else {
        FileCategory::Source
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    fn diff_trees(local: &[(&str, &str)], upstream: &[(&str, &str)], strict: bool) -> DirDiff {
        diff_directories(tree(local).path(), tree(upstream).path(), strict).unwrap()
    }

    fn changed(paths: &[(&str, FileStatus)]) -> DirDiff {
        let files = paths
            .iter()
            .map(|(path, status)| FileDiff::new(path, *status, "", ""))
            .collect();
        DirDiff::new(files, SkippedDirs::default(), 0)
    }

    fn paths(diff: &DirDiff) -> Vec<&str> {
        diff.files.iter().map(|f| f.path.as_str()).collect()
    }

    fn exclude(patterns: &[&str]) -> Filter {
        let patterns: Vec<String> = patterns.iter().map(ToString::to_string).collect();
        Filter::new(false, false, &patterns)
    }

    #[test]
    fn categorize_one_file_of_each_kind() {
        assert_eq!(
            categorize_file("includes/class-wc.php"),
            FileCategory::Source
        );
        assert_eq!(categorize_file("dist/bundle.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("app.min.js"), FileCategory::Artifact);
        assert_eq!(categorize_file("font.woff2"), FileCategory::Asset);
        assert_eq!(categorize_file("changelog.txt"), FileCategory::Metadata);
        assert_eq!(categorize_file("languages/en.po"), FileCategory::Metadata);
    }

    #[test]
    fn categorize_ignores_case() {
        assert_eq!(categorize_file("README.md"), FileCategory::Metadata);
        assert_eq!(categorize_file("Assets/Logo.PNG"), FileCategory::Asset);
        assert_eq!(
            categorize_file("Vendor/autoload.php"),
            FileCategory::Artifact
        );
    }

    #[test]
    fn categorize_generated_locations_win_over_file_type() {
        assert_eq!(categorize_file("src/dist/logo.png"), FileCategory::Artifact);
        assert_eq!(categorize_file("build/readme.txt"), FileCategory::Artifact);
    }

    #[test]
    fn categorize_metadata_name_only_matches_whole_file_name() {
        assert_eq!(categorize_file("docs/license"), FileCategory::Metadata);
        assert_eq!(categorize_file("my-license"), FileCategory::Source);
    }

    #[test]
    fn unified_diff_of_equal_texts_is_empty() {
        assert_eq!(unified_diff("hello\n", "hello\n", "a", "b"), "");
        assert_eq!(unified_diff("", "", "a", "b"), "");
    }

    #[test]
    fn unified_diff_has_labels_and_changes() {
        let diff = unified_diff("old line\n", "new line\n", "a/f", "b/f");
        assert_eq!(
            diff,
            "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old line\n+new line\n"
        );
    }

    #[test]
    fn line_counts_include_lines_that_look_like_diff_headers() {
        let file = FileDiff::new(
            "q.sql",
            FileStatus::Modified,
            "keep\n-- old comment\n",
            "keep\n++ new marker\nextra\n",
        );
        assert_eq!((file.insertions, file.deletions), (2, 1));
    }

    #[test]
    fn filter_hides_artifacts_and_assets_unless_included() {
        let all = [
            ("src/a.php", FileStatus::Modified),
            ("readme.txt", FileStatus::Modified),
            ("app.min.js", FileStatus::Modified),
            ("logo.png", FileStatus::Added),
        ];

        let default = changed(&all).filtered(&Filter::default());
        assert_eq!(paths(&default), ["src/a.php", "readme.txt"]);

        let artifacts = changed(&all).filtered(&Filter::new(true, false, &[]));
        assert_eq!(paths(&artifacts), ["src/a.php", "readme.txt", "app.min.js"]);

        let assets = changed(&all).filtered(&Filter::new(false, true, &[]));
        assert_eq!(paths(&assets), ["src/a.php", "readme.txt", "logo.png"]);
    }

    #[test]
    fn exclude_bare_name_drops_directory_at_any_depth() {
        let diff = changed(&[
            ("a/b/c.php", FileStatus::Modified),
            ("x/a/d.php", FileStatus::Modified),
            ("a.php", FileStatus::Modified),
            ("e.php", FileStatus::Modified),
        ]);
        assert_eq!(paths(&diff.filtered(&exclude(&["a"]))), ["a.php", "e.php"]);
    }

    #[test]
    fn exclude_trailing_slash_is_ignored() {
        let diff = changed(&[
            ("assets/js/app.js", FileStatus::Modified),
            ("includes/foo.php", FileStatus::Modified),
        ]);
        assert_eq!(
            paths(&diff.filtered(&exclude(&["assets/"]))),
            ["includes/foo.php"]
        );
    }

    #[test]
    fn exclude_glob_is_used_verbatim() {
        let diff = changed(&[
            ("src/a.php", FileStatus::Modified),
            ("src/b.js", FileStatus::Modified),
            ("b.js", FileStatus::Modified),
        ]);
        assert_eq!(
            paths(&diff.filtered(&exclude(&["src/*.js"]))),
            ["src/a.php", "b.js"]
        );
    }

    #[test]
    fn filtering_recalculates_summary_but_keeps_unchanged() {
        let files = [
            ("a.php", FileStatus::Added),
            ("b.php", FileStatus::Modified),
            ("readme.txt", FileStatus::Removed),
            ("c.min.js", FileStatus::Modified),
        ]
        .map(|(path, status)| FileDiff::new(path, status, "", ""));
        let diff = DirDiff::new(files.into(), SkippedDirs::default(), 7);
        assert_eq!(diff.summary.modified, 2);

        let summary = diff.filtered(&Filter::default()).summary;
        assert_eq!(
            (summary.added, summary.removed, summary.modified),
            (1, 1, 1)
        );
        assert_eq!(summary.unchanged, 7);
        assert_eq!(summary.by_category[&FileCategory::Source].added, 1);
        assert_eq!(summary.by_category[&FileCategory::Source].modified, 1);
        assert_eq!(summary.by_category[&FileCategory::Metadata].removed, 1);
        assert!(!summary.by_category.contains_key(&FileCategory::Artifact));
    }

    #[test]
    fn patch_concatenates_file_diffs_in_path_order() {
        let diff = diff_trees(
            &[("b.php", "new b\n"), ("a.php", "new a\n")],
            &[("b.php", "old b\n"), ("a.php", "old a\n")],
            true,
        );
        assert_eq!(
            diff.patch(),
            "--- a/a.php\n+++ b/a.php\n@@ -1 +1 @@\n-old a\n+new a\n\
             --- a/b.php\n+++ b/b.php\n@@ -1 +1 @@\n-old b\n+new b\n"
        );
        assert_eq!(diff.line_changes(), (2, 2));
    }

    #[test]
    fn diff_directories_identical() {
        let diff = diff_trees(&[("a.php", "hello\n")], &[("a.php", "hello\n")], false);
        assert!(diff.files.is_empty());
        assert_eq!(diff.summary.unchanged, 1);
    }

    #[test]
    fn diff_directories_modified_file() {
        let diff = diff_trees(
            &[("a.php", "same\nmodified\n")],
            &[("a.php", "same\noriginal\n")],
            false,
        );
        assert_eq!(diff.summary.modified, 1);
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].status, FileStatus::Modified);
        assert_eq!((diff.files[0].insertions, diff.files[0].deletions), (1, 1));
    }

    #[test]
    fn diff_directories_added_file() {
        let diff = diff_trees(
            &[("a.php", "hello\n"), ("sub/new.php", "one\ntwo\n")],
            &[("a.php", "hello\n")],
            false,
        );
        assert_eq!(diff.summary.added, 1);
        assert_eq!(diff.summary.unchanged, 1);
        assert_eq!(paths(&diff), ["sub/new.php"]);
        assert_eq!(diff.files[0].status, FileStatus::Added);
        assert_eq!((diff.files[0].insertions, diff.files[0].deletions), (2, 0));
    }

    #[test]
    fn diff_directories_removed_file() {
        let diff = diff_trees(
            &[("a.php", "hello\n")],
            &[("a.php", "hello\n"), ("old.php", "removed\n")],
            false,
        );
        assert_eq!(diff.summary.removed, 1);
        assert_eq!(paths(&diff), ["old.php"]);
        assert_eq!(diff.files[0].status, FileStatus::Removed);
        assert_eq!((diff.files[0].insertions, diff.files[0].deletions), (0, 1));
    }

    #[test]
    fn diff_directories_reports_empty_added_file() {
        let diff = diff_trees(&[("empty.php", "")], &[], false);
        assert_eq!(paths(&diff), ["empty.php"]);
        assert_eq!(diff.files[0].status, FileStatus::Added);
    }

    #[test]
    fn diff_directories_skips_dependency_dirs_at_any_depth() {
        let diff = diff_trees(
            &[
                ("a.php", "hello\n"),
                ("node_modules/pkg/index.js", "junk\n"),
                ("vendor/lib/x.php", "vendor\n"),
                ("src/external/y.php", "nested\n"),
                ("vendor_custom/z.php", "kept\n"),
            ],
            &[("a.php", "hello\n"), ("vendor/lib/x.php", "upstream\n")],
            false,
        );
        assert_eq!(paths(&diff), ["vendor_custom/z.php"]);
        assert_eq!(diff.skipped_dirs.local, ["node_modules", "vendor"]);
        assert_eq!(diff.skipped_dirs.upstream, ["vendor"]);
    }

    #[test]
    fn diff_directories_whitespace_ignored_by_default() {
        let diff = diff_trees(
            &[("a.php", "hello   \r\nworld\r\n"), ("mac.php", "a\rb\r")],
            &[("a.php", "hello\nworld\n"), ("mac.php", "a\nb\n")],
            false,
        );
        assert_eq!(diff.summary.unchanged, 2);
        assert!(diff.files.is_empty());
    }

    #[test]
    fn diff_directories_whitespace_strict() {
        let diff = diff_trees(&[("a.php", "hello   \n")], &[("a.php", "hello\n")], true);
        assert_eq!(diff.summary.modified, 1);
        assert_eq!(
            diff.files[0].diff_text,
            "--- a/a.php\n+++ b/a.php\n@@ -1 +1 @@\n-hello\n+hello   \n"
        );
    }

    #[test]
    fn diff_directories_fails_on_missing_directory() {
        let local = tree(&[("a.php", "hello\n")]);
        let missing = local.path().join("does-not-exist");
        assert!(diff_directories(local.path(), &missing, false).is_err());
    }
}
