# Kernel image signing (D3 + D7)

`rime-mok.pem` is the public half of the Rime MOK (the `APEX_SB_KEY_B64` / `APEX_SB_CRT_B64` CI secrets). It is
the certificate bootc images ship as `/usr/share/rime-os/secureboot/rime-mok.der` (written by
`Containerfile.core` stage 1b from the `rime_sb_crt` secret) and that users enrol with `mokutil --import`. It
was not in the repository before; this copy was taken on 2026-11-01 from that file on a machine running a
published bootc image, and `sbverify --cert rime-mok.pem` accepts that image's kernel
(`7.2.9-cachyos1.apex1.fc45`).

- Subject `CN=APEX-OS Secure Boot key (andre)`, serial `67EDF53EC448DD45BD6504BBE8479B999721E2F1`,
  valid 2026-07-26 to 2036-07-23.
- SHA-256 of the DER: `2fdd2f55e18d6d56ca97094bb9ffa116d9ad962c8cb5591484ee1e2a6958bcbb`.

Flow: `nix-kernel-sign.yml` (GitHub-hosted, never katana) builds `rimeKernelImage` for a host, runs
`../sign-detached.sh` with the secrets (which refuses a key whose certificate is not this file), and uploads
`<hash>.sig` + `<hash>.json`. They are committed to `signatures/`. `nix/modules/kernel.nix` then boots
`sign.nix`'s output: the same image with the signature attached, verified against this certificate at build
time. Rotating the MOK means replacing this file in the same PR as the new signatures.
