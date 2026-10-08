# ─────────────────────────────────────────────────────────────────────────────
#  jobs/after-install — Windows, started again after Rime was installed.
#
#  That this script runs at all is the first result: Windows still boots
#  from its own ESP with its own boot manager. The rest records what Windows
#  sees of the machine Rime now shares with it.
# ─────────────────────────────────────────────────────────────────────────────
$ErrorActionPreference = 'Continue'
"WINDOWS-BOOTED-AFTER-RIME"
Get-Partition | Format-Table -AutoSize DiskNumber, PartitionNumber, DriveLetter, Size, GptType, Guid | Out-String -Width 220
Get-Volume | Format-Table -AutoSize DriveLetter, FileSystemLabel, FileSystem, HealthStatus, Size | Out-String -Width 200
"--- chkdsk C: (read-only scan) ---"
& chkdsk C: /scan 2>&1 | Select-Object -Last 8 | Out-String -Width 200
"--- firmware entries ---"
& bcdedit /enum firmware | Out-String -Width 200
"AFTER-INSTALL-JOB-OK"
exit 0
