param(
  [string]$Action = "click",   # click | rclick | dblclick | drag | move | keys | scroll | where
  [int]$X = 0, [int]$Y = 0, [int]$X2 = 0, [int]$Y2 = 0,
  [string]$Keys = "", [int]$Delta = 0, [string]$ProcName = "hadion"
)
# X/Y are relative to the top-left of the Hadion window (same space as the screenshots).
Add-Type @"
using System; using System.Runtime.InteropServices;
public class M {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint data, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[M]::SetProcessDPIAware() | Out-Null
Add-Type -AssemblyName System.Windows.Forms
$p = Get-Process $ProcName -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Output "no window"; exit 1 }
$h = $p.MainWindowHandle
if ([M]::GetForegroundWindow() -ne $h) { [M]::SetForegroundWindow($h) | Out-Null; Start-Sleep -Milliseconds 300 }
$r = New-Object M+RECT; [M]::GetWindowRect($h, [ref]$r) | Out-Null
$ox = $r.L; $oy = $r.T
function Down($b) { [M]::mouse_event($(if ($b -eq "right") { 0x0008 } else { 0x0002 }), 0, 0, 0, [UIntPtr]::Zero) }
function Up($b)   { [M]::mouse_event($(if ($b -eq "right") { 0x0010 } else { 0x0004 }), 0, 0, 0, [UIntPtr]::Zero) }
function MoveTo($x, $y) { [M]::SetCursorPos($ox + $x, $oy + $y) | Out-Null }
switch ($Action) {
  "where"    { }
  "move"     { MoveTo $X $Y }
  "click"    { MoveTo $X $Y; Start-Sleep -Milliseconds 200; Down "left"; Start-Sleep -Milliseconds 60; Up "left" }
  "rclick"   { MoveTo $X $Y; Start-Sleep -Milliseconds 200; Down "right"; Start-Sleep -Milliseconds 60; Up "right" }
  "dblclick" { MoveTo $X $Y; Start-Sleep -Milliseconds 200; Down "left"; Start-Sleep -Milliseconds 40; Up "left"; Start-Sleep -Milliseconds 80; Down "left"; Start-Sleep -Milliseconds 40; Up "left" }
  "drag"     {
    MoveTo $X $Y; Start-Sleep -Milliseconds 200; Down "left"; Start-Sleep -Milliseconds 150
    $steps = 30
    for ($i = 1; $i -le $steps; $i++) {
      MoveTo ([int]($X + ($X2 - $X) * $i / $steps)) ([int]($Y + ($Y2 - $Y) * $i / $steps)); Start-Sleep -Milliseconds 16
    }
    Start-Sleep -Milliseconds 200; Up "left"
  }
  "scroll"   { MoveTo $X $Y; Start-Sleep -Milliseconds 100; $u = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]$Delta), 0); [M]::mouse_event(0x0800, 0, 0, $u, [UIntPtr]::Zero) }
  "keys"     { [System.Windows.Forms.SendKeys]::SendWait($Keys) }
}
Write-Output "ok $Action window=$ox,$oy size=$($r.R - $r.L)x$($r.B - $r.T)"
