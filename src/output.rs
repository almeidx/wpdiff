use crate::diff::{DiffResult, FileCategory, FileDiff, FileStatus};
use colored::Colorize;
use std::io::Write;

pub fn format_version_hint(result: &DiffResult) -> String {
    match &result.latest_version {
        Some(lv) if lv != &result.plugin_version => {
            format!(" (latest: {})", format!("v{lv}").green())
        }
        _ => String::new(),
    }
}

pub fn write_colored_diff(
    out: &mut impl Write,
    diff_text: &str,
    indent: &str,
) -> anyhow::Result<()> {
    for line in diff_text.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            writeln!(out, "{indent}{}", line.green())?;
        } else if line.starts_with('-') && !line.starts_with("---") {
            writeln!(out, "{indent}{}", line.red())?;
        } else if line.starts_with("@@") {
            writeln!(out, "{indent}{}", line.cyan())?;
        } else {
            writeln!(out, "{indent}{line}")?;
        }
    }
    Ok(())
}

pub fn render_terminal(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(
        out,
        "{}{}",
        format!("wpdiff: {} v{}", result.plugin_slug, result.plugin_version).bold(),
        format_version_hint(result),
    )?;
    writeln!(
        out,
        "  {} added, {} removed, {} modified, {} unchanged\n",
        result.summary.added.to_string().green(),
        result.summary.removed.to_string().red(),
        result.summary.modified.to_string().yellow(),
        result.summary.unchanged,
    )?;

    render_skipped_dirs(result, out)?;

    if result.files.is_empty() {
        writeln!(out, "{}", "No differences found.".green().bold())?;
        return Ok(());
    }

    let categories = [
        (FileCategory::Source, "Source Files"),
        (FileCategory::Artifact, "Build Artifacts"),
        (FileCategory::Asset, "Assets"),
        (FileCategory::Metadata, "Metadata"),
    ];

    for (cat, label) in &categories {
        let cat_files: Vec<&FileDiff> =
            result.files.iter().filter(|f| f.category == *cat).collect();
        if cat_files.is_empty() {
            continue;
        }

        writeln!(out, "{}", format!("── {label} ──").bold().dimmed())?;

        for file in &cat_files {
            let status_marker = match file.status {
                FileStatus::Added => "+".green().bold(),
                FileStatus::Removed => "-".red().bold(),
                FileStatus::Modified => "~".yellow().bold(),
            };
            writeln!(out, "  {} {}", status_marker, file.path)?;
        }
        writeln!(out)?;
    }

    for file in &result.files {
        if file.diff_text.is_empty() {
            continue;
        }

        writeln!(out, "{}", format!("diff {}", file.path).bold())?;
        write_colored_diff(out, &file.diff_text, "")?;
        writeln!(out)?;
    }

    Ok(())
}

fn render_skipped_dirs(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    let local_only: Vec<&str> = result
        .skipped_dirs
        .local
        .iter()
        .filter(|d| !result.skipped_dirs.upstream.contains(d))
        .map(std::string::String::as_str)
        .collect();
    let upstream_only: Vec<&str> = result
        .skipped_dirs
        .upstream
        .iter()
        .filter(|d| !result.skipped_dirs.local.contains(d))
        .map(std::string::String::as_str)
        .collect();
    let both: Vec<&str> = result
        .skipped_dirs
        .local
        .iter()
        .filter(|d| result.skipped_dirs.upstream.contains(d))
        .map(std::string::String::as_str)
        .collect();

    if local_only.is_empty() && upstream_only.is_empty() && both.is_empty() {
        return Ok(());
    }

    writeln!(out, "{}", "── Skipped Directories ──".bold().dimmed())?;
    for dir in &both {
        writeln!(out, "  {} {}/", "=".dimmed(), dir)?;
    }
    for dir in &local_only {
        writeln!(
            out,
            "  {} {}/ {}",
            "+".green().bold(),
            dir,
            "(local only)".dimmed()
        )?;
    }
    for dir in &upstream_only {
        writeln!(
            out,
            "  {} {}/ {}",
            "-".red().bold(),
            dir,
            "(upstream only)".dimmed()
        )?;
    }
    writeln!(out)?;

    Ok(())
}

pub fn render_unified(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    for file in &result.files {
        if !file.diff_text.is_empty() {
            write!(out, "{}", file.diff_text)?;
        }
    }
    Ok(())
}

pub fn render_json(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(out, result)?;
    Ok(())
}

pub fn render_summary_compact(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    render_summary_table(std::slice::from_ref(result), 0, 0, out)
}

fn col_green(val: usize, width: usize, prefix: &str) -> String {
    if val > 0 {
        format!("{:>w$}", format!("{}{}", prefix, val), w = width)
            .green()
            .to_string()
    } else {
        format!("{:>w$}", "·", w = width).dimmed().to_string()
    }
}

fn col_red(val: usize, width: usize, prefix: &str) -> String {
    if val > 0 {
        format!("{:>w$}", format!("{}{}", prefix, val), w = width)
            .red()
            .to_string()
    } else {
        format!("{:>w$}", "·", w = width).dimmed().to_string()
    }
}

fn col_yellow(val: usize, width: usize) -> String {
    if val > 0 {
        format!("{val:>width$}").yellow().to_string()
    } else {
        format!("{:>w$}", "·", w = width).dimmed().to_string()
    }
}

pub fn render_summary_table(
    results: &[DiffResult],
    clean_count: usize,
    unmatched_count: usize,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    if results.is_empty() && clean_count == 0 && unmatched_count == 0 {
        return Ok(());
    }

    let max_name = results
        .iter()
        .map(|r| r.plugin_slug.len())
        .max()
        .unwrap_or(6)
        .max(6);
    let max_ver = results
        .iter()
        .map(|r| r.plugin_version.len() + 1)
        .max()
        .unwrap_or(3)
        .max(3);

    let any_outdated = results.iter().any(|r| {
        r.latest_version
            .as_ref()
            .is_some_and(|lv| lv != &r.plugin_version)
    });
    let max_latest = if any_outdated {
        results
            .iter()
            .filter_map(|r| r.latest_version.as_ref())
            .map(|v| v.len() + 1)
            .max()
            .unwrap_or(0)
            .max(6)
    } else {
        0
    };

    let cw = [6, 6, 5, 8, 8];

    if any_outdated {
        writeln!(
            out,
            " {:<name_w$}  {:<ver_w$}  {:<lat_w$}  {:>cw0$}  {:>cw1$}  {:>cw2$}  {:>cw3$}  {:>cw4$}",
            "Plugin".bold().underline(),
            "Ver".bold().underline(),
            "Latest".bold().underline(),
            "Added".bold().underline(),
            "Rm'd".bold().underline(),
            "Mod".bold().underline(),
            "Ins(+)".bold().underline(),
            "Del(-)".bold().underline(),
            name_w = max_name,
            ver_w = max_ver,
            lat_w = max_latest,
            cw0 = cw[0],
            cw1 = cw[1],
            cw2 = cw[2],
            cw3 = cw[3],
            cw4 = cw[4],
        )?;
    } else {
        writeln!(
            out,
            " {:<name_w$}  {:<ver_w$}  {:>cw0$}  {:>cw1$}  {:>cw2$}  {:>cw3$}  {:>cw4$}",
            "Plugin".bold().underline(),
            "Ver".bold().underline(),
            "Added".bold().underline(),
            "Rm'd".bold().underline(),
            "Mod".bold().underline(),
            "Ins(+)".bold().underline(),
            "Del(-)".bold().underline(),
            name_w = max_name,
            ver_w = max_ver,
            cw0 = cw[0],
            cw1 = cw[1],
            cw2 = cw[2],
            cw3 = cw[3],
            cw4 = cw[4],
        )?;
    }

    let mut total_added = 0;
    let mut total_removed = 0;
    let mut total_modified = 0;
    let mut total_ins = 0;
    let mut total_del = 0;

    for result in results {
        let ins: usize = result.files.iter().map(|f| f.insertions).sum();
        let del: usize = result.files.iter().map(|f| f.deletions).sum();

        total_added += result.summary.added;
        total_removed += result.summary.removed;
        total_modified += result.summary.modified;
        total_ins += ins;
        total_del += del;

        let latest_col = if any_outdated {
            let s = match &result.latest_version {
                Some(lv) if lv != &result.plugin_version => {
                    format!("{:<lat_w$}", format!("v{lv}").green(), lat_w = max_latest)
                }
                _ => format!("{:<lat_w$}", "✓".dimmed(), lat_w = max_latest),
            };
            format!("  {s}")
        } else {
            String::new()
        };

        writeln!(
            out,
            " {:<name_w$}  {:<ver_w$}{}  {}  {}  {}  {}  {}",
            result.plugin_slug.bold(),
            format!("v{}", result.plugin_version).dimmed(),
            latest_col,
            col_green(result.summary.added, cw[0], "+"),
            col_red(result.summary.removed, cw[1], "-"),
            col_yellow(result.summary.modified, cw[2]),
            col_green(ins, cw[3], "+"),
            col_red(del, cw[4], "-"),
            name_w = max_name,
            ver_w = max_ver,
        )?;
    }

    if results.len() > 1 {
        let latest_spacer = if any_outdated {
            format!(
                "  {:<lat_w$}",
                "─".repeat(max_latest).dimmed(),
                lat_w = max_latest
            )
        } else {
            String::new()
        };
        let latest_spacer_empty = if any_outdated {
            format!("  {:<lat_w$}", "", lat_w = max_latest)
        } else {
            String::new()
        };
        writeln!(
            out,
            " {:<name_w$}  {:<ver_w$}{}  {}  {}  {}  {}  {}",
            "".dimmed(),
            "".dimmed(),
            latest_spacer,
            "─".repeat(cw[0]).dimmed(),
            "─".repeat(cw[1]).dimmed(),
            "─".repeat(cw[2]).dimmed(),
            "─".repeat(cw[3]).dimmed(),
            "─".repeat(cw[4]).dimmed(),
            name_w = max_name,
            ver_w = max_ver,
        )?;
        writeln!(
            out,
            " {:<name_w$}  {:<ver_w$}{}  {}  {}  {}  {}  {}",
            "Total".bold(),
            "",
            latest_spacer_empty,
            col_green(total_added, cw[0], "+"),
            col_red(total_removed, cw[1], "-"),
            col_yellow(total_modified, cw[2]),
            col_green(total_ins, cw[3], "+"),
            col_red(total_del, cw[4], "-"),
            name_w = max_name,
            ver_w = max_ver,
        )?;
    }

    let total_plugins = results.len() + clean_count + unmatched_count;
    writeln!(out)?;
    writeln!(
        out,
        " {} plugins scanned: {} changed, {} unchanged, {} unmatched",
        total_plugins.to_string().bold(),
        results.len().to_string().yellow(),
        clean_count.to_string().green(),
        if unmatched_count > 0 {
            unmatched_count.to_string().red().to_string()
        } else {
            unmatched_count.to_string()
        },
    )?;

    Ok(())
}

fn term_width() -> usize {
    terminal_size::terminal_size().map_or(80, |(w, _)| w.0 as usize)
}

pub fn render_summary(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(
        out,
        "{}{}",
        format!("wpdiff: {} v{}", result.plugin_slug, result.plugin_version).bold(),
        format_version_hint(result),
    )?;

    render_skipped_dirs(result, out)?;

    if result.files.is_empty() {
        writeln!(out, "{}", "No differences found.".green().bold())?;
        return Ok(());
    }

    let width = term_width();
    let max_path_len = result.files.iter().map(|f| f.path.len()).max().unwrap_or(0);
    let max_change_digits = result
        .files
        .iter()
        .map(|f| format!("{}", f.insertions + f.deletions).len())
        .max()
        .unwrap_or(1);
    let max_changes = result
        .files
        .iter()
        .map(|f| f.insertions + f.deletions)
        .max()
        .unwrap_or(1)
        .max(1);

    // " ~ path | 12345 +++---" => 3 (marker+spaces) + path + 3 ( | ) + digits + 1 (space) + bar
    let fixed_cols = 3 + max_path_len + 3 + max_change_digits + 1;
    let bar_width = if width > fixed_cols + 2 {
        width - fixed_cols
    } else {
        10
    };

    for file in &result.files {
        let total = file.insertions + file.deletions;
        let bar_total = if max_changes > 0 {
            (total * bar_width / max_changes).max(usize::from(total > 0))
        } else {
            0
        };
        let bar_ins = if total > 0 {
            (file.insertions * bar_total / total).max(usize::from(file.insertions > 0))
        } else {
            0
        };
        let bar_del = bar_total.saturating_sub(bar_ins);

        let status_marker = match file.status {
            FileStatus::Added => "+".green().bold(),
            FileStatus::Removed => "-".red().bold(),
            FileStatus::Modified => "~".yellow().bold(),
        };

        let bar = format!(
            "{}{}",
            "+".repeat(bar_ins).green(),
            "-".repeat(bar_del).red(),
        );

        writeln!(
            out,
            " {} {:path_w$} | {:>num_w$} {}",
            status_marker,
            file.path,
            total,
            bar,
            path_w = max_path_len,
            num_w = max_change_digits,
        )?;
    }

    let total_ins: usize = result.files.iter().map(|f| f.insertions).sum();
    let total_del: usize = result.files.iter().map(|f| f.deletions).sum();

    writeln!(out)?;
    writeln!(
        out,
        " {} files changed, {} insertions(+), {} deletions(-)",
        result.files.len().to_string().bold(),
        total_ins.to_string().green(),
        total_del.to_string().red(),
    )?;

    if result.summary.unchanged > 0 {
        writeln!(out, " {} files unchanged", result.summary.unchanged)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffSummary, SkippedDirs};
    use std::collections::BTreeMap;

    fn make_result(
        slug: &str,
        version: &str,
        latest: Option<&str>,
        files: Vec<FileDiff>,
        skipped_local: Vec<&str>,
        skipped_upstream: Vec<&str>,
    ) -> DiffResult {
        let mut added = 0;
        let mut removed = 0;
        let mut modified = 0;
        let mut by_category = BTreeMap::new();

        for f in &files {
            crate::diff::CategorySummary::tally(&mut by_category, f.category, f.status);
            match f.status {
                FileStatus::Added => added += 1,
                FileStatus::Removed => removed += 1,
                FileStatus::Modified => modified += 1,
            }
        }

        DiffResult {
            plugin_slug: slug.to_string(),
            plugin_version: version.to_string(),
            latest_version: latest.map(String::from),
            files,
            skipped_dirs: SkippedDirs {
                local: skipped_local.into_iter().map(String::from).collect(),
                upstream: skipped_upstream.into_iter().map(String::from).collect(),
            },
            summary: DiffSummary {
                added,
                removed,
                modified,
                unchanged: 5,
                by_category,
            },
        }
    }

    fn make_file(
        path: &str,
        status: FileStatus,
        cat: FileCategory,
        ins: usize,
        del: usize,
    ) -> FileDiff {
        FileDiff {
            path: path.to_string(),
            status,
            category: cat,
            insertions: ins,
            deletions: del,
            diff_text: if ins > 0 || del > 0 {
                format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-old\n+new\n")
            } else {
                String::new()
            },
        }
    }

    fn output_to_string(f: impl FnOnce(&mut Vec<u8>) -> anyhow::Result<()>) -> String {
        colored::control::set_override(false);
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        colored::control::unset_override();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn version_hint_when_outdated() {
        colored::control::set_override(false);
        let result = make_result("test", "1.0", Some("2.0"), vec![], vec![], vec![]);
        let hint = format_version_hint(&result);
        colored::control::unset_override();
        assert!(hint.contains("v2.0"));
        assert!(hint.contains("latest"));
    }

    #[test]
    fn version_hint_when_current() {
        let result = make_result("test", "1.0", Some("1.0"), vec![], vec![], vec![]);
        assert!(format_version_hint(&result).is_empty());
    }

    #[test]
    fn version_hint_when_none() {
        let result = make_result("test", "1.0", None, vec![], vec![], vec![]);
        assert!(format_version_hint(&result).is_empty());
    }

    #[test]
    fn colored_diff_classifies_lines() {
        let diff = "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n context\n";
        let out = output_to_string(|buf| write_colored_diff(buf, diff, ""));
        assert!(out.contains("-old"));
        assert!(out.contains("+new"));
        assert!(out.contains("@@ -1 +1 @@"));
        assert!(out.contains(" context"));
    }

    #[test]
    fn colored_diff_with_indent() {
        let diff = "+added\n";
        let out = output_to_string(|buf| write_colored_diff(buf, diff, ">>"));
        assert!(out.starts_with(">>"));
    }

    #[test]
    fn terminal_no_diffs() {
        let result = make_result("akismet", "5.0", None, vec![], vec![], vec![]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("akismet"));
        assert!(out.contains("v5.0"));
        assert!(out.contains("No differences found"));
    }

    #[test]
    fn terminal_with_files() {
        let files = vec![make_file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            3,
            1,
        )];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("a.php"));
        assert!(out.contains("Source Files"));
        assert!(out.contains("diff a.php"));
    }

    #[test]
    fn terminal_groups_by_category() {
        let files = vec![
            make_file("src.php", FileStatus::Modified, FileCategory::Source, 1, 1),
            make_file(
                "app.min.js",
                FileStatus::Modified,
                FileCategory::Artifact,
                1,
                1,
            ),
        ];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("Source Files"));
        assert!(out.contains("Build Artifacts"));
    }

    #[test]
    fn terminal_shows_skipped_dirs() {
        let result = make_result("test", "1.0", None, vec![], vec!["node_modules"], vec![]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("Skipped Directories"));
        assert!(out.contains("node_modules"));
    }

    #[test]
    fn unified_outputs_raw_diff() {
        let files = vec![make_file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            1,
            1,
        )];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_unified(&result, buf));
        assert!(out.contains("--- a/a.php"));
        assert!(out.contains("+++ b/a.php"));
    }

    #[test]
    fn unified_skips_empty_diffs() {
        let files = vec![FileDiff {
            path: "empty.php".to_string(),
            status: FileStatus::Modified,
            category: FileCategory::Source,
            insertions: 0,
            deletions: 0,
            diff_text: String::new(),
        }];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_unified(&result, buf));
        assert!(out.is_empty());
    }

    #[test]
    fn json_is_valid() {
        let files = vec![make_file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            1,
            1,
        )];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_json(&result, buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["plugin_slug"], "test");
        assert!(parsed["files"].is_array());
    }

    #[test]
    fn json_includes_latest_version() {
        let result = make_result("test", "1.0", Some("2.0"), vec![], vec![], vec![]);
        let out = output_to_string(|buf| render_json(&result, buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["latest_version"], "2.0");
    }

    #[test]
    fn json_omits_latest_when_none() {
        let result = make_result("test", "1.0", None, vec![], vec![], vec![]);
        let out = output_to_string(|buf| render_json(&result, buf));
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.get("latest_version").is_none());
    }

    #[test]
    fn summary_no_diffs() {
        let result = make_result("akismet", "5.0", None, vec![], vec![], vec![]);
        let out = output_to_string(|buf| render_summary(&result, buf));
        assert!(out.contains("akismet"));
        assert!(out.contains("No differences found"));
    }

    #[test]
    fn summary_shows_diffstat() {
        let files = vec![
            make_file("a.php", FileStatus::Modified, FileCategory::Source, 10, 5),
            make_file("b.php", FileStatus::Added, FileCategory::Source, 20, 0),
        ];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_summary(&result, buf));
        assert!(out.contains("a.php"));
        assert!(out.contains("b.php"));
        assert!(out.contains("files changed"));
        assert!(out.contains("insertions"));
        assert!(out.contains("deletions"));
    }

    #[test]
    fn summary_shows_unchanged_count() {
        let files = vec![make_file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            1,
            1,
        )];
        let result = make_result("test", "1.0", None, files, vec![], vec![]);
        let out = output_to_string(|buf| render_summary(&result, buf));
        assert!(out.contains("5 files unchanged"));
    }

    #[test]
    fn summary_table_single() {
        let files = vec![make_file(
            "a.php",
            FileStatus::Modified,
            FileCategory::Source,
            5,
            3,
        )];
        let result = make_result("akismet", "5.0", None, files, vec![], vec![]);
        let out =
            output_to_string(|buf| render_summary_table(std::slice::from_ref(&result), 0, 0, buf));
        assert!(out.contains("akismet"));
        assert!(out.contains("v5.0"));
        assert!(out.contains("plugins scanned"));
    }

    #[test]
    fn summary_table_multiple_with_totals() {
        let r1 = make_result(
            "akismet",
            "5.0",
            None,
            vec![make_file(
                "a.php",
                FileStatus::Modified,
                FileCategory::Source,
                5,
                3,
            )],
            vec![],
            vec![],
        );
        let r2 = make_result(
            "woo",
            "10.0",
            None,
            vec![make_file(
                "b.php",
                FileStatus::Added,
                FileCategory::Source,
                10,
                0,
            )],
            vec![],
            vec![],
        );
        let results = vec![r1, r2];
        let out = output_to_string(|buf| render_summary_table(&results, 3, 1, buf));
        assert!(out.contains("Total"));
        assert!(out.contains("6 plugins scanned"));
        assert!(out.contains("2 changed"));
        assert!(out.contains("3 unchanged"));
        assert!(out.contains("1 unmatched"));
    }

    #[test]
    fn summary_table_shows_latest_column() {
        let r1 = make_result(
            "akismet",
            "5.0",
            Some("5.5"),
            vec![make_file(
                "a.php",
                FileStatus::Modified,
                FileCategory::Source,
                1,
                1,
            )],
            vec![],
            vec![],
        );
        let out =
            output_to_string(|buf| render_summary_table(std::slice::from_ref(&r1), 0, 0, buf));
        assert!(out.contains("Latest"));
        assert!(out.contains("v5.5"));
    }

    #[test]
    fn summary_table_hides_latest_when_current() {
        let r1 = make_result(
            "akismet",
            "5.0",
            Some("5.0"),
            vec![make_file(
                "a.php",
                FileStatus::Modified,
                FileCategory::Source,
                1,
                1,
            )],
            vec![],
            vec![],
        );
        let out =
            output_to_string(|buf| render_summary_table(std::slice::from_ref(&r1), 0, 0, buf));
        assert!(!out.contains("Latest"));
    }

    #[test]
    fn summary_table_empty() {
        let out = output_to_string(|buf| render_summary_table(&[], 0, 0, buf));
        assert!(out.is_empty());
    }

    #[test]
    fn skipped_dirs_both_sides() {
        let result = make_result("test", "1.0", None, vec![], vec!["vendor"], vec!["vendor"]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("vendor/"));
        assert!(!out.contains("local only"));
        assert!(!out.contains("upstream only"));
    }

    #[test]
    fn skipped_dirs_local_only() {
        let result = make_result("test", "1.0", None, vec![], vec!["node_modules"], vec![]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("node_modules/"));
        assert!(out.contains("local only"));
    }

    #[test]
    fn skipped_dirs_upstream_only() {
        let result = make_result("test", "1.0", None, vec![], vec![], vec!["vendor"]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(out.contains("vendor/"));
        assert!(out.contains("upstream only"));
    }

    #[test]
    fn skipped_dirs_none_shown() {
        let result = make_result("test", "1.0", None, vec![], vec![], vec![]);
        let out = output_to_string(|buf| render_terminal(&result, buf));
        assert!(!out.contains("Skipped Directories"));
    }
}
