use crate::diff::{DirDiff, FileCategory, FileDiff, FileStatus, SkippedDirs};
use colored::{Color, ColoredString, Colorize};
use serde::Serialize;
use std::io::{self, Write};

/// Serializes as a single flat JSON object.
#[derive(Debug, Serialize)]
pub struct Report {
    pub plugin_slug: String,
    pub plugin_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(flatten)]
    pub diff: DirDiff,
}

impl Report {
    /// The latest upstream version, when it is known and is not the installed one.
    fn newer_version(&self) -> Option<&str> {
        self.latest_version
            .as_deref()
            .filter(|latest| *latest != self.plugin_version)
    }
}

/// Writes the unified diff of a single file, colouring its hunks.
pub fn write_colored_diff(
    out: &mut impl Write,
    diff_text: &str,
    indent: &str,
) -> anyhow::Result<()> {
    for (line, color) in diff_line_colors(diff_text) {
        match color {
            Some(color) => writeln!(out, "{indent}{}", line.color(color))?,
            None => writeln!(out, "{indent}{line}")?,
        }
    }
    Ok(())
}

fn diff_line_colors(diff_text: &str) -> impl Iterator<Item = (&str, Option<Color>)> {
    let mut in_hunk = false;
    diff_text.lines().map(move |line| {
        in_hunk |= line.starts_with("@@");
        let color = match line.as_bytes().first() {
            Some(b'@') if in_hunk => Some(Color::Cyan),
            Some(b'+') if in_hunk => Some(Color::Green),
            Some(b'-') if in_hunk => Some(Color::Red),
            _ => None,
        };
        (line, color)
    })
}

fn version_label(version: &str) -> String {
    format!("v{version}")
}

const fn category_heading(category: FileCategory) -> &'static str {
    match category {
        FileCategory::Source => "Source Files",
        FileCategory::Artifact => "Build Artifacts",
        FileCategory::Asset => "Assets",
        FileCategory::Metadata => "Metadata",
    }
}

pub fn render_terminal(report: &Report, out: &mut impl Write) -> anyhow::Result<()> {
    let diff = &report.diff;

    write_title(report, out)?;
    writeln!(
        out,
        "  {} added, {} removed, {} modified, {} unchanged\n",
        diff.summary.added.to_string().green(),
        diff.summary.removed.to_string().red(),
        diff.summary.modified.to_string().yellow(),
        diff.summary.unchanged,
    )?;
    write_skipped_dirs(&diff.skipped_dirs, out)?;

    if diff.files.is_empty() {
        write_no_differences(out)?;
        return Ok(());
    }

    for category in diff.summary.by_category.keys() {
        let heading = category_heading(*category);
        writeln!(out, "{}", format!("── {heading} ──").bold().dimmed())?;
        for file in diff.files.iter().filter(|f| f.category == *category) {
            writeln!(out, "  {} {}", status_marker(file.status), file.path)?;
        }
        writeln!(out)?;
    }

    for file in &diff.files {
        if file.diff_text.is_empty() {
            continue;
        }

        writeln!(out, "{}", format!("diff {}", file.path).bold())?;
        write_colored_diff(out, &file.diff_text, "")?;
        writeln!(out)?;
    }

    Ok(())
}

pub fn render_summary(report: &Report, out: &mut impl Write) -> anyhow::Result<()> {
    write_title(report, out)?;
    write_skipped_dirs(&report.diff.skipped_dirs, out)?;

    if report.diff.files.is_empty() {
        write_no_differences(out)?;
    } else {
        write_diffstat(&report.diff, term_width(), out)?;
    }
    Ok(())
}

pub fn render_json(report: &Report, out: &mut impl Write) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(out, report)?;
    Ok(())
}

fn write_title(report: &Report, out: &mut impl Write) -> io::Result<()> {
    let installed = version_label(&report.plugin_version);
    write!(
        out,
        "{}",
        format!("wpdiff: {} {installed}", report.plugin_slug).bold(),
    )?;
    if let Some(latest) = report.newer_version() {
        write!(out, " (latest: {})", version_label(latest).green())?;
    }
    writeln!(out)
}

fn write_no_differences(out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "{}", "No differences found.".green().bold())
}

fn status_marker(status: FileStatus) -> ColoredString {
    match status {
        FileStatus::Added => "+".green().bold(),
        FileStatus::Removed => "-".red().bold(),
        FileStatus::Modified => "~".yellow().bold(),
    }
}

fn write_skipped_dirs(skipped: &SkippedDirs, out: &mut impl Write) -> io::Result<()> {
    if skipped.local.is_empty() && skipped.upstream.is_empty() {
        return Ok(());
    }
    let (both, local_only): (Vec<_>, Vec<_>) = skipped
        .local
        .iter()
        .partition(|dir| skipped.upstream.contains(dir));
    let upstream_only = skipped
        .upstream
        .iter()
        .filter(|dir| !skipped.local.contains(dir));

    writeln!(out, "{}", "── Skipped Directories ──".bold().dimmed())?;
    for dir in both {
        writeln!(out, "  {} {}/", "=".dimmed(), dir)?;
    }
    for dir in local_only {
        writeln!(
            out,
            "  {} {}/ {}",
            "+".green().bold(),
            dir,
            "(local only)".dimmed()
        )?;
    }
    for dir in upstream_only {
        writeln!(
            out,
            "  {} {}/ {}",
            "-".red().bold(),
            dir,
            "(upstream only)".dimmed()
        )?;
    }
    writeln!(out)
}

fn term_width() -> usize {
    terminal_size::terminal_size().map_or(80, |(w, _)| w.0 as usize)
}

fn write_diffstat(diff: &DirDiff, width: usize, out: &mut impl Write) -> io::Result<()> {
    let changes = |f: &FileDiff| f.insertions + f.deletions;
    let max_path_len = diff.files.iter().map(|f| f.path.len()).max().unwrap_or(0);
    let max_changes = diff.files.iter().map(changes).max().unwrap_or(0);
    let max_change_digits = max_changes.to_string().len();

    let fixed_cols = " ~ ".len() + max_path_len + " | ".len() + max_change_digits + " ".len();
    let bar_width = if width > fixed_cols + 2 {
        width - fixed_cols
    } else {
        10
    };

    for file in &diff.files {
        let total = changes(file);
        let bar_total = total
            .saturating_mul(bar_width)
            .checked_div(max_changes)
            .unwrap_or(0)
            .max(usize::from(total > 0));
        let bar_ins = file
            .insertions
            .saturating_mul(bar_total)
            .checked_div(total)
            .unwrap_or(0)
            .max(usize::from(file.insertions > 0));
        let bar_del = bar_total.saturating_sub(bar_ins);

        writeln!(
            out,
            " {} {:max_path_len$} | {:>max_change_digits$} {}{}",
            status_marker(file.status),
            file.path,
            total,
            "+".repeat(bar_ins).green(),
            "-".repeat(bar_del).red(),
        )?;
    }

    let (insertions, deletions) = diff.line_changes();
    writeln!(out)?;
    writeln!(
        out,
        " {} files changed, {} insertions(+), {} deletions(-)",
        diff.files.len().to_string().bold(),
        insertions.to_string().green(),
        deletions.to_string().red(),
    )?;

    if diff.summary.unchanged > 0 {
        writeln!(out, " {} files unchanged", diff.summary.unchanged)?;
    }
    Ok(())
}

/// Count columns of the multi-plugin table: title, width, sign, colour.
const COUNT_COLUMNS: [(&str, usize, &str, Color); 5] = [
    ("Added", 6, "+", Color::Green),
    ("Rm'd", 6, "-", Color::Red),
    ("Mod", 5, "", Color::Yellow),
    ("Ins(+)", 8, "+", Color::Green),
    ("Del(-)", 8, "-", Color::Red),
];

type Counts = [usize; COUNT_COLUMNS.len()];

fn count_cells(counts: Counts) -> [String; COUNT_COLUMNS.len()] {
    std::array::from_fn(|i| {
        let (_, width, sign, color) = COUNT_COLUMNS[i];
        if counts[i] > 0 {
            format!("{:>width$}", format!("{sign}{}", counts[i]))
                .color(color)
                .to_string()
        } else {
            format!("{:>width$}", "·").dimmed().to_string()
        }
    })
}

const NAME_TITLE: &str = "Plugin";
const VERSION_TITLE: &str = "Ver";
const LATEST_TITLE: &str = "Latest";

fn column_width(title: &str, cell_lengths: impl Iterator<Item = usize>) -> usize {
    cell_lengths.max().unwrap_or(0).max(title.len())
}

struct ColumnWidths {
    name: usize,
    version: usize,
    /// Present only when some plugin's latest version differs from the installed one.
    latest: Option<usize>,
}

impl ColumnWidths {
    fn write_row(
        &self,
        out: &mut impl Write,
        name: &ColoredString,
        version: &ColoredString,
        latest: &ColoredString,
        cells: &[String],
    ) -> io::Result<()> {
        write!(
            out,
            " {name:<name_width$}  {version:<version_width$}",
            name_width = self.name,
            version_width = self.version,
        )?;
        if let Some(latest_width) = self.latest {
            write!(out, "  {latest:<latest_width$}")?;
        }
        for cell in cells {
            write!(out, "  {cell}")?;
        }
        writeln!(out)
    }
}

pub fn render_summary_table(
    reports: &[Report],
    clean_count: usize,
    unmatched_count: usize,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    if reports.is_empty() && clean_count == 0 && unmatched_count == 0 {
        return Ok(());
    }

    let any_outdated = reports.iter().any(|r| r.newer_version().is_some());
    let version_len = |version: &String| version_label(version).len();
    let layout = ColumnWidths {
        name: column_width(NAME_TITLE, reports.iter().map(|r| r.plugin_slug.len())),
        version: column_width(
            VERSION_TITLE,
            reports.iter().map(|r| version_len(&r.plugin_version)),
        ),
        latest: any_outdated.then(|| {
            column_width(
                LATEST_TITLE,
                reports
                    .iter()
                    .filter_map(|r| r.latest_version.as_ref())
                    .map(version_len),
            )
        }),
    };

    layout.write_row(
        out,
        &NAME_TITLE.bold().underline(),
        &VERSION_TITLE.bold().underline(),
        &LATEST_TITLE.bold().underline(),
        &COUNT_COLUMNS.map(|(title, width, ..)| format!("{:>width$}", title.bold().underline())),
    )?;

    let mut totals = Counts::default();
    for report in reports {
        let summary = &report.diff.summary;
        let (insertions, deletions) = report.diff.line_changes();
        let counts = [
            summary.added,
            summary.removed,
            summary.modified,
            insertions,
            deletions,
        ];
        for (total, count) in totals.iter_mut().zip(counts) {
            *total += count;
        }

        layout.write_row(
            out,
            &report.plugin_slug.bold(),
            &version_label(&report.plugin_version).dimmed(),
            &report
                .newer_version()
                .map_or_else(|| "✓".dimmed(), |latest| version_label(latest).green()),
            &count_cells(counts),
        )?;
    }

    if reports.len() > 1 {
        let rule = |width: usize| "─".repeat(width).dimmed();
        layout.write_row(
            out,
            &"".dimmed(),
            &"".dimmed(),
            &rule(layout.latest.unwrap_or(0)),
            &COUNT_COLUMNS.map(|(_, width, ..)| rule(width).to_string()),
        )?;
        layout.write_row(
            out,
            &"Total".bold(),
            &"".normal(),
            &"".normal(),
            &count_cells(totals),
        )?;
    }

    let total_plugins = reports.len() + clean_count + unmatched_count;
    writeln!(out)?;
    writeln!(
        out,
        " {} plugins scanned: {} changed, {} unchanged, {} unmatched",
        total_plugins.to_string().bold(),
        reports.len().to_string().yellow(),
        clean_count.to_string().green(),
        if unmatched_count > 0 {
            unmatched_count.to_string().red()
        } else {
            unmatched_count.to_string().normal()
        },
    )?;

    Ok(())
}

pub fn render_scan_footer(
    changed: usize,
    clean: usize,
    unmatched: usize,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    writeln!(out)?;
    writeln!(
        out,
        "{changed} changed, {clean} unchanged, {unmatched} unmatched"
    )?;
    Ok(())
}

#[derive(Serialize)]
struct ScanDocument<'a> {
    changed: &'a [Report],
    unchanged: usize,
    unmatched: Vec<UnmatchedPlugin<'a>>,
}

#[derive(Serialize)]
struct UnmatchedPlugin<'a> {
    plugin_slug: &'a str,
    error: &'a str,
}

/// A whole installation as one JSON document; `unmatched` pairs each plugin
/// that could not be compared with the reason.
pub fn render_scan_json(
    changed: &[Report],
    unchanged: usize,
    unmatched: &[(String, String)],
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let document = ScanDocument {
        changed,
        unchanged,
        unmatched: unmatched
            .iter()
            .map(|(plugin_slug, error)| UnmatchedPlugin { plugin_slug, error })
            .collect(),
    };
    serde_json::to_writer_pretty(&mut *out, &document)?;
    writeln!(out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        slug: &str,
        version: &str,
        latest: Option<&str>,
        files: Vec<FileDiff>,
        skipped_local: &[&str],
        skipped_upstream: &[&str],
    ) -> Report {
        let owned = |dirs: &[&str]| dirs.iter().map(ToString::to_string).collect();
        Report {
            plugin_slug: slug.to_string(),
            plugin_version: version.to_string(),
            latest_version: latest.map(String::from),
            diff: DirDiff::new(
                files,
                SkippedDirs {
                    local: owned(skipped_local),
                    upstream: owned(skipped_upstream),
                },
                5,
            ),
        }
    }

    fn file(
        path: &str,
        status: FileStatus,
        category: FileCategory,
        insertions: usize,
        deletions: usize,
    ) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            status,
            category,
            insertions,
            deletions,
            diff_text: if insertions > 0 || deletions > 0 {
                format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+new\n")
            } else {
                String::new()
            },
        }
    }

    fn rendered(render: impl FnOnce(&mut Vec<u8>) -> anyhow::Result<()>) -> String {
        colored::control::set_override(false);
        let mut buf = Vec::new();
        render(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn only_hunk_lines_are_coloured() {
        let diff = "--- a/q.sql\n+++ b/q.sql\n@@ -1,3 +1,3 @@\n keep\n-- old comment\n++ marker\n\\ No newline at end of file\n";
        let colors: Vec<Option<Color>> = diff_line_colors(diff).map(|(_, color)| color).collect();
        assert_eq!(
            colors,
            [
                None,
                None,
                Some(Color::Cyan),
                None,
                Some(Color::Red),
                Some(Color::Green),
                None,
            ]
        );
    }

    #[test]
    fn colored_diff_indents_every_line() {
        let out = rendered(|buf| write_colored_diff(buf, "@@ -1 +1 @@\n-old\n+new\n", "  "));
        assert_eq!(out, "  @@ -1 +1 @@\n  -old\n  +new\n");
    }

    #[test]
    fn terminal_without_differences() {
        let report = report("akismet", "5.0", None, vec![], &[], &[]);
        let out = rendered(|buf| render_terminal(&report, buf));
        assert_eq!(
            out,
            "wpdiff: akismet v5.0\n\
             \x20 0 added, 0 removed, 0 modified, 5 unchanged\n\
             \n\
             No differences found.\n"
        );
    }

    #[test]
    fn terminal_lists_files_by_category_then_shows_diffs() {
        let files = vec![
            file(
                "app.min.js",
                FileStatus::Modified,
                FileCategory::Artifact,
                1,
                1,
            ),
            file("empty.php", FileStatus::Added, FileCategory::Source, 0, 0),
            file("old.php", FileStatus::Removed, FileCategory::Source, 0, 1),
        ];
        let report = report("test", "1.0", None, files, &[], &[]);
        let out = rendered(|buf| render_terminal(&report, buf));
        assert_eq!(
            out,
            "wpdiff: test v1.0\n\
             \x20 1 added, 1 removed, 1 modified, 5 unchanged\n\
             \n\
             ── Source Files ──\n\
             \x20 + empty.php\n\
             \x20 - old.php\n\
             \n\
             ── Build Artifacts ──\n\
             \x20 ~ app.min.js\n\
             \n\
             diff app.min.js\n\
             --- a/app.min.js\n\
             +++ b/app.min.js\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n\
             \n\
             diff old.php\n\
             --- a/old.php\n\
             +++ b/old.php\n\
             @@ -1 +1 @@\n\
             -old\n\
             +new\n\
             \n"
        );
    }

    #[test]
    fn title_mentions_latest_version_only_when_outdated() {
        let title = |latest| {
            let report = report("test", "1.0", latest, vec![], &[], &[]);
            let out = rendered(|buf| render_summary(&report, buf));
            out.lines().next().unwrap().to_string()
        };
        assert_eq!(title(Some("2.0")), "wpdiff: test v1.0 (latest: v2.0)");
        assert_eq!(title(Some("1.0")), "wpdiff: test v1.0");
        assert_eq!(title(None), "wpdiff: test v1.0");
    }

    #[test]
    fn skipped_dirs_are_grouped_by_side() {
        let report = report(
            "test",
            "1.0",
            None,
            vec![],
            &["node_modules", "vendor"],
            &["external", "vendor"],
        );
        let out = rendered(|buf| render_summary(&report, buf));
        assert_eq!(
            out,
            "wpdiff: test v1.0\n\
             ── Skipped Directories ──\n\
             \x20 = vendor/\n\
             \x20 + node_modules/ (local only)\n\
             \x20 - external/ (upstream only)\n\
             \n\
             No differences found.\n"
        );
    }

    #[test]
    fn json_is_one_flat_object() {
        let files = vec![file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            1,
            1,
        )];
        let report = report("test", "1.0", Some("2.0"), files, &["vendor"], &[]);
        let out = rendered(|buf| render_json(&report, buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(
            parsed,
            serde_json::json!({
                "plugin_slug": "test",
                "plugin_version": "1.0",
                "latest_version": "2.0",
                "files": [{
                    "path": "a.php",
                    "status": "modified",
                    "category": "source",
                    "insertions": 1,
                    "deletions": 1,
                    "diff_text": "--- a/a.php\n+++ b/a.php\n@@ -1 +1 @@\n-old\n+new\n",
                }],
                "skipped_dirs": { "local": ["vendor"], "upstream": [] },
                "summary": {
                    "added": 0,
                    "removed": 0,
                    "modified": 1,
                    "unchanged": 5,
                    "by_category": { "source": { "added": 0, "removed": 0, "modified": 1 } },
                },
            })
        );
    }

    #[test]
    fn json_omits_latest_when_unknown() {
        let report = report("test", "1.0", None, vec![], &[], &[]);
        let out = rendered(|buf| render_json(&report, buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.get("latest_version").is_none());
    }

    #[test]
    fn scan_json_is_a_single_document() {
        let reports = vec![
            report("akismet", "5.0", None, vec![], &[], &[]),
            report("woo", "10.0", None, vec![], &[], &[]),
        ];
        let unmatched = vec![("private".to_string(), "not on wordpress.org".to_string())];
        let out = rendered(|buf| render_scan_json(&reports, 3, &unmatched, buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(parsed["changed"][0]["plugin_slug"], "akismet");
        assert_eq!(parsed["changed"][1]["summary"]["unchanged"], 5);
        assert_eq!(parsed["unchanged"], 3);
        assert_eq!(
            parsed["unmatched"],
            serde_json::json!([{ "plugin_slug": "private", "error": "not on wordpress.org" }])
        );
    }

    #[test]
    fn scan_json_of_a_clean_installation_is_still_a_document() {
        let out = rendered(|buf| render_scan_json(&[], 4, &[], buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!({ "changed": [], "unchanged": 4, "unmatched": [] })
        );
    }

    #[test]
    fn diffstat_scales_bars_to_the_largest_change() {
        let files = vec![
            file("a.php", FileStatus::Modified, FileCategory::Source, 6, 2),
            file("long/b.php", FileStatus::Added, FileCategory::Source, 16, 0),
            file("c.php", FileStatus::Removed, FileCategory::Source, 0, 1),
        ];
        let diff = DirDiff::new(files, SkippedDirs::default(), 5);
        // 3 + 10 (longest path) + 3 + 2 (digits) + 1 = 19 fixed columns, leaving 16 for the bar.
        let out = rendered(|buf| Ok(write_diffstat(&diff, 35, buf)?));
        assert_eq!(
            out,
            "\x20~ a.php      |  8 ++++++--\n\
             \x20+ long/b.php | 16 ++++++++++++++++\n\
             \x20- c.php      |  1 -\n\
             \n\
             \x203 files changed, 22 insertions(+), 3 deletions(-)\n\
             \x205 files unchanged\n"
        );
    }

    #[test]
    fn diffstat_falls_back_to_short_bars_on_narrow_terminals() {
        let files = vec![file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            30,
            10,
        )];
        let diff = DirDiff::new(files, SkippedDirs::default(), 0);
        let out = rendered(|buf| Ok(write_diffstat(&diff, 5, buf)?));
        assert_eq!(
            out,
            "\x20~ a.php | 40 +++++++---\n\
             \n\
             \x201 files changed, 30 insertions(+), 10 deletions(-)\n"
        );
    }

    #[test]
    fn summary_table_single_plugin_has_no_totals() {
        let files = vec![file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            5,
            3,
        )];
        let report = report("akismet", "5.0", Some("5.0"), files, &[], &[]);
        let out = rendered(|buf| render_summary_table(std::slice::from_ref(&report), 0, 0, buf));
        assert_eq!(
            out,
            "\x20Plugin   Ver    Added    Rm'd    Mod    Ins(+)    Del(-)\n\
             \x20akismet  v5.0       ·       ·      1        +5        -3\n\
             \n\
             \x201 plugins scanned: 1 changed, 0 unchanged, 0 unmatched\n"
        );
    }

    #[test]
    fn summary_table_totals_and_latest_column() {
        let reports = vec![
            report(
                "akismet",
                "5.0",
                Some("5.5.1"),
                vec![file(
                    "a.php",
                    FileStatus::Modified,
                    FileCategory::Source,
                    5,
                    3,
                )],
                &[],
                &[],
            ),
            report(
                "woo",
                "10.0",
                Some("10.0"),
                vec![
                    file("b.php", FileStatus::Added, FileCategory::Source, 10, 0),
                    file("c.php", FileStatus::Removed, FileCategory::Source, 0, 7),
                ],
                &[],
                &[],
            ),
        ];
        let out = rendered(|buf| render_summary_table(&reports, 3, 1, buf));
        assert_eq!(
            out,
            "\x20Plugin   Ver    Latest   Added    Rm'd    Mod    Ins(+)    Del(-)\n\
             \x20akismet  v5.0   v5.5.1       ·       ·      1        +5        -3\n\
             \x20woo      v10.0  ✓           +1      -1      ·       +10        -7\n\
             \x20                ──────  ──────  ──────  ─────  ────────  ────────\n\
             \x20Total                       +1      -1      1       +15       -10\n\
             \n\
             \x206 plugins scanned: 2 changed, 3 unchanged, 1 unmatched\n"
        );
    }

    #[test]
    fn summary_table_counts_plugins_even_when_none_changed() {
        let out = rendered(|buf| render_summary_table(&[], 2, 1, buf));
        assert_eq!(
            out,
            "\x20Plugin  Ver   Added    Rm'd    Mod    Ins(+)    Del(-)\n\
             \n\
             \x203 plugins scanned: 0 changed, 2 unchanged, 1 unmatched\n"
        );
    }

    #[test]
    fn summary_table_is_silent_when_nothing_was_scanned() {
        let out = rendered(|buf| render_summary_table(&[], 0, 0, buf));
        assert_eq!(out, "");
    }
}
