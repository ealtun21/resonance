//! Developer tasks for this host. `cargo xtask e2e` runs the live-audio suite
//! (spec: docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md).

// Developer tooling, never shipped: error/panic docs and similar lints are noise here.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::format_push_string
)]

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod linux;
mod macos;
mod vm;
mod windows;

#[derive(Parser)]
enum Cli {
    /// Build, then run the live-audio scenarios in their environments.
    E2e {
        #[command(subcommand)]
        cmd: Option<E2eCmd>,
        #[arg(long, value_delimiter = ',', default_value = "linux")]
        os: Vec<Os>,
        #[arg(long, default_value = "quick")]
        tier: String,
        /// Only scenarios whose id matches this glob (`*` wildcard).
        #[arg(long)]
        scenario: Option<String>,
        /// Leave the container running after a failure for inspection.
        #[arg(long)]
        keep: bool,
        /// Rewrite contrib/e2e/baselines/<os>.toml from this run.
        #[arg(long)]
        update_baseline: bool,
        /// Run the OS legs one after another instead of in parallel.
        #[arg(long)]
        serial: bool,
    },
}

#[derive(clap::Subcommand)]
enum E2eCmd {
    /// Build an OS's base image (VM guests only), or do nothing if it exists.
    Image { os: Os },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Os {
    Linux,
    Windows,
    Macos,
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the repo root")
        .to_path_buf()
}

pub fn status(cmd: &mut Command) -> Result<bool> {
    Ok(cmd
        .status()
        .with_context(|| format!("spawn {cmd:?}"))?
        .success())
}

/// The options every OS leg shares.
pub struct Opts {
    pub tier: String,
    pub scenario: Option<String>,
    pub keep: bool,
    pub update_baseline: bool,
}

fn main() -> Result<ExitCode> {
    match Cli::parse() {
        Cli::E2e {
            cmd: Some(E2eCmd::Image { os }),
            ..
        } => match os {
            Os::Windows => windows::image(),
            Os::Macos => {
                bail!("the macOS image needs a few GUI steps; follow contrib/e2e/macos/README.md")
            }
            Os::Linux => bail!("the Linux leg builds its container image on every run"),
        },
        Cli::E2e {
            os,
            tier,
            scenario,
            keep,
            update_baseline,
            serial,
            ..
        } => {
            let o = Opts {
                tier,
                scenario,
                keep,
                update_baseline,
            };
            let run = |os: Os| match os {
                Os::Linux => linux::run(&o),
                Os::Windows => windows::run(&o),
                Os::Macos => macos::run(&o),
            };
            // Each leg has its own run dir and a free ssh port, so the legs are independent.
            let codes: Vec<Result<ExitCode>> = if serial || os.len() < 2 {
                os.iter().map(|&os| run(os)).collect()
            } else {
                std::thread::scope(|s| {
                    let legs: Vec<_> = os.iter().map(|&os| s.spawn(move || run(os))).collect();
                    legs.into_iter()
                        .map(|h| h.join().unwrap_or_else(|_| bail!("an e2e leg panicked")))
                        .collect()
                })
            };
            let mut ok = true;
            for code in codes {
                ok &= code? == ExitCode::SUCCESS;
            }
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}
