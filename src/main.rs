mod diff;
mod output;
mod plugin;
mod progress;
mod source;
mod upgrade;

use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, Parser, Subcommand};
use colored::Colorize;
use diff::{DirDiff, Filter};
use log::{LevelFilter, debug, info, warn};
use output::Report;
use plugin::PluginMeta;
use source::WpOrg;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "wpdiff",
    about = "Diff locally installed WordPress plugins against their upstream versions",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    // Options of the default diff, which subcommands inherit
    // (see `Command::inherit`).
    #[command(flatten)]
    diff: DiffArgs,

    /// `WordPress` root directory
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<PathBuf>,

    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress all non-error output
    #[arg(short, long, conflicts_with = "verbose", global = true)]
    quiet: bool,
}

#[derive(Args, Default)]
struct DiffArgs {
    /// Plugin slug or path to plugin directory
    plugin: Option<String>,

    /// Scan all plugins in a `WordPress` installation
    #[arg(long)]
    all: bool,

    /// Include whitespace and line ending changes
    #[arg(long)]
    whitespace: bool,

    #[command(flatten)]
    filter: FilterArgs,

    /// Output as JSON instead of terminal format
    #[arg(long)]
    json: bool,
}

#[derive(Args, Default)]
struct FilterArgs {
    /// Include build artifacts (.min.js, dist/, etc.)
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
}

#[derive(Subcommand)]
enum Command {
    /// Show a summary of changes (no diffs)
    Summary(DiffArgs),
    /// Export changes as a .patch file
    Export {
        /// Plugin slug or path to plugin directory
        plugin: Option<String>,
        /// Output file path (default: <slug>-<version>.patch)
        #[arg(short, long)]
        output: Option<String>,
        /// Include whitespace and line ending changes
        #[arg(long)]
        whitespace: bool,
        #[command(flatten)]
        filter: FilterArgs,
    },
    /// List available versions from wordpress.org
    Versions {
        /// Plugin slug or path
        plugin: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Upgrade plugin and attempt to reapply local customizations
    Upgrade {
        /// Plugin slug or path
        plugin: Option<String>,
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

impl DiffArgs {
    fn or(self, outer: Self) -> Self {
        Self {
            plugin: self.plugin.or(outer.plugin),
            all: self.all || outer.all,
            whitespace: self.whitespace || outer.whitespace,
            filter: self.filter.or(outer.filter),
            json: self.json || outer.json,
        }
    }

    /// The command-line name of an option that is set, if any; the plugin is
    /// not an option.
    fn set_option(&self) -> Option<&'static str> {
        [
            ("--all", self.all),
            ("--whitespace", self.whitespace),
            ("--include-artifacts", self.filter.include_artifacts),
            ("--include-assets", self.filter.include_assets),
            ("--include-all", self.filter.include_all),
            ("--exclude", !self.filter.exclude.is_empty()),
            ("--json", self.json),
        ]
        .into_iter()
        .find_map(|(option, set)| set.then_some(option))
    }
}

impl FilterArgs {
    fn or(self, outer: Self) -> Self {
        Self {
            include_artifacts: self.include_artifacts || outer.include_artifacts,
            include_assets: self.include_assets || outer.include_assets,
            include_all: self.include_all || outer.include_all,
            exclude: [outer.exclude, self.exclude].concat(),
        }
    }

    fn to_filter(&self) -> Filter {
        Filter::new(
            self.include_all || self.include_artifacts,
            self.include_all || self.include_assets,
            &self.exclude,
        )
    }
}

impl Command {
    /// Options given before the subcommand name also apply to it: flags and
    /// `-x` patterns combine, a plugin named after the subcommand wins. An
    /// option the subcommand has no use for is an error, as it is after the
    /// name.
    fn inherit(self, mut outer: DiffArgs) -> Result<Self, clap::Error> {
        use std::mem::take;

        let (name, command) = match self {
            Self::Summary(args) => ("summary", Self::Summary(args.or(take(&mut outer)))),
            Self::Export {
                plugin,
                output,
                whitespace,
                filter,
            } => (
                "export",
                Self::Export {
                    plugin: plugin.or_else(|| outer.plugin.take()),
                    output,
                    whitespace: whitespace || take(&mut outer.whitespace),
                    filter: filter.or(take(&mut outer.filter)),
                },
            ),
            Self::Versions { plugin, json } => (
                "versions",
                Self::Versions {
                    plugin: plugin.or_else(|| outer.plugin.take()),
                    json: json || take(&mut outer.json),
                },
            ),
            Self::Upgrade {
                plugin,
                to,
                yes,
                dry_run,
                whitespace,
            } => (
                "upgrade",
                Self::Upgrade {
                    plugin: plugin.or_else(|| outer.plugin.take()),
                    to,
                    yes,
                    dry_run,
                    whitespace: whitespace || take(&mut outer.whitespace),
                },
            ),
        };

        outer.set_option().map_or_else(
            || Ok(command),
            |option| {
                Err(Cli::command().error(
                    clap::error::ErrorKind::UnknownArgument,
                    format!("'{option}' cannot be used with '{name}'"),
                ))
            },
        )
    }
}

fn main() {
    let cli = Cli::parse();

    init_logger(cli.verbose, cli.quiet);

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
    let dir = cli.dir.as_deref();

    let Some(command) = cli.command else {
        if cli.diff.plugin.is_none() && !cli.diff.all {
            Cli::command().print_help()?;
            return Ok(());
        }
        return cmd_diff(&cli.diff, dir, Format::Diff);
    };

    match command.inherit(cli.diff).unwrap_or_else(|e| e.exit()) {
        Command::Summary(args) => cmd_diff(&args, dir, Format::Summary),
        Command::Export {
            plugin,
            output,
            whitespace,
            filter,
        } => {
            let plugin = plugin.context("Provide a plugin slug or path to export")?;
            cmd_export(
                &plugin,
                dir,
                whitespace,
                &filter.to_filter(),
                output.as_deref(),
            )
        }
        Command::Versions { plugin, json } => {
            let plugin = plugin.context("Provide a plugin slug")?;
            cmd_versions(&plugin, dir, json)
        }
        Command::Upgrade {
            plugin,
            to,
            yes,
            dry_run,
            whitespace,
        } => {
            let plugin = plugin.context("Provide a plugin slug or path to upgrade")?;
            let meta = plugin::locate(&plugin, dir)?;
            upgrade::run(
                &meta,
                &upgrade::Options {
                    target_version: to.as_deref(),
                    yes,
                    dry_run,
                    strict_whitespace: whitespace,
                },
            )
        }
    }
}

#[derive(Clone, Copy)]
enum Format {
    Diff,
    Summary,
    Json,
}

fn render(report: &Report, format: Format, out: &mut impl Write) -> Result<()> {
    match format {
        Format::Diff => output::render_terminal(report, out),
        Format::Summary => output::render_summary(report, out),
        Format::Json => output::render_json(report, out),
    }
}

fn cmd_diff(args: &DiffArgs, dir: Option<&Path>, view: Format) -> Result<()> {
    let format = if args.json { Format::Json } else { view };
    let filter = args.filter.to_filter();

    if !args.all {
        let plugin = args
            .plugin
            .as_deref()
            .context("Provide a plugin slug or path, or use --all")?;
        let meta = plugin::locate(plugin, dir)?;
        let report = compare(&WpOrg::new()?, &meta, args.whitespace, &filter)?;
        return render(&report, format, &mut io::stdout().lock());
    }

    let scan = scan_all(dir, args.whitespace, &filter)?;
    let mut out = io::stdout().lock();

    match format {
        Format::Summary => {
            output::render_summary_table(
                &scan.changed,
                scan.clean,
                scan.unmatched.len(),
                &mut out,
            )?;
        }
        Format::Json => {
            output::render_scan_json(&scan.changed, scan.clean, &scan.unmatched, &mut out)?;
        }
        Format::Diff => {
            for report in &scan.changed {
                output::render_terminal(report, &mut out)?;
            }
            output::render_scan_footer(
                scan.changed.len(),
                scan.clean,
                scan.unmatched.len(),
                &mut out,
            )?;
        }
    }

    for (slug, reason) in &scan.unmatched {
        warn!("{slug} - {reason}");
    }
    Ok(())
}

/// Diffs an installed plugin against the upstream package `WpOrg::download`
/// finds for its version.
fn diff_installed(
    wporg: &WpOrg,
    meta: &PluginMeta,
    strict_whitespace: bool,
    filter: &Filter,
) -> Result<DirDiff> {
    info!(
        "Comparing {} v{} against upstream...",
        meta.name, meta.version
    );
    debug!("Plugin directory: {}", meta.dir.display());

    let upstream = wporg.download(&meta.slug, &meta.version)?;
    debug!("Upstream extracted to: {}", upstream.path().display());

    Ok(diff::diff_directories(&meta.dir, upstream.path(), strict_whitespace)?.filtered(filter))
}

fn compare(
    wporg: &WpOrg,
    meta: &PluginMeta,
    strict_whitespace: bool,
    filter: &Filter,
) -> Result<Report> {
    let diff = diff_installed(wporg, meta, strict_whitespace, filter)?;
    // Without the lookup the report only lacks its "latest version" hint.
    let latest_version = wporg
        .plugin_info(&meta.slug)
        .ok()
        .map(|info| info.latest_version);

    Ok(Report {
        plugin_slug: meta.slug.clone(),
        plugin_version: meta.version.clone(),
        latest_version,
        diff,
    })
}

/// The outcome of comparing every plugin of an installation, ordered by slug.
#[derive(Default)]
struct Scan {
    changed: Vec<Report>,
    /// Number of plugins identical to upstream after filtering.
    clean: usize,
    /// Plugins that could not be compared, and why.
    unmatched: Vec<(String, String)>,
}

fn scan_all(dir: Option<&Path>, strict_whitespace: bool, filter: &Filter) -> Result<Scan> {
    use rayon::prelude::*;

    let mut plugins: Vec<PluginMeta> = plugin::discover_all(dir)?
        .into_iter()
        .filter_map(|discovered| discovered.inspect_err(|e| warn!("{e:#}")).ok())
        .collect();
    plugins.sort_by_key(|meta| meta.slug.to_lowercase());

    debug!("Found {} plugins", plugins.len());

    let wporg = WpOrg::new()?;
    let progress = progress::bar(
        plugins.len() as u64,
        "  {spinner:.green} Scanning [{bar:30.cyan/dim}] {pos}/{len} plugins",
    );
    let results: Vec<Result<Report>> = plugins
        .par_iter()
        .map(|meta| {
            let result = compare(&wporg, meta, strict_whitespace, filter);
            progress.inc(1);
            result
        })
        .collect();
    progress.finish_and_clear();

    let mut scan = Scan::default();
    for (meta, result) in plugins.into_iter().zip(results) {
        match result {
            Ok(report) if report.diff.files.is_empty() => {
                scan.clean += 1;
                debug!("{}: no differences", meta.slug);
            }
            Ok(report) => scan.changed.push(report),
            Err(e) => scan.unmatched.push((meta.slug, format!("{e:#}"))),
        }
    }
    Ok(scan)
}

fn cmd_export(
    plugin: &str,
    dir: Option<&Path>,
    strict_whitespace: bool,
    filter: &Filter,
    output_path: Option<&str>,
) -> Result<()> {
    let meta = plugin::locate(plugin, dir)?;
    let diff = diff_installed(&WpOrg::new()?, &meta, strict_whitespace, filter)?;

    if diff.files.is_empty() {
        bail!("No differences to export");
    }

    let path = output_path.map_or_else(
        || format!("{}-{}.patch", meta.slug, meta.version),
        str::to_string,
    );
    fs::write(&path, diff.patch())
        .with_context(|| format!("Failed to create patch file {path}"))?;

    info!("Patch exported to {path}");
    Ok(())
}

fn cmd_versions(plugin: &str, dir: Option<&Path>, json: bool) -> Result<()> {
    // The plugin does not have to be installed: an argument that resolves to
    // nothing on disk is taken as a wordpress.org slug.
    let installed_dir = plugin::resolve_plugin_path(plugin, dir).ok();
    let slug = installed_dir
        .as_deref()
        .and_then(Path::file_name)
        .map_or_else(
            || {
                let trimmed = plugin.trim_end_matches('/');
                let (_, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
                name.to_string()
            },
            |name| name.to_string_lossy().into_owned(),
        );
    let installed_version = installed_dir
        .and_then(|path| plugin::discover_plugin(&path).ok())
        .map(|meta| meta.version);

    info!("Fetching versions for {slug}...");
    let info = WpOrg::new()?.plugin_info(&slug)?;

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

    if let Some(installed) = &installed_version {
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
        let is_installed = installed_version.as_ref() == Some(version);
        let is_latest = version == &info.latest_version;

        let marker = match (is_installed, is_latest) {
            (true, true) => "◀ installed, latest".green(),
            (true, false) => "◀ installed".yellow(),
            (false, true) => "◀ latest".green(),
            (false, false) => "".normal(),
        };
        let label = format!("v{version}");
        let label = if is_installed {
            label.yellow().bold()
        } else if is_latest {
            label.green()
        } else {
            label.normal()
        };

        println!("    {label} {marker}");
    }

    println!("\n  {total} versions total");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from([&["wpdiff"][..], args].concat()).unwrap()
    }

    fn command(args: &[&str]) -> Command {
        let cli = parse(args);
        cli.command
            .expect("a subcommand")
            .inherit(cli.diff)
            .unwrap()
    }

    #[test]
    fn dir_applies_on_either_side_of_the_subcommand() {
        let wordpress = Some(PathBuf::from("/tmp/wordpress"));
        assert_eq!(
            parse(&["-C", "/tmp/wordpress", "summary", "--all"]).dir,
            wordpress
        );
        assert_eq!(
            parse(&["summary", "--all", "-C", "/tmp/wordpress"]).dir,
            wordpress
        );
        assert_eq!(
            parse(&["versions", "akismet", "-C", "/tmp/wordpress"]).dir,
            wordpress
        );
    }

    #[test]
    fn dir_after_the_subcommand_wins() {
        let cli = parse(&[
            "-C",
            "/tmp/global",
            "versions",
            "-C",
            "/tmp/command",
            "akismet",
        ]);
        assert_eq!(cli.dir, Some(PathBuf::from("/tmp/command")));
    }

    #[test]
    fn verbosity_applies_on_either_side_of_the_subcommand() {
        assert_eq!(parse(&["-vv", "upgrade", "akismet"]).verbose, 2);
        assert_eq!(parse(&["summary", "akismet", "-vv"]).verbose, 2);
        assert!(parse(&["export", "akismet", "-q"]).quiet);
    }

    #[test]
    fn summary_inherits_options_given_before_it() {
        let Command::Summary(args) = command(&[
            "--all",
            "--whitespace",
            "--json",
            "--include-assets",
            "-x",
            "outer",
            "summary",
            "-x",
            "inner",
        ]) else {
            panic!("expected summary command");
        };

        assert!(args.all && args.whitespace && args.json);
        assert!(args.filter.include_assets && !args.filter.include_artifacts);
        assert_eq!(args.filter.exclude, ["outer", "inner"]);
    }

    #[test]
    fn plugin_may_precede_the_subcommand_but_the_one_after_wins() {
        let Command::Summary(before) = command(&["akismet", "summary"]) else {
            panic!("expected summary command");
        };
        assert_eq!(before.plugin.as_deref(), Some("akismet"));

        let Command::Versions { plugin, .. } = command(&["akismet", "versions", "jetpack"]) else {
            panic!("expected versions command");
        };
        assert_eq!(plugin.as_deref(), Some("jetpack"));
    }

    #[test]
    fn export_inherits_filters_and_whitespace() {
        let Command::Export {
            plugin,
            whitespace,
            filter,
            ..
        } = command(&["--whitespace", "--include-all", "akismet", "export"])
        else {
            panic!("expected export command");
        };

        assert_eq!(plugin.as_deref(), Some("akismet"));
        assert!(whitespace && filter.include_all);
    }

    #[test]
    fn versions_inherits_json() {
        let Command::Versions { json, .. } = command(&["--json", "versions", "akismet"]) else {
            panic!("expected versions command");
        };
        assert!(json);
    }

    #[test]
    fn upgrade_inherits_whitespace() {
        let Command::Upgrade {
            whitespace,
            dry_run,
            ..
        } = command(&["--whitespace", "upgrade", "akismet", "--dry-run"])
        else {
            panic!("expected upgrade command");
        };
        assert!(whitespace && dry_run);
    }

    #[test]
    fn subcommands_reject_options_they_cannot_honour() {
        let rejects = |args: &[&str]| {
            Cli::try_parse_from([&["wpdiff"][..], args].concat())
                .is_err_and(|e| e.kind() == clap::error::ErrorKind::UnknownArgument)
        };
        assert!(rejects(&["export", "akismet", "--all"]));
        assert!(rejects(&["export", "akismet", "--json"]));
        assert!(rejects(&["versions", "akismet", "--whitespace"]));
        assert!(rejects(&["upgrade", "akismet", "--include-all"]));
    }

    #[test]
    fn subcommands_reject_those_options_before_their_name_too() {
        let assert_rejects = |args: &[&str], option: &str| {
            let cli = parse(args);
            let Err(err) = cli.command.expect("a subcommand").inherit(cli.diff) else {
                panic!("{args:?} was accepted");
            };
            assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
            assert!(err.to_string().contains(&format!("'{option}' cannot")));
        };
        assert_rejects(&["--all", "export", "akismet"], "--all");
        assert_rejects(&["--json", "export", "akismet"], "--json");
        assert_rejects(&["--whitespace", "versions", "akismet"], "--whitespace");
        assert_rejects(&["-x", "views", "versions", "akismet"], "--exclude");
        assert_rejects(
            &["--include-assets", "upgrade", "akismet"],
            "--include-assets",
        );
        assert_rejects(&["--json", "upgrade", "akismet"], "--json");
    }
}
