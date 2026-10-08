//! The Linux leg: build on the host, run the agent in a rootless podman container.

use crate::{Opts, repo_root, status};
use anyhow::{Result, ensure};
use std::process::{Command, ExitCode};

pub fn run(o: &Opts) -> Result<ExitCode> {
    let Opts {
        tier,
        scenario,
        keep,
        update_baseline,
    } = o;
    let (tier, scenario, keep, update_baseline) =
        (tier.clone(), scenario.clone(), *keep, *update_baseline);
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
