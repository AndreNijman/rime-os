# ─────────────────────────────────────────────────────────────────────────────
#  jobs/install — the Windows half of an install, done the way a person does it.
#
#  1. Make room the way the instructions tell a person to: shrink C: with
#     Windows' own tool (Resize-Partition is what Disk Management's Shrink
#     Volume calls) and leave the space unallocated.
#  2. Run the real rime-windows-installer.exe: `candidates`, then `install`
#     with the ID it offered, from the installer image on the RIMEISO disk.
#  3. Record what Windows sees afterwards. The agent then shuts Windows down,
#     and the HOST boots the machine again: the firmware's BootNext should
#     start Rime's installer from Rime's own new ESP.
# ─────────────────────────────────────────────────────────────────────────────
$ErrorActionPreference = 'Continue'
$exe = Join-Path $PSScriptRoot 'rime-windows-installer.exe'
function Run-Installer([string[]]$a) {
    $out = Join-Path $env:TEMP ("rwi-" + [guid]::NewGuid() + ".txt")
    $err = "$out.err"
    $p = Start-Process -FilePath $exe -ArgumentList $a -Wait -NoNewWindow -PassThru `
         -RedirectStandardOutput $out -RedirectStandardError $err
    $text = (Get-Content $out -Raw) + (Get-Content $err -Raw)
    "--- rime-windows-installer $($a -join ' ') (exit $($p.ExitCode)) ---"
    $text
    return @{ code = $p.ExitCode; text = $text }
}

"--- before ---"
Get-Partition | Format-Table -AutoSize DiskNumber, PartitionNumber, DriveLetter, Size, GptType | Out-String -Width 200
$c = Get-Partition -DriveLetter C
$sup = Get-PartitionSupportedSize -DriveLetter C
$new = [Math]::Max($sup.SizeMin + 2GB, $c.Size - 34GB)
"shrinking C: from $($c.Size) to $new (as Disk Management's Shrink Volume would; minimum $($sup.SizeMin))"
Resize-Partition -DriveLetter C -Size $new -ErrorAction Stop
Get-Partition | Format-Table -AutoSize DiskNumber, PartitionNumber, DriveLetter, Size, GptType | Out-String -Width 200

$iso = Get-Volume | Where-Object { $_.FileSystemLabel -eq 'RIMEISO' -and $_.DriveLetter } | Select-Object -First 1
if (-not $iso) { "FATAL: no RIMEISO volume"; exit 3 }
$isoPath = "$($iso.DriveLetter):\rime-os-netinstall-x86_64.iso"
"installer image: $isoPath"

$r = Run-Installer @('candidates')
$r.text
$id = ($r.text -split "`n" | Where-Object { $_ -match '^CANDIDATE (free:\S+)' } | ForEach-Object { $Matches[1] } | Select-Object -First 1)
if (-not $id) { "FATAL: no free-space candidate offered"; exit 4 }
"chosen: $id"

$r = Run-Installer @('install', $id, '--iso', $isoPath, '--yes')
if ($r.code -ne 0 -or $r.text -notmatch 'INSTALL-STAGED-OK') { "FATAL: install failed"; exit 5 }

"--- after ---"
Get-Partition | Format-Table -AutoSize DiskNumber, PartitionNumber, DriveLetter, Size, GptType, Guid | Out-String -Width 220
Get-Volume | Format-Table -AutoSize DriveLetter, FileSystemLabel, FileSystem, Size | Out-String -Width 200
"--- firmware, as Windows' own bcdedit sees it ---"
& bcdedit /enum firmware | Out-String -Width 200
$r = Run-Installer @('candidates')
if ($r.text -match 'CANDIDATE free:' -and $r.text -notmatch 'REFUSED') { "NOTE: free space still offered" }
"INSTALL-JOB-OK"
exit 0
