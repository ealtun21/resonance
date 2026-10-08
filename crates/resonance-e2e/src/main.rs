//! `resonance-e2e run`: execute scenarios on this machine's live audio stack.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(about = "Resonance live-audio e2e agent (runs inside the e2e container/VM)")]
enum Cli {
    /// Run scenarios and write report.json + report.md into --out.
    Run {
        #[arg(long)]
        scenarios: PathBuf,
        #[arg(long)]
        baselines: PathBuf,
        #[arg(long)]
        daemon: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_enum, default_value_t = resonance_e2e::scenario::Tier::Quick)]
        tier: resonance_e2e::scenario::Tier,
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        update_baseline: bool,
    },
}

fn main() -> Result<ExitCode> {
    let Cli::Run {
        scenarios,
        baselines,
        daemon,
        out,
        tier,
        filter,
        update_baseline,
    } = Cli::parse();
    run(
        scenarios,
        baselines,
        daemon,
        out,
        tier,
        filter,
        update_baseline,
    )
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)] // mirrors the CLI fields 1:1
fn run(
    scenarios: PathBuf,
    baselines: PathBuf,
    daemon: PathBuf,
    out: PathBuf,
    tier: resonance_e2e::scenario::Tier,
    filter: Option<String>,
    update_baseline: bool,
) -> Result<ExitCode> {
    use resonance_e2e::{common::RunOpts, scenario};
    // This agent creates devices and changes the default sink: refuse to run
    // anywhere but the e2e container/VM, whose entrypoint sets this.
    anyhow::ensure!(
        std::env::var_os("RESONANCE_E2E_SANDBOX").is_some(),
        "refusing to run outside the e2e sandbox (RESONANCE_E2E_SANDBOX unset): \
         it would reconfigure this machine's audio"
    );
    let os = std::env::consts::OS;
    let all = scenario::load_dir(&scenarios)?;
    let selected = scenario::select(&all, tier, filter.as_deref(), os)
        .into_iter()
        .cloned()
        .collect();
    std::fs::create_dir_all(&out)?;
    let opts = RunOpts {
        scenarios: selected,
        tier,
        out_dir: out.clone(),
        daemon_bin: daemon,
        baselines_path: baselines,
        update_baseline,
    };
    let report = run_os(&opts, &scenarios)?;
    std::fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    let md = report.to_markdown();
    std::fs::write(out.join("report.md"), &md)?;
    println!("{md}");
    Ok(if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(target_os = "linux")]
fn run_os(
    opts: &resonance_e2e::common::RunOpts,
    _scenarios: &std::path::Path,
) -> Result<resonance_e2e::report::Report> {
    resonance_e2e::runner::run(opts)
}

#[cfg(windows)]
fn run_os(
    opts: &resonance_e2e::common::RunOpts,
    scenarios: &std::path::Path,
) -> Result<resonance_e2e::report::Report> {
    // `contrib/e2e/windows` sits next to `contrib/e2e/scenarios`.
    let scripts = scenarios.join("..").join("windows");
    resonance_e2e::windows::runner::run(opts, &scripts)
}

#[cfg(target_os = "macos")]
fn run_os(
    opts: &resonance_e2e::common::RunOpts,
    _scenarios: &std::path::Path,
) -> Result<resonance_e2e::report::Report> {
    // The CoreAudio helper (`audiodev`) is compiled into this directory by the guest provisioning.
    let dir = std::env::var_os("RESONANCE_E2E_AUDIODEV").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("e2e-bin"),
        PathBuf::from,
    );
    resonance_e2e::macos::runner::run(opts, &dir)
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn run_os(
    _: &resonance_e2e::common::RunOpts,
    _: &std::path::Path,
) -> Result<resonance_e2e::report::Report> {
    anyhow::bail!("resonance-e2e: unsupported OS")
}
