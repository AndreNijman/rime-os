# P3 gates on katana: what moved out of P2

P3 is the GO on katana with Andre: the first time Rime on NixOS runs on real hardware with his accounts. Until then
nothing is installed, switched or booted on katana (VMs only). This file lists the P2 items whose last part needs
exactly that, so they are not left as open P2 rows. Each row says what it needs; none of it can be faked in a VM.

## Decision (2026-11-02, nix/apps): the rest of P2 moves to P3

docs/nixos/p2-gates.md row P2 asked for "Steam launches a game in Gaming Mode in a VM with a GPU stub". The VM half
is proven offline by `checks.gaming-launch`: greetd starts Gaming Mode, Steam's own client binary runs inside
gamescope with the environment Rime gives it, and a game started the way Steam starts one is focused and composited
(gamescope's own screenshot shows the game, told apart from any other picture). What is left is Steam itself:
signing in, updating, and starting an installed game (`steam -applaunch`), including a Windows game through Proton.

That part moves to P3 on katana, not to a P2 VM run with a test account. Why:

- It needs a Steam account with Steam Guard and the network. The only account that owns the games Rime is measured
  with is Andre's, and its credentials do not go into a VM rig or a test log.
- A throwaway account in a VM would prove only the sign-in screen: it owns no game to start, Proton in a VM runs on
  lavapipe (not the NVIDIA driver path Rime ships), and gamescope runs `--backend headless`, not on DRM. Those are
  exactly the parts that broke on bootc (the Ampere opacity-micromap hang, direct scan-out, the shader cache size).
- katana is where those parts are measured anyway (its bootc bench: the same games, the same settings), so the P3
  run compares like with like.

## Rows

| # | Gate | Needs | Evidence that closes it |
|---|---|---|---|
| P3-S1 | Steam signs in and updates in Gaming Mode | katana on Rime on NixOS, Andre's Steam account, network | Gaming Mode screenshot of the library after sign-in; Steam's bootstrap log shows the update finished |
| P3-S2 | `steam -applaunch` starts an installed native game from Gaming Mode | P3-S1 | the game's window focused by gamescope (`GAMESCOPE_FOCUSED_APP` = its app id) and a frame-time log |
| P3-S3 | A Windows game through Proton (Cyberpunk 2077, app 1091500, the bootc bench game) | P3-S1, the NVIDIA driver | the built-in benchmark runs to the end; fps within the bootc bench's range on the same settings |
| P3-S4 | gamescope on DRM with the NVIDIA GPU (not headless), HDR off by Rime's guard, composite forced | katana's panel | gamescope's screenshot plus `xprop -root` showing the guard's values while a game runs |
| P3-V1 | ChatGPT and Claude Desktop sign in and render on the real GPU | Andre's OpenAI and Anthropic accounts | each app signed in, window screenshot; `checks.vendor-apps` proves install and start offline |
| P3-E1 | Equibop's Rich Presence with a signed-in Discord account shows a running game | Andre's Discord account | Discord shows the game as his activity; `checks.equibop` proves arRPC answers offline |
