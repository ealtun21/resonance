//! Developer tasks for this host. `cargo xtask e2e` runs the live-audio suite
//! (spec: docs/superpowers/specs/2026-10-06-e2e-audio-test-infra-design.md).

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Parser)]
enum Cli {
    /// Build, then run the live-audio scenarios in their environments.
    E2e {
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
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Os {
    Linux,
    Windows,
    Macos,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the repo root")
        .to_path_buf()
}

fn status(cmd: &mut Command) -> Result<bool> {
    Ok(cmd
        .status()
        .with_context(|| format!("spawn {cmd:?}"))?
        .success())
}

fn main() -> Result<ExitCode> {
    let Cli::E2e {
        os,
        tier,
        scenario,
        keep,
        update_baseline,
    } = Cli::parse();
    if os.iter().any(|o| *o != Os::Linux) {
        bail!("only --os linux exists so far (Windows: M2, macOS: M3)");
    }
    let root = repo_root();
    ensure!(
        status(Command::new("cargo").current_dir(&root).args([
            "build",
            "--profile",
            "e2e-build",
            "-p",
            "resonance-daemon",
            "-p",
            "resonance-e2e",
        ]))?,
        "build failed"
    );
    ensure!(
        status(
            Command::new("podman")
                .args(["build", "-q", "-t", "resonance-e2e-linux"])
                .arg(root.join("contrib/e2e/linux"))
        )?,
        "container image build failed"
    );
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let out = root.join(format!("target/e2e/run-{stamp}/linux"));
    std::fs::create_dir_all(&out)?;
    let name = format!("resonance-e2e-{stamp}");
    let contrib_mode = if update_baseline { "rw" } else { "ro" };
    let mut agent = vec![
        "/e2e/bin/resonance-e2e".to_string(),
        "run".into(),
        "--scenarios".into(),
        "/e2e/contrib/scenarios".into(),
        "--baselines".into(),
        "/e2e/contrib/baselines/linux.toml".into(),
        "--daemon".into(),
        "/e2e/bin/resonanced".into(),
        "--out".into(),
        "/e2e/out".into(),
        "--tier".into(),
        tier,
    ];
    if let Some(g) = scenario {
        agent.extend(["--filter".into(), g]);
    }
    if update_baseline {
        agent.push("--update-baseline".into());
    }
    // With --keep the container stays up after the agent exits (code in a file).
    let script = format!(
        "{}; echo $? > /e2e/out/exit-code; [ \"$(cat /e2e/out/exit-code)\" = 0 ] || [ \"{keep}\" = false ] || exec sleep infinity",
        agent.join(" ")
    );
    let mut run = Command::new("podman");
    run.args(["run", "--name", &name, "--security-opt", "label=disable"])
        .arg("-v")
        .arg(format!(
            "{}:/e2e/bin:ro",
            root.join("target/e2e-build").display()
        ))
        .arg("-v")
        .arg(format!(
            "{}:/e2e/contrib:{contrib_mode}",
            root.join("contrib/e2e").display()
        ))
        .arg("-v")
        .arg(format!("{}:/e2e/out", out.display()));
    if keep {
        run.arg("-d");
    } else {
        run.arg("--rm");
    }
    run.args(["resonance-e2e-linux", "sh", "-c", &script]);
    let ok = status(&mut run)?;
    if keep {
        // Detached: wait for the agent's exit code to appear.
        while !out.join("exit-code").exists() {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    let code = std::fs::read_to_string(out.join("exit-code")).unwrap_or_default();
    let passed = (keep || ok) && code.trim() == "0";
    println!("\nreport: {}", out.join("report.md").display());
    if keep {
        if passed {
            let _ = status(Command::new("podman").args(["rm", "-f", &name]));
        } else {
            println!("container kept: podman exec -it {name} bash   (remove: podman rm -f {name})");
        }
    }
    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
