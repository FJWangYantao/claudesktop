param([string]$Out = "C:\Users\WYT\Desktop\claudesktop\tools\shot.png", [int]$X = -1, [int]$Y = -1, [switch]$Click)
Add-Type -AssemblyName System.Drawing
$sig = @'
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(int dwFlags, int dx, int dy, int dwData, int dwExtraInfo);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
Add-Type -TypeDefinition $sig -ReferencedAssemblies System.Drawing
# 先声明 DPI 感知：否则 GetWindowRect 拿到的是缩放后的逻辑坐标，
# 截出来的图会把窗口右/下边裁掉一段（缩放 125% 时少 20%）。
[Win32]::SetProcessDPIAware() | Out-Null
$proc = Get-Process claudesktop -ErrorAction Stop
[Win32]::SetForegroundWindow($proc.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 400
if ($Click) {
    [Win32]::SetCursorPos($X, $Y) | Out-Null
    Start-Sleep -Milliseconds 120
    [Win32]::mouse_event(0x02, 0, 0, 0, 0)
    [Win32]::mouse_event(0x04, 0, 0, 0, 0)
    Start-Sleep -Milliseconds 900
}
$r = New-Object Win32+RECT
[Win32]::GetWindowRect($proc.MainWindowHandle, [ref]$r) | Out-Null
$w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
# 按窗口左上角截图：CopyFromScreen 用的是物理像素，必须配 DPI 感知的坐标
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out ${w}x${h} origin $($r.Left),$($r.Top)"
