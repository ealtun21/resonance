//! The macOS leg: boot a throwaway overlay of the base image (Intel macOS under
//! quickemu), build in the guest, run the agent inside the logged-in session
//! through launchd, pull the report back.

use crate::vm::{self, Guest};
use crate::{Opts, repo_root};
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const NAME: &str = "e2e-macos";
const USER: &str = "e2e";

fn base_dir() -> PathBuf {
    vm::home().join("macos/base")
}

fn make_overlay(run: &Path) -> Result<()> {
    let base = base_dir();
    let disk = base.join("disk.qcow2");
    ensure!(
        disk.exists(),
        "no macOS base image at {}; see contrib/e2e/macos/README.md",
        disk.display()
    );
    let vmdir = run.join(NAME);
    std::fs::create_dir_all(&vmdir)?;
    vm::qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-F",
        "qcow2",
        "-b",
        &disk.to_string_lossy(),
        &vmdir.join("disk.qcow2").to_string_lossy(),
    ])?;
    // Small mutable boot files are copied. quickemu attaches the recovery image whenever the
    // overlay "looks unused" (it is a small file), and OpenCore would then boot Recovery; an
    // empty placeholder satisfies quickemu and leaves the installed disk as the only bootable entry.
    for f in ["OpenCore.qcow2", "OVMF_CODE.fd", "OVMF_VARS-1920x1080.fd"] {
        std::fs::copy(base.join(f), vmdir.join(f))?;
    }
    std::fs::File::create(vmdir.join("RecoveryImage.img"))?.set_len(1 << 20)?;
    std::fs::write(
        run.join(format!("{NAME}.conf")),
        format!(
            "#!/usr/bin/quickemu --vm\nguest_os=\"macos\"\ndisk_img=\"{NAME}/disk.qcow2\"\n\
             img=\"{NAME}/RecoveryImage.img\"\nmacos_release=\"sequoia\"\ncpu_cores=\"4\"\nram=\"8G\"\n"
        ),
    )?;
    Ok(())
}

/// Run `prepare.sh` (build + app bundle + LaunchAgents) and fail with its log.
fn prepare(g: &Guest) -> Result<()> {
    let ok = g.run("bash $HOME/resonance/contrib/e2e/macos/prepare.sh > $HOME/prepare.log 2>&1")?;
    let log = g.output("tail -40 $HOME/prepare.log")?;
    ensure!(
        ok && log.contains("PREPARE-DONE"),
        "guest prepare failed:\n{log}"
    );
    Ok(())
}

pub fn run(o: &Opts) -> Result<ExitCode> {
    let root = repo_root();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let run_dir = vm::home().join(format!("macos/run-{stamp}"));
    make_overlay(&run_dir)?;
    let out = root.join(format!("target/e2e/run-{stamp}/macos"));
    println!("macos: booting {}", run_dir.display());
    let g = Guest::boot(&run_dir, NAME, USER)?;
    let result = (|| -> Result<bool> {
        g.wait_ssh(Duration::from_secs(900))?;
        g.run("mkdir -p $HOME/resonance")?;
        g.sync_source(&root, "$HOME/resonance")?;
        prepare(&g)?;
        // The agent must live in the GUI session: write its command line, then start it
        // through launchd. The daemon agent is only registered; the agent starts it.
        let mut cmd = format!(
            "RESONANCE_E2E_SANDBOX=1 RESONANCE_E2E_AUDIODEV=$HOME/e2e-bin $HOME/e2e-bin/resonance-e2e run \
             --scenarios $HOME/resonance/contrib/e2e/scenarios \
             --baselines $HOME/resonance/contrib/e2e/baselines/macos.toml \
             --daemon /Applications/Resonance.app/Contents/MacOS/resonanced \
             --out $HOME/e2e-out --tier {}",
            o.tier
        );
        if let Some(f) = &o.scenario {
            cmd.push_str(&format!(" --filter '{f}'"));
        }
        if o.update_baseline {
            cmd.push_str(" --update-baseline");
        }
        ensure!(
            g.run(&format!(
                "rm -rf $HOME/e2e-out $HOME/e2e-exit && printf '%s\\n' '#!/bin/bash' '{} > $HOME/e2e-agent.log 2>&1' 'echo $? > $HOME/e2e-exit' > $HOME/e2e-run.sh",
                cmd.replace('\'', "'\\''")
            ))?,
            "could not write the run script"
        );
        ensure!(
            g.run(
                "uid=$(id -u); for l in daemon agent; do launchctl bootout gui/$uid/e2e.$l 2>/dev/null; \
                 launchctl bootstrap gui/$uid $HOME/Library/LaunchAgents/e2e.$l.plist; done; \
                 launchctl kickstart -k gui/$uid/e2e.agent"
            )?,
            "could not start the agent through launchd (is the e2e user logged in?)"
        );
        let end = Instant::now() + Duration::from_secs(60 * 120);
        let code = loop {
            let c = g.output("cat $HOME/e2e-exit 2>/dev/null")?;
            if !c.trim().is_empty() {
                break c.trim().to_string();
            }
            ensure!(Instant::now() < end, "agent timed out");
            std::thread::sleep(Duration::from_secs(10));
        };
        println!("{}", g.output("cat $HOME/e2e-agent.log")?);
        g.pull("$HOME/e2e-out", &out)?;
        if o.update_baseline {
            let tmp = root
                .join("target/e2e")
                .join(format!("baselines-macos-{stamp}"));
            g.pull("$HOME/resonance/contrib/e2e/baselines", &tmp)?;
            std::fs::copy(
                tmp.join("macos.toml"),
                root.join("contrib/e2e/baselines/macos.toml"),
            )?;
        }
        Ok(code == "0")
    })();
    let passed = *result.as_ref().unwrap_or(&false);
    println!("report: {}", out.join("report.md").display());
    if o.keep && !passed {
        println!(
            "guest kept running in {} (ssh key {})",
            run_dir.display(),
            vm::ssh_key().display()
        );
    } else {
        g.shutdown("sudo shutdown -h now");
        let _ = std::fs::remove_dir_all(&run_dir);
    }
    result.map(|ok| {
        if ok {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    })
}
