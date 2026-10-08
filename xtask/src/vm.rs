//! Throwaway quickemu guests: boot an overlay of a base image, reach it over
//! ssh, move source in and reports out, shut it down.

use anyhow::{Context, Result, bail, ensure};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `$RESONANCE_E2E_HOME` (default `~/.cache/resonance-e2e`).
pub fn home() -> PathBuf {
    std::env::var_os("RESONANCE_E2E_HOME").map_or_else(
        || {
            let h = std::env::var_os("HOME").expect("HOME is set");
            PathBuf::from(h).join(".cache/resonance-e2e")
        },
        PathBuf::from,
    )
}

/// The ssh key authorised in the guest images (created by `image`, or
/// `$RESONANCE_E2E_SSH_KEY` for a hand-made image).
pub fn ssh_key() -> PathBuf {
    std::env::var_os("RESONANCE_E2E_SSH_KEY")
        .map_or_else(|| home().join("ssh/id_ed25519"), PathBuf::from)
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

pub struct Guest {
    dir: PathBuf,
    name: String,
    port: u16,
    user: &'static str,
}

impl Guest {
    /// Boot `<conf>` (a quickemu conf in `dir`) headless on a free ssh port.
    pub fn boot(dir: &Path, name: &str, user: &'static str) -> Result<Self> {
        let port = free_port()?;
        let ok = Command::new("quickemu")
            .current_dir(dir)
            .args([
                "--vm",
                &format!("{name}.conf"),
                "--display",
                "none",
                "--ssh-port",
            ])
            .arg(port.to_string())
            .stdout(std::fs::File::create(dir.join("quickemu.log"))?)
            .stderr(Stdio::inherit())
            .status()
            .context("run quickemu (is it installed?)")?
            .success();
        ensure!(ok, "quickemu failed to start {name}");
        Ok(Self {
            dir: dir.to_path_buf(),
            name: name.into(),
            port,
            user,
        })
    }

    fn ssh_cmd(&self) -> Command {
        let mut c = Command::new("ssh");
        c.args(["-i"])
            .arg(ssh_key())
            .args(["-p", &self.port.to_string()])
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "BatchMode=yes",
            ])
            .arg(format!("{}@127.0.0.1", self.user));
        c
    }

    fn alive(&self) -> bool {
        std::fs::read_to_string(self.dir.join(format!("{}/{}.pid", self.name, self.name)))
            .ok()
            .and_then(|p| p.trim().parse::<u32>().ok())
            .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists())
    }

    /// Start the guest again on the same disk and ssh port (an unattended Windows install
    /// powers the machine off at the end of a setup phase instead of rebooting it).
    fn relaunch(&self) {
        let _ = Command::new("quickemu")
            .current_dir(&self.dir)
            .args([
                "--vm",
                &format!("{}.conf", self.name),
                "--display",
                "none",
                "--ssh-port",
            ])
            .arg(self.port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    pub fn wait_ssh(&self, timeout: Duration) -> Result<()> {
        let end = Instant::now() + timeout;
        loop {
            if !self.alive() {
                eprintln!("guest {} is not running; starting it again", self.name);
                self.relaunch();
            }
            let up = self
                .ssh_cmd()
                .arg("echo up")
                .stderr(Stdio::null())
                .output()
                .is_ok_and(|o| o.status.success());
            if up {
                return Ok(());
            }
            ensure!(
                Instant::now() < end,
                "guest {} did not answer ssh in time",
                self.name
            );
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    /// Run a command line in the guest's shell; true on exit code 0.
    pub fn run(&self, cmd: &str) -> Result<bool> {
        Ok(self.ssh_cmd().arg(cmd).status()?.success())
    }

    pub fn output(&self, cmd: &str) -> Result<String> {
        let o = self.ssh_cmd().arg(cmd).output()?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// Copy tracked + untracked-not-ignored files of `root` into `dest` in the
    /// guest as a tar stream. `-m` stamps files with the guest's clock: host
    /// mtimes ahead of or behind the guest's would otherwise make cargo think
    /// stale outputs are fresh.
    pub fn sync_source(&self, root: &Path, dest: &str) -> Result<()> {
        let list = Command::new("git")
            .current_dir(root)
            .args(["ls-files", "-z", "-c", "-o", "--exclude-standard"])
            .output()?;
        ensure!(list.status.success(), "git ls-files failed");
        let files: Vec<&[u8]> = list
            .stdout
            .split(|&b| b == 0)
            .filter(|f| !f.is_empty() && root.join(String::from_utf8_lossy(f).as_ref()).is_file())
            .collect();
        let mut names = Vec::new();
        for f in files {
            names.extend_from_slice(f);
            names.push(b'\n');
        }
        let mut tar = Command::new("tar")
            .current_dir(root)
            .args(["-cf", "-", "-T", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        tar.stdin.take().expect("piped").write_all(&names)?;
        let mut untar = self
            .ssh_cmd()
            .arg(format!("tar -xmf - -C {dest}"))
            .stdin(tar.stdout.take().expect("piped"))
            .spawn()?;
        ensure!(
            untar.wait()?.success(),
            "extracting source in the guest failed"
        );
        ensure!(tar.wait()?.success(), "tar of the source failed");
        Ok(())
    }

    /// Copy the guest directory `remote` into `local` as a tar stream.
    pub fn pull(&self, remote: &str, local: &Path) -> Result<()> {
        std::fs::create_dir_all(local)?;
        let mut src = self
            .ssh_cmd()
            .arg(format!("tar -cf - -C {remote} ."))
            .stdout(Stdio::piped())
            .spawn()?;
        let ok = Command::new("tar")
            .args(["-xmf", "-", "-C"])
            .arg(local)
            .stdin(src.stdout.take().expect("piped"))
            .status()?
            .success();
        src.wait()?;
        ensure!(ok, "pulling {remote} failed");
        Ok(())
    }

    /// Ask the guest to power off, then make sure the qemu process is gone.
    pub fn shutdown(&self, shutdown_cmd: &str) {
        let _ = self.ssh_cmd().arg(shutdown_cmd).status();
        let pid_file = self.dir.join(format!("{}/{}.pid", self.name, self.name));
        let end = Instant::now() + Duration::from_secs(90);
        while Instant::now() < end {
            let alive = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|p| p.trim().parse::<u32>().ok())
                .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
            if !alive {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        if let Ok(p) = std::fs::read_to_string(&pid_file) {
            let _ = Command::new("kill").arg(p.trim()).status();
        }
    }
}

pub fn qemu_img(args: &[&str]) -> Result<()> {
    if !Command::new("qemu-img").args(args).status()?.success() {
        bail!("qemu-img {args:?} failed");
    }
    Ok(())
}
