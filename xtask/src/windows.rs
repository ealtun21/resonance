//! The Windows leg: boot a throwaway overlay of the base image, build and
//! install in the guest, run the agent, pull the report back.

use crate::vm::{self, Guest};
use crate::{Opts, repo_root, status};
use anyhow::{Result, ensure};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

const NAME: &str = "e2e-windows";
const USER: &str = "Quickemu";

fn base_dir() -> PathBuf {
    vm::home().join("windows/base")
}

fn stamp() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}

/// A run directory holding the quickemu conf, an overlay disk on the base
/// image and a private copy of the UEFI variables.
fn make_overlay(run: &Path) -> Result<()> {
    let base = base_dir();
    let disk = base.join("disk.qcow2");
    ensure!(
        disk.exists(),
        "no Windows base image at {}; build it with `cargo xtask e2e image windows`",
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
    std::fs::copy(base.join("OVMF_VARS.fd"), vmdir.join("OVMF_VARS.fd"))?;
    // quickemu insists on boot media when the disk file "looks unused" (small overlay):
    // an empty placeholder keeps it quiet, and UEFI falls through to the disk.
    std::fs::File::create(vmdir.join("boot.iso"))?.set_len(1 << 20)?;
    std::fs::write(
        run.join(format!("{NAME}.conf")),
        format!(
            "#!/usr/bin/quickemu --vm\nguest_os=\"windows\"\ndisk_img=\"{NAME}/disk.qcow2\"\n\
             iso=\"{NAME}/boot.iso\"\ntpm=\"off\"\nsecureboot=\"off\"\ncpu_cores=\"8\"\nram=\"8G\"\n"
        ),
    )?;
    Ok(())
}

/// Start `prepare.ps1` as SYSTEM and wait for its log to say it is done.
fn prepare(g: &Guest) -> Result<()> {
    g.run(r"del C:\build.log 2>nul & schtasks /delete /tn e2eprep /f >nul 2>&1")?;
    ensure!(
        g.run(
            r#"schtasks /create /tn e2eprep /sc once /st 23:59 /ru SYSTEM /tr "cmd /c powershell -ExecutionPolicy Bypass -File C:\src\contrib\e2e\windows\prepare.ps1" /f >nul & schtasks /run /tn e2eprep >nul"#
        )?,
        "could not start the prepare task"
    );
    let end = Instant::now() + Duration::from_secs(60 * 45);
    loop {
        let log = g.output(r"type C:\build.log 2>nul")?;
        if log.contains("BUILD DONE") {
            for step in ["agent", "apo", "install"] {
                ensure!(
                    log.contains(&format!("== {step} exit 0")),
                    "guest step `{step}` failed; C:\\build.log:\n{log}"
                );
            }
            return Ok(());
        }
        ensure!(
            Instant::now() < end,
            "guest build timed out; C:\\build.log:\n{log}"
        );
        std::thread::sleep(Duration::from_secs(5));
    }
}

pub fn run(o: &Opts) -> Result<ExitCode> {
    let root = repo_root();
    let stamp = stamp()?;
    let run_dir = vm::home().join(format!("windows/run-{stamp}"));
    make_overlay(&run_dir)?;
    let out = root.join(format!("target/e2e/run-{stamp}/windows"));
    println!("windows: booting {}", run_dir.display());
    let g = Guest::boot(&run_dir, NAME, USER)?;
    let result = (|| -> Result<bool> {
        g.wait_ssh(Duration::from_secs(60 * 30))?;
        g.sync_source(&root, r"C:\src")?;
        prepare(&g)?;
        let mut agent = format!(
            r"set RESONANCE_E2E_SANDBOX=1& C:\src\target\e2e-build\resonance-e2e.exe run --scenarios C:\src\contrib\e2e\scenarios --baselines C:\src\contrib\e2e\baselines\windows.toml --daemon C:\src\target\e2e-build\resonanced.exe --out C:\e2e-out --tier {}",
            o.tier
        );
        if let Some(f) = &o.scenario {
            agent.push_str(&format!(" --filter {f}"));
        }
        if o.update_baseline {
            agent.push_str(" --update-baseline");
        }
        let ok = g.run(&agent)?;
        g.pull(r"C:\e2e-out", &out)?;
        if o.update_baseline {
            let tmp = root
                .join("target/e2e")
                .join(format!("baselines-windows-{stamp}"));
            g.pull(r"C:\src\contrib\e2e\baselines", &tmp)?;
            std::fs::copy(
                tmp.join("windows.toml"),
                root.join("contrib/e2e/baselines/windows.toml"),
            )?;
        }
        Ok(ok)
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
        g.shutdown("shutdown /s /t 0");
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

const ISO_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkID=2195404";
const VIRTIO_URL: &str =
    "https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/stable-virtio/virtio-win.iso";
const SCREAM_URL: &str =
    "https://github.com/duncanthrax/scream/releases/download/4.0/Scream4.0.zip";

fn curl(url: &str, to: &Path) -> Result<()> {
    if to.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(to.parent().expect("has a parent"))?;
    let tmp = to.with_extension("part");
    ensure!(
        status(
            Command::new("curl")
                .args(["-fL", "--retry", "3", "-o"])
                .arg(&tmp)
                .arg(url)
        )?,
        "download of {url} failed"
    );
    std::fs::rename(tmp, to)?;
    Ok(())
}

fn ensure_ssh_key() -> Result<String> {
    let key = vm::ssh_key();
    if !key.exists() {
        std::fs::create_dir_all(key.parent().expect("has a parent"))?;
        ensure!(
            status(
                Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-C", "resonance-e2e", "-f"])
                    .arg(&key)
            )?,
            "ssh-keygen failed"
        );
    }
    Ok(std::fs::read_to_string(key.with_extension("pub"))?
        .trim()
        .to_string())
}

/// Run a script in the guest as SYSTEM through a one-shot scheduled task and
/// wait until `done_marker` shows up in `log` (a file the script writes).
fn run_as_system(
    g: &Guest,
    script: &str,
    log: &str,
    done_marker: &str,
    timeout: Duration,
) -> Result<String> {
    g.run(&format!(
        "del {log} 2>nul & schtasks /delete /tn e2eimg /f >nul 2>&1"
    ))?;
    ensure!(
        g.run(&format!(
            r#"schtasks /create /tn e2eimg /sc once /st 23:59 /ru SYSTEM /tr "cmd /c powershell -ExecutionPolicy Bypass -File {script}" /f >nul & schtasks /run /tn e2eimg >nul"#
        ))?,
        "could not start {script}"
    );
    let end = Instant::now() + timeout;
    loop {
        let text = g.output(&format!("type {log} 2>nul"))?;
        if text.contains(done_marker) {
            return Ok(text);
        }
        ensure!(Instant::now() < end, "{script} timed out; {log}:\n{text}");
        std::thread::sleep(Duration::from_secs(10));
    }
}

/// Install Windows 10 LTSC unattended under quickemu, then provision it (SSH is
/// installed by the answer file): build tools, rust, Scream with a locally
/// signed catalog, a warm source/build cache. The result is `windows/base`.
pub fn image() -> Result<ExitCode> {
    let base = base_dir();
    if base.join("disk.qcow2").exists() {
        println!("windows base image is up to date: {}", base.display());
        return Ok(ExitCode::SUCCESS);
    }
    let root = repo_root();
    let home = vm::home();
    let pubkey = ensure_ssh_key()?;
    let dl = home.join("downloads");
    let (iso, virtio) = (dl.join("windows-10-ltsc.iso"), dl.join("virtio-win.iso"));
    println!(
        "windows image: downloading installer media (cached in {})",
        dl.display()
    );
    curl(ISO_URL, &iso)?;
    curl(VIRTIO_URL, &virtio)?;

    let build = home.join("windows/image-build");
    let _ = std::fs::remove_dir_all(&build);
    let vmdir = build.join(NAME);
    std::fs::create_dir_all(&vmdir)?;
    // Answer file carrying our ssh key, packed as the ISO quickemu attaches as `unattended.iso`.
    let ans = build.join("answer");
    std::fs::create_dir_all(&ans)?;
    let xml = std::fs::read_to_string(root.join("contrib/e2e/windows/autounattend.xml"))?
        .replace("@SSH_PUBKEY@", &pubkey);
    std::fs::write(ans.join("autounattend.xml"), xml)?;
    ensure!(
        status(
            Command::new("genisoimage")
                .args(["-quiet", "-J", "-o"])
                .arg(vmdir.join("unattended.iso"))
                .arg(&ans)
        )?,
        "genisoimage failed (install cdrtools/genisoimage)"
    );
    std::os::unix::fs::symlink(&iso, vmdir.join("windows-11.iso"))?;
    std::os::unix::fs::symlink(&virtio, vmdir.join("virtio-win.iso"))?;
    std::fs::write(
        build.join(format!("{NAME}.conf")),
        format!(
            "#!/usr/bin/quickemu --vm\nguest_os=\"windows\"\ndisk_img=\"{NAME}/disk.qcow2\"\n\
             iso=\"{NAME}/windows-11.iso\"\nfixed_iso=\"{NAME}/virtio-win.iso\"\n\
             disk_size=\"64G\"\ntpm=\"off\"\nsecureboot=\"off\"\ncpu_cores=\"8\"\nram=\"8G\"\n"
        ),
    )?;
    println!("windows image: installing (unattended, 20-40 min)");
    let g = Guest::boot(&build, NAME, USER)?;
    let result = (|| -> Result<()> {
        g.wait_ssh(Duration::from_secs(60 * 120))?;
        let t = Duration::from_secs(60 * 60);
        // 1. Build tools + rust (the scripts write the marker last).
        g.run(r"if not exist C:\src mkdir C:\src")?;
        g.sync_source(&root, r"C:\src")?;
        run_as_system(
            &g,
            r"C:\src\contrib\e2e\windows\tools.ps1",
            r"C:\tools.log",
            "rust done",
            t,
        )?;
        // 2. Scream with a locally signed catalog, then test-signing needs a reboot.
        ensure!(
            g.run(&format!(
                r#"powershell -NoProfile -Command "curl.exe -sL -o C:\scream.zip {SCREAM_URL}; Expand-Archive -Force C:\scream.zip C:\scream""#
            ))?,
            "could not fetch Scream"
        );
        ensure!(
            g.run(r"powershell -NoProfile -ExecutionPolicy Bypass -File C:\src\contrib\e2e\windows\sign.ps1")?,
            "signing the Scream catalog failed"
        );
        g.run("shutdown /r /t 0")?;
        std::thread::sleep(Duration::from_secs(30));
        g.wait_ssh(Duration::from_secs(60 * 30))?;
        ensure!(
            g.run(r"cd /d C:\scream\Install && Install-x64.bat")?,
            "Scream install failed"
        );
        // 3. Warm build cache + APO install (also proves prepare.ps1 on a clean image).
        run_as_system(
            &g,
            r"C:\src\contrib\e2e\windows\prepare.ps1",
            r"C:\build.log",
            "BUILD DONE",
            Duration::from_secs(60 * 60),
        )?;
        Ok(())
    })();
    g.shutdown("shutdown /s /t 0");
    result?;
    std::fs::create_dir_all(&base)?;
    std::fs::rename(vmdir.join("disk.qcow2"), base.join("disk.qcow2"))?;
    std::fs::copy(vmdir.join("OVMF_VARS.fd"), base.join("OVMF_VARS.fd"))?;
    let _ = std::fs::remove_dir_all(&build);
    println!("windows image: ready at {}", base.display());
    Ok(ExitCode::SUCCESS)
}
