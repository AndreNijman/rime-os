# Fixtures for `run-checks.sh classify`

Nix build logs that `checks.ci` feeds to `run-checks.sh classify` (and to `build`, through a stub `nix`). A
download failure is retried once; anything else never is.

| file | from | classify |
|---|---|---|
| `download-416.log` | hosted run 41365303501, job `polish` (11-02): cache.nixos.org answered a resumed NAR download with HTTP 416 after curl's "Failed sending data to the peer"; timestamps stripped | download (retry) |
| `download-curl-send.log` | the same log with its final error changed to the curl form (`HTTP error 200 (curl error: Failed sending data to the peer)`), which nix prints when the retries run out on the send error itself | download (retry) |
| `build-failure.log` | a local `nix flake check` on 11-01: rime-libexec's installPhase failed (`Reason: builder failed with exit code 1`) | not a download |
| `build-failure-after-download-warnings.log` | eight download warnings from `download-416.log` (nix recovered from them) followed by `build-failure.log`: a builder failure must never be retried, whatever else the log says | not a download |
| `eval-error.log` | `nix build path:.#checks.x86_64-linux.no-such-check` | not a download |
