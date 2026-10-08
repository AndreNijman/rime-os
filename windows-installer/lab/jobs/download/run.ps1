# ─────────────────────────────────────────────────────────────────────────────
#  jobs/download — the app fetches its pinned installer image over the
#  network itself (WinHTTP), from the published release, and checks it.
#  Run: RIME_WINLAB_NET=1 winlab run jobs/download   (with a released .exe)
# ─────────────────────────────────────────────────────────────────────────────
$ErrorActionPreference = 'Continue'
$exe = Join-Path $PSScriptRoot 'rime-windows-installer.exe'
$out = Join-Path $env:TEMP 'dl.txt'
for ($i = 0; $i -lt 60 -and -not (Test-NetConnection github.com -Port 443 -InformationLevel Quiet -WarningAction SilentlyContinue); $i++) { Start-Sleep -Seconds 2 }
$t0 = Get-Date
$p = Start-Process -FilePath $exe -ArgumentList 'download' -Wait -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError "$out.err"
"--- rime-windows-installer download (exit $($p.ExitCode), $([int]((Get-Date) - $t0).TotalSeconds) s) ---"
Get-Content $out | Where-Object { $_ -notmatch '^PROGRESS' -or $_ -match '100%' }
Get-Content "$out.err"
$iso = 'C:\ProgramData\Rime\Installer\rime-os-netinstall-x86_64.iso'
if (Test-Path $iso) { "file: $((Get-Item $iso).Length) bytes, sha256 $((Get-FileHash $iso -Algorithm SHA256).Hash.ToLower())" }
if ($p.ExitCode -eq 0 -and (Get-Content $out -Raw) -match 'DOWNLOAD-OK') { 'DOWNLOAD-JOB-OK'; exit 0 } else { exit 1 }
