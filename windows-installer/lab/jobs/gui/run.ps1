# ─────────────────────────────────────────────────────────────────────────────
#  jobs/gui — the installer WINDOW, seen running on a real Windows desktop.
#
#  The lab agent runs as SYSTEM in session 0, where no window can be seen.
#  Boot 1 turns on autologon for the lab's Administrator (a lab-only account
#  whose password is already in autounattend.xml) and registers an at-logon
#  task that starts the installer in the logged-on desktop and presses Enter
#  Enter twice and Space once: Welcome, then the page that lists where Rime
#  could go, then the confirmation with its box ticked. Install is never
#  pressed. Boot 2 just waits while the host takes screenshots.
#  Run with: winlab run jobs/gui --boots 2
# ─────────────────────────────────────────────────────────────────────────────
$ErrorActionPreference = 'Continue'
$marker = 'C:\rimelab\gui-phase2'
if (-not (Test-Path $marker)) {
    # Free space, made the way a person makes it (see jobs/install).
    $c = Get-Partition -DriveLetter C
    $sup = Get-PartitionSupportedSize -DriveLetter C
    Resize-Partition -DriveLetter C -Size ([Math]::Max($sup.SizeMin + 2GB, $c.Size - 34GB))
    Copy-Item (Join-Path $PSScriptRoot 'rime-windows-installer.exe') 'C:\rimelab\rime-windows-installer.exe' -Force
    @'
Start-Transcript -Path 'C:\rimelab\gui-demo.log' -Force | Out-Null
Start-Process 'C:\rimelab\rime-windows-installer.exe'
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, string t);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  public static void Click(string text) {
    IntPtr top = FindWindow("RimeOsInstaller", null);
    IntPtr b = FindWindowEx(top, IntPtr.Zero, "Button", text);
    SendMessage(b, 0xF5, IntPtr.Zero, IntPtr.Zero); // BM_CLICK
  }
}
"@
Start-Sleep -Seconds 12
[W]::Click('Next')          # Welcome -> Where should Rime OS go?
Start-Sleep -Seconds 40     # the disks are read on a second thread
[W]::Click('Next')          # the one usable space is preselected -> Confirm
Start-Sleep -Seconds 20
[W]::Click('I have read this, and my important files are backed up')  # Install is NEVER pressed
Stop-Transcript | Out-Null
'@ | Out-File -FilePath 'C:\rimelab\gui-demo.ps1' -Encoding ascii
    $k = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
    Set-ItemProperty $k AutoAdminLogon '1'
    Set-ItemProperty $k DefaultUserName 'Administrator'
    Set-ItemProperty $k DefaultPassword 'RimeLab!2026'
    $a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\rimelab\gui-demo.ps1'
    $t = New-ScheduledTaskTrigger -AtLogOn -User 'Administrator'
    $p = New-ScheduledTaskPrincipal -UserId 'Administrator' -LogonType Interactive -RunLevel Highest
    Register-ScheduledTask -TaskName RimeGuiDemo -Action $a -Trigger $t -Principal $p -Force | Out-Null
    New-Item -ItemType File $marker -Force | Out-Null
    'reboot' | Out-File -FilePath (Join-Path $PSScriptRoot 'reboot.txt') -Encoding ascii
    "GUI-PHASE1-OK"
    exit 0
}
"GUI-PHASE2: waiting while the host takes screenshots"
Start-Sleep -Seconds 170
"--- the walk-through script's own transcript ---"
Get-Content 'C:\rimelab\gui-demo.log' -ErrorAction SilentlyContinue
"GUI-PHASE2-OK"
exit 0
