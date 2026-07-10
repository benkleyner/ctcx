use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ctcx::{
    BuildSafety, build_project, check_project, compile_project, discover_config, explain_rule,
    explain_target, init_project, load_project, render_diffs,
};
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "ctcx", version, about)]
struct Cli {
    /// Use this configuration instead of discovering ctcx.yaml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Control colored diagnostics.
    #[arg(long, global = true, default_value = "auto")]
    color: ColorChoice,

    /// Suppress success messages.
    #[arg(long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scaffold a new ctcx project and generate its context files.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Validate and compile the project in memory without writing files.
    Validate,
    /// Compile all configured context files.
    #[command(alias = "compile")]
    Build {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Check generated files and the manifest for drift.
    Check {
        #[arg(long)]
        target: Option<String>,
    },
    /// Show unified diffs against expected generated content.
    Diff {
        #[arg(long)]
        target: Option<String>,
    },
    /// Explain effective or suppressed rules.
    Explain {
        #[arg(long, conflicts_with = "rule")]
        target: Option<String>,
        #[arg(long, requires = "target")]
        slot: Option<String>,
        #[arg(long, conflicts_with_all = ["target", "slot"])]
        rule: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let color = cli.color;
    if let Err(error) = run(cli) {
        let use_color = match color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => std::io::stderr().is_terminal(),
        };
        if use_color {
            eprintln!("\x1b[31merror:\x1b[0m {error:#}");
        } else {
            eprintln!("error: {error:#}");
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Command::Init { force } = cli.command {
        let config = match cli.config {
            Some(path) => absolute(path)?,
            None => std::env::current_dir()?.join("ctcx.yaml"),
        };
        let written = init_project(&config, force)?;
        if !cli.quiet {
            for path in written {
                println!("created {}", path.display());
            }
        }
        return Ok(());
    }

    let config = match cli.config {
        Some(path) => absolute(path)?,
        None => discover_config(&std::env::current_dir()?)?,
    };
    let project = load_project(&config)?;
    let compiled = compile_project(&project)?;

    match cli.command {
        Command::Init { .. } => unreachable!(),
        Command::Validate => {
            if !cli.quiet {
                println!(
                    "valid: {} output(s), {} rule(s), {} dependency file(s)",
                    compiled.outputs.len(),
                    project.rules.len(),
                    project.dependencies.len()
                );
            }
        }
        Command::Build { dry_run, force } => {
            let safety = if force {
                BuildSafety::Force
            } else {
                BuildSafety::Safe
            };
            let changes = build_project(&project, &compiled, safety, dry_run)?;
            if !cli.quiet {
                if changes.is_empty() {
                    println!("up to date");
                } else {
                    for change in changes {
                        println!(
                            "{} {}",
                            if dry_run { "would update" } else { "updated" },
                            change.display()
                        );
                    }
                }
            }
        }
        Command::Check { target } => {
            let report = check_project(&project, &compiled, target.as_deref())?;
            if report.is_clean() {
                if !cli.quiet {
                    println!("generated context is up to date");
                }
            } else {
                bail!(report.to_string());
            }
        }
        Command::Diff { target } => {
            let diff = render_diffs(&project, &compiled, target.as_deref())?;
            if diff.is_empty() {
                if !cli.quiet {
                    println!("no differences");
                }
            } else {
                print!("{diff}");
                bail!("generated context differs");
            }
        }
        Command::Explain { target, slot, rule } => match (target, rule) {
            (Some(target), None) => print!(
                "{}",
                explain_target(&project, &compiled, &target, slot.as_deref())?
            ),
            (None, Some(rule)) => print!("{}", explain_rule(&project, &compiled, &rule)?),
            _ => bail!("explain requires either --target <output> or --rule <rule-id>"),
        },
    }

    Ok(())
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("failed to determine current directory")?
            .join(path))
    }
}
