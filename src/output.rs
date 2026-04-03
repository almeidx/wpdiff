use crate::diff::{DiffResult, FileCategory, FileDiff, FileStatus};
use colored::Colorize;
use std::io::Write;

pub fn render_terminal(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(
        out,
        "{}",
        format!(
            "wpdiff: {} v{}",
            result.plugin_slug, result.plugin_version
        )
        .bold()
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
        let cat_files: Vec<&FileDiff> = result.files.iter().filter(|f| f.category == *cat).collect();
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
        for line in file.diff_text.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                writeln!(out, "{}", line.green())?;
            } else if line.starts_with('-') && !line.starts_with("---") {
                writeln!(out, "{}", line.red())?;
            } else if line.starts_with("@@") {
                writeln!(out, "{}", line.cyan())?;
            } else {
                writeln!(out, "{line}")?;
            }
        }
        writeln!(out)?;
    }

    Ok(())
}

fn render_skipped_dirs(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    let local_only: Vec<&str> = result.skipped_dirs.local.iter()
        .filter(|d| !result.skipped_dirs.upstream.contains(d))
        .map(std::string::String::as_str)
        .collect();
    let upstream_only: Vec<&str> = result.skipped_dirs.upstream.iter()
        .filter(|d| !result.skipped_dirs.local.contains(d))
        .map(std::string::String::as_str)
        .collect();
    let both: Vec<&str> = result.skipped_dirs.local.iter()
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
        writeln!(out, "  {} {}/ {}", "+".green().bold(), dir, "(local only)".dimmed())?;
    }
    for dir in &upstream_only {
        writeln!(out, "  {} {}/ {}", "-".red().bold(), dir, "(upstream only)".dimmed())?;
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

    let cw = [6, 6, 5, 8, 8]; // column widths: Added, Rm'd, Mod, Ins(+), Del(-)

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
        cw0 = cw[0], cw1 = cw[1], cw2 = cw[2], cw3 = cw[3], cw4 = cw[4],
    )?;

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

        writeln!(
            out,
            " {:<name_w$}  {:<ver_w$}  {}  {}  {}  {}  {}",
            result.plugin_slug.bold(),
            format!("v{}", result.plugin_version).dimmed(),
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
        writeln!(out, " {:<name_w$}  {:<ver_w$}  {}  {}  {}  {}  {}",
            "".dimmed(),
            "".dimmed(),
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
            " {:<name_w$}  {:<ver_w$}  {}  {}  {}  {}  {}",
            "Total".bold(),
            "",
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
    terminal_size::terminal_size()
        .map_or(80, |(w, _)| w.0 as usize)
}

pub fn render_summary(result: &DiffResult, out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(
        out,
        "{}",
        format!("wpdiff: {} v{}", result.plugin_slug, result.plugin_version).bold()
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
