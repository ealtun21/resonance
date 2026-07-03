//! `resonance-tray` — optional standalone system-tray controller.
//! Not standalone: refuses to run without at least one installed UI, and is
//! never embedded in the daemon.

use resonance_ipc::tray::Ui;

mod icons;
mod menu;

/// The tray is an add-on to a UI; without any interface installed there is
/// nothing to "Open" and the tray must not run as an orphan.
fn should_run(uis: &[Ui]) -> bool {
    !uis.is_empty()
}

fn main() -> anyhow::Result<()> {
    let uis = resonance_ipc::tray::installed_uis();
    if !should_run(&uis) {
        eprintln!(
            "resonance-tray: no interface installed (need one of resonance-gui, \
             resonance-tui, or resonance). The tray cannot run standalone."
        );
        std::process::exit(2);
    }
    // Single instance: a second tray exits cleanly.
    let Some(_guard) =
        resonance_ipc::singleton::acquire(resonance_ipc::tray::control::TRAY_INSTANCE)?
    else {
        eprintln!("resonance-tray: already running");
        return Ok(());
    };

    // Backend + threads are wired in Task 12. For now, a reachable stub so the
    // crate builds and links.
    eprintln!(
        "resonance-tray: started ({} interface(s) present)",
        uis.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_run_without_a_ui() {
        assert!(!should_run(&[]));
        assert!(should_run(&[Ui::Cli]));
    }
}
