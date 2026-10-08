# Windows e2e guest

`cargo xtask e2e --os windows` boots a throwaway overlay of the base image, copies the source in, builds
the daemon, agent and APO in the guest (`prepare.ps1`, as SYSTEM), installs the APO with the repo's own
`contrib/windows/install-apo.ps1`, runs `resonance-e2e`, pulls `C:\e2e-out` back and powers the guest off.
`cargo xtask e2e image windows` builds the base image (no-op when it exists): Windows 10 Enterprise LTSC 2021
evaluation, unattended (`autounattend.xml`, which also installs sshd with the e2e key), then `tools.ps1`
(VS Build Tools + rustup), `sign.ps1` + Scream, and a warm build.

Two playback devices exist in the guest, and the agent switches between them (`select-endpoint.ps1`):

| Device | Formats | Used for |
|---|---|---|
| QEMU HDA | 2 ch, 48 kHz only | stereo scenarios |
| Scream (virtual, test-signed) | 8 ch only; 44.1, 48, 88.2, 96, 192 kHz (`setfmt.ps1`; not 176.4) | multichannel and rate scenarios |

Scream is a legacy WDM driver without effect modes, so the APO is attached in the **GFX slot (2)**
(`attach-slot.ps1`); EFX (slot 7) is never loaded there (spec section 14, W0).

Scenarios that need a steerable audio graph (mid-stream events, rate conversion hops) or a format neither
device offers are listed as "not applicable" in the report, with the reason; they run on Linux.

Hand-made images: set `RESONANCE_E2E_SSH_KEY` to the private key authorised in the guest.
