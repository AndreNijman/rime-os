# ─────────────────────────────────────────────────────────────────────────────
#  jobs/gui-install — the WINDOW does the install, the way a person would.
#
#  Like jobs/gui, boot 1 shrinks C:, turns on autologon and registers an
#  at-logon task; boot 2 is the logged-on desktop. The task puts the
#  installer image beside the .exe (a copy from the RIMEISO disk, which
#  `winlab run` attaches with RIME_WINLAB_EXTRA), starts the window and
#  clicks Next, Next, ticks the box and clicks Install, then waits for the
#  window's "Restart now" button. Boot 2 reports what Windows then sees:
#  Rime's partitions, and "Rime OS Setup" as the firmware's one-shot.
#  Run: RIME_WINLAB_EXTRA=iso.raw winlab run jobs/gui-install --boots 2
# ─────────────────────────────────────────────────────────────────────────────
$ErrorActionPreference = 'Continue'
$marker = 'C:\rimelab\gui-install-phase2'
if (-not (Test-Path $marker)) {
    $c = Get-Partition -DriveLetter C
    $sup = Get-PartitionSupportedSize -DriveLetter C
    Resize-Partition -DriveLetter C -Size ([Math]::Max($sup.SizeMin + 2GB, $c.Size - 34GB))
    Copy-Item (Join-Path $PSScriptRoot 'rime-windows-installer.exe') 'C:\rimelab\rime-windows-installer.exe' -Force
    @'
Start-Transcript -Path 'C:\rimelab\gui-install.log' -Force | Out-Null
$iso = Get-Volume | Where-Object { $_.FileSystemLabel -eq 'RIMEISO' -and $_.DriveLetter } | Select-Object -First 1
Copy-Item "$($iso.DriveLetter):\rime-os-netinstall-x86_64.iso" 'C:\rimelab\rime-os-netinstall-x86_64.iso'
Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public static class W {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, string t);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr h);
  static IntPtr Btn(string text) { return FindWindowEx(FindWindow("RimeOsInstaller", null), IntPtr.Zero, "Button", text); }
  public static void Click(string text) { SendMessage(Btn(text), 0xF5, IntPtr.Zero, IntPtr.Zero); }
  public static bool Shown(string text) { IntPtr b = Btn(text); return b != IntPtr.Zero && IsWindowVisible(b) && IsWindowEnabled(b); }
}
"@
Start-Process 'C:\rimelab\rime-windows-installer.exe'
Start-Sleep -Seconds 10
[W]::Click('Next'); Start-Sleep -Seconds 40
[W]::Click('Next'); Start-Sleep -Seconds 5
[W]::Click('I have read this, and my important files are backed up'); Start-Sleep -Seconds 2
[W]::Click('Install')  # same button, its text is "Install" on this page
"clicked Install at $(Get-Date -Format o)"
for ($i = 0; $i -lt 120; $i++) {
  if ([W]::Shown('Restart now')) { "DONE PAGE at $(Get-Date -Format o)"; 'done' | Out-File C:\rimelab\gui-install.done; break }
  Start-Sleep -Seconds 5
}
Stop-Transcript | Out-Null
'@ | Out-File -FilePath 'C:\rimelab\gui-install.ps1' -Encoding ascii
    $k = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
    Set-ItemProperty $k AutoAdminLogon '1'
    Set-ItemProperty $k DefaultUserName 'Administrator'
    Set-ItemProperty $k DefaultPassword 'RimeLab!2026'
    $a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\rimelab\gui-install.ps1'
    $t = New-ScheduledTaskTrigger -AtLogOn -User 'Administrator'
    $p = New-ScheduledTaskPrincipal -UserId 'Administrator' -LogonType Interactive -RunLevel Highest
    Register-ScheduledTask -TaskName RimeGuiInstall -Action $a -Trigger $t -Principal $p -Force | Out-Null
    New-Item -ItemType File $marker -Force | Out-Null
    'reboot' | Out-File -FilePath (Join-Path $PSScriptRoot 'reboot.txt') -Encoding ascii
    "GUI-INSTALL-PHASE1-OK"
    exit 0
}
for ($i = 0; $i -lt 160 -and -not (Test-Path 'C:\rimelab\gui-install.done'); $i++) { Start-Sleep -Seconds 5 }
"--- the window's run ---"
Get-Content 'C:\rimelab\gui-install.log' -ErrorAction SilentlyContinue
Get-Partition | Format-Table -AutoSize DiskNumber, PartitionNumber, DriveLetter, Size, GptType | Out-String -Width 200
& bcdedit /enum firmware | Out-String -Width 200
Get-Content 'C:\ProgramData\Rime\Installer\journal.cfg' -ErrorAction SilentlyContinue
if (Test-Path 'C:\rimelab\gui-install.done') { "GUI-INSTALL-OK"; exit 0 } else { "GUI-INSTALL-TIMEOUT"; exit 1 }
