mod diff;
mod output;
mod plugin;
mod progress;
mod source;
mod upgrade;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use colored::Colorize;
use diff::FileCategory;
use log::{LevelFilter, debug, info, warn};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

#[derive(Parser)]
#[command(
    name = "wpdiff",
    about = "Diff locally installed WordPress plugins against their upstream versions",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    common: CommonArgs,

    #[command(flatten)]
    filter: FilterArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Show a summary of changes (no diffs)
    Summary {
        #[command(flatten)]
        common: CommonArgs,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// Export changes as a .patch file
    Export {
        /// Output file path (default: <slug>-<version>.patch)
        #[arg(short, long)]
        output: Option<String>,
        #[command(flatten)]
        common: CommonArgs,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// List available versions from wordpress.org
    Versions {
        /// Plugin slug or path
        plugin: Option<String>,
        /// `WordPress` root directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Upgrade plugin and attempt to reapply local customizations
    Upgrade {
        /// Plugin slug or path
        plugin: Option<String>,
        /// `WordPress` root directory
        #[arg(short = 'C', long = "dir")]
        dir: Option<String>,
        /// Target version (default: latest)
        #[arg(long)]
        to: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Test if upgrade + patch would succeed without modifying anything
        #[arg(long)]
        dry_run: bool,
        /// Include whitespace and line ending changes
        #[arg(long)]
        whitespace: bool,
    },
}

#[derive(Parser, Clone)]
struct CommonArgs {
    /// Plugin slug or path to plugin directory
    plugin: Option<String>,

    /// Scan all plugins in a `WordPress` installation
    #[arg(long)]
    all: bool,

    /// `WordPress` root directory
    #[arg(short = 'C', long = "dir")]
    dir: Option<String>,

    /// Include whitespace and line ending changes
    #[arg(long)]
    whitespace: bool,

    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress all non-error output
    #[arg(short, long, conflicts_with = "verbose", global = true)]
    quiet: bool,
}

impl CommonArgs {
    fn base_dir(&self) -> Option<&Path> {
        self.dir.as_deref().map(Path::new)
    }

    fn require_plugin(&self) -> Result<&str> {
        self.plugin
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Provide a plugin slug or path, or use --all"))
    }
}

#[derive(Parser, Clone)]
struct FilterArgs {
    /// Include build artifacts (.min.js, vendor/, etc.)
    #[arg(long)]
    include_artifacts: bool,

    /// Include binary assets (images, fonts)
    #[arg(long)]
    include_assets: bool,

    /// Include all file categories (artifacts + assets)
    #[arg(long)]
    include_all: bool,

    /// Exclude files matching glob patterns (repeatable)
    #[arg(short = 'x', long = "exclude", value_name = "GLOB")]
    exclude: Vec<String>,

    /// Output as JSON instead of terminal format
    #[arg(long)]
    json: bool,
}

impl FilterArgs {
    fn included_categories(&self) -> HashSet<FileCategory> {
        let mut cats = HashSet::new();
        cats.insert(FileCategory::Source);
        cats.insert(FileCategory::Metadata);

        if self.include_all || self.include_artifacts {
            cats.insert(FileCategory::Artifact);
        }
        if self.include_all || self.include_assets {
            cats.insert(FileCategory::Asset);
        }

        cats
    }

    fn apply_filters(&self, result: &diff::DiffResult) -> diff::DiffResult {
        result.apply(&self.included_categories(), &self.exclude)
    }
}

fn main() {
    let cli = Cli::parse();

    let (verbose, quiet) = match &cli.command {
        Some(Command::Summary { common, .. } | Command::Export { common, .. }) => {
            (common.verbose, common.quiet)
        }
        Some(Command::Versions { .. } | Command::Upgrade { .. }) => {
            (cli.common.verbose, cli.common.quiet)
        }
        None => (cli.common.verbose, cli.common.quiet),
    };

    init_logger(verbose, quiet);

    if let Err(e) = run(cli) {
        log::error!("{e:#}");
        std::process::exit(1);
    }
}

fn init_logger(verbose: u8, quiet: bool) {
    let level = if quiet {
        LevelFilter::Error
    } else {
        match verbose {
            0 => LevelFilter::Warn,
            1 => LevelFilter::Info,
            2 => LevelFilter::Debug,
            _ => LevelFilter::Trace,
        }
    };

    env_logger::Builder::new()
        .filter_level(level)
        .format_timestamp(None)
        .format_target(false)
        .init();
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None => {
            if cli.common.plugin.is_none() && !cli.common.all {
                Cli::parse_from(["wpdiff", "--help"]);
                return Ok(());
            }
            render_command(&cli.common, &cli.filter, &RenderMode::Diff)
        }
        Some(Command::Summary { common, filter }) => {
            render_command(&common, &filter, &RenderMode::Summary)
        }
        Some(Command::Export {
            output,
            common,
            filter,
        }) => cmd_export(&common, &filter, output.as_deref()),
        Some(Command::Versions { plugin, dir, json }) => {
            cmd_versions(plugin.as_deref(), dir.as_deref().map(Path::new), json)
        }
        Some(Command::Upgrade {
            plugin,
            dir,
            to,
            yes,
            dry_run,
            whitespace,
        }) => upgrade::run(
            plugin.as_deref(),
            dir.as_deref().map(Path::new),
            to.as_deref(),
            yes,
            dry_run,
            whitespace,
        ),
    }
}

enum RenderMode {
    Diff,
    Summary,
}

fn render_output(
    result: &diff::DiffResult,
    mode: &RenderMode,
    json: bool,
    is_all: bool,
) -> Result<()> {
    let mut out = io::stdout().lock();
    if json {
        output::render_json(result, &mut out)?;
    } else {
        match (mode, is_all) {
            (RenderMode::Diff, _) => output::render_terminal(result, &mut out)?,
            (RenderMode::Summary, true) => output::render_summary_compact(result, &mut out)?,
            (RenderMode::Summary, false) => output::render_summary(result, &mut out)?,
        }
    }
    Ok(())
}

fn render_command(common: &CommonArgs, filter: &FilterArgs, mode: &RenderMode) -> Result<()> {
    if common.all {
        let is_summary = matches!(mode, RenderMode::Summary);
        return run_all(common, filter, is_summary, |filtered| {
            render_output(filtered, mode, filter.json, true)
        });
    }

    let result = resolve_and_diff(common)?;
    let filtered = filter.apply_filters(&result);
    render_output(&filtered, mode, filter.json, false)
}

fn cmd_versions(plugin_arg: Option<&str>, base_dir: Option<&Path>, json: bool) -> Result<()> {
    let plugin_arg = plugin_arg.ok_or_else(|| anyhow::anyhow!("Provide a plugin slug"))?;

    let slug = resolve_slug(plugin_arg, base_dir);

    let installed_version = plugin::resolve_plugin_path(plugin_arg, base_dir)
        .ok()
        .and_then(|p| plugin::discover_plugin(&p).ok())
        .map(|m| m.version);

    info!("Fetching versions for {slug}...");
    let info = source::fetch_plugin_versions(&slug)?;

    if json {
        let json = serde_json::json!({
            "slug": slug,
            "name": info.name,
            "latest": info.latest_version,
            "installed": installed_version,
            "versions": info.versions,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    println!("{} {}", info.name.bold(), format!("({slug})").dimmed());

    if let Some(ref installed) = installed_version {
        println!("  Installed: {}", format!("v{installed}").yellow().bold());
    }
    println!(
        "  Latest:    {}",
        format!("v{}", info.latest_version).green().bold()
    );

    println!("\n  {}", "Available versions:".dimmed());

    let display_count = 20;
    let total = info.versions.len();
    let start = total.saturating_sub(display_count);

    if start > 0 {
        println!("  {} ({} older versions hidden)", "...".dimmed(), start);
    }

    for version in &info.versions[start..] {
        let is_installed = installed_version.as_deref() == Some(version.as_str());
        let is_latest = version == &info.latest_version;

        let marker = if is_installed && is_latest {
            format!("{}", "◀ installed, latest".green())
        } else if is_installed {
            format!("{}", "◀ installed".yellow())
        } else if is_latest {
            format!("{}", "◀ latest".green())
        } else {
            String::new()
        };

        let ver_display = if is_installed {
            format!("v{version}").yellow().bold().to_string()
        } else if is_latest {
            format!("v{version}").green().to_string()
        } else {
            format!("v{version}")
        };

        println!("    {ver_display} {marker}");
    }

    println!("\n  {total} versions total");

    Ok(())
}

fn resolve_slug(plugin_arg: &str, base_dir: Option<&Path>) -> String {
    plugin::resolve_plugin_path(plugin_arg, base_dir).map_or_else(
        |_| {
            plugin_arg
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(plugin_arg)
                .to_string()
        },
        |path| {
            path.file_name().map_or_else(
                || plugin_arg.to_string(),
                |n| n.to_string_lossy().to_string(),
            )
        },
    )
}

fn cmd_export(common: &CommonArgs, filter: &FilterArgs, output_path: Option<&str>) -> Result<()> {
    let result = resolve_and_diff(common)?;
    let filtered = filter.apply_filters(&result);

    if filtered.files.is_empty() {
        bail!("No differences to export");
    }

    let path = match output_path {
        Some(p) => p.to_string(),
        None => format!("{}-{}.patch", filtered.plugin_slug, filtered.plugin_version),
    };

    let file =
        fs::File::create(&path).with_context(|| format!("Failed to create patch file {path}"))?;
    let mut writer = io::BufWriter::new(file);
    output::render_unified(&filtered, &mut writer)?;

    info!("Patch exported to {path}");
    Ok(())
}

fn resolve_and_diff(common: &CommonArgs) -> Result<diff::DiffResult> {
    let plugin_arg = common.require_plugin()?;

    debug!("Resolving plugin: {plugin_arg}");
    let plugin_path = plugin::resolve_plugin_path(plugin_arg, common.base_dir())?;
    let mut result = diff_single_plugin(&plugin_path, common.whitespace)?;
    result.latest_version = source::fetch_plugin_versions(&result.plugin_slug)
        .ok()
        .map(|info| info.latest_version);
    Ok(result)
}

fn diff_single_plugin(plugin_path: &Path, strict_whitespace: bool) -> Result<diff::DiffResult> {
    let registry = source::Registry::new();
    diff_plugin_with_registry(&registry, plugin_path, strict_whitespace)
}

fn diff_plugin_with_registry(
    registry: &source::Registry,
    plugin_path: &Path,
    strict_whitespace: bool,
) -> Result<diff::DiffResult> {
    let meta = plugin::discover_plugin(plugin_path)?;

    info!(
        "Comparing {} v{} against upstream...",
        meta.name, meta.version
    );
    debug!("Plugin directory: {}", meta.dir.display());
    debug!("Main file: {}", meta.main_file.display());

    let upstream = registry.fetch(&meta, None)?;

    debug!("Upstream extracted to: {}", upstream.path.display());

    diff::diff_directories(
        &meta.dir,
        &upstream.path,
        &meta.slug,
        &meta.version,
        strict_whitespace,
    )
}

fn run_all(
    common: &CommonArgs,
    filter: &FilterArgs,
    is_summary: bool,
    render: impl Fn(&diff::DiffResult) -> anyhow::Result<()>,
) -> Result<()> {
    use rayon::prelude::*;

    let base = common.dir.as_deref().unwrap_or(".");
    debug!("Scanning all plugins in {base}");

    let discovered = plugin::discover_all(Path::new(base))?;

    let plugins: Vec<plugin::PluginMeta> = discovered
        .into_iter()
        .filter_map(|r| match r {
            Ok(meta) => Some(meta),
            Err(e) => {
                warn!("{e:#}");
                None
            }
        })
        .collect();

    debug!("Found {} plugins", plugins.len());

    let whitespace = common.whitespace;
    let registry = source::Registry::new();

    let overall = progress::bar(
        plugins.len() as u64,
        "  {spinner:.green} Scanning [{bar:30.cyan/dim}] {pos}/{len} plugins",
    );
    progress::suppress(true);

    let results: Vec<(String, Result<diff::DiffResult>)> = plugins
        .par_iter()
        .map(|meta| {
            let result = diff_plugin_with_registry(&registry, &meta.dir, whitespace);
            overall.inc(1);
            (meta.slug.clone(), result)
        })
        .collect();

    progress::suppress(false);
    overall.finish_and_clear();

    let version_pb = progress::bar(
        results.len() as u64,
        "  {spinner:.green} Checking versions [{bar:30.cyan/dim}] {pos}/{len}",
    );
    progress::suppress(true);

    let results: Vec<(String, Result<diff::DiffResult>)> = results
        .into_par_iter()
        .map(|(slug, result)| {
            let result = result.map(|mut r| {
                r.latest_version = source::fetch_plugin_versions(&slug)
                    .ok()
                    .map(|info| info.latest_version);
                r
            });
            version_pb.inc(1);
            (slug, result)
        })
        .collect();

    progress::suppress(false);
    version_pb.finish_and_clear();

    let mut sorted: Vec<(String, Result<diff::DiffResult>)> = results;
    sorted.sort_by(|(a, _), (b, _)| a.to_lowercase().cmp(&b.to_lowercase()));

    let mut diffs = Vec::new();
    let mut clean_count = 0usize;
    let mut unmatched = Vec::new();

    for (slug, result) in sorted {
        match result {
            Ok(diff_result) => {
                let filtered = filter.apply_filters(&diff_result);
                if filtered.files.is_empty() {
                    clean_count += 1;
                    debug!("{slug}: no differences");
                } else {
                    diffs.push(filtered);
                }
            }
            Err(e) => {
                unmatched.push((slug, format!("{e:#}")));
            }
        }
    }

    if is_summary && !filter.json {
        output::render_summary_table(
            &diffs,
            clean_count,
            unmatched.len(),
            &mut io::stdout().lock(),
        )?;
    } else if diffs.is_empty() && unmatched.is_empty() {
        info!("All plugins match their upstream versions.");
        return Ok(());
    } else {
        for diff in &diffs {
            render(diff)?;
        }
        if clean_count > 0 || !unmatched.is_empty() {
            let mut out = io::stdout().lock();
            writeln!(out)?;
            writeln!(
                out,
                "{} changed, {} unchanged, {} unmatched",
                diffs.len(),
                clean_count,
                unmatched.len()
            )?;
        }
    }

    if !unmatched.is_empty() {
        for (slug, reason) in &unmatched {
            warn!("{slug} - {reason}");
        }
    }

    Ok(())
}
