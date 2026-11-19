# RBridge

Pure-Rust driver patcher, zero-Test-Mode catalog signer, and windowless boot guard for **AMD Radeon HD 8670M (GCN 1.0 Oland)** on **Radeon PRO Enterprise 21.Q2.1** (`U0366524.INF` / `U0369543.inf`).

No AMD driver binaries are stored in this repository.

## Usage
1. Download and extract AMD Radeon PRO Enterprise 21.Q2.1 locally (e.g. `C:\AMD\21.Q2.1`).
2. Download `rbridge.exe` and `rbridge-daemon.exe` from GitHub Actions artifacts into the same folder.
3. Run `rbridge.exe` as Administrator and choose **Option 1** for the full patch, sign, driver-only install, and boot guard setup.
4. Reboot once to finalize.
