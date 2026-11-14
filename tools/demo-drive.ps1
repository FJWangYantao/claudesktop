param(
    [string]$Keys = "",
    [int]$X = -1,
    [int]$Y = -1,
    [int]$Wheel = 0,
    [int]$Wait = 1200,
    [string]$Out = "D:\360MoveData\Users\asus\Desktop\claudesktop\tools\demo-step.png"
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class W4 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(int dwFlags, int dx, int dy, int dwData, int dwExtraInfo);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
}
"@
[W4]::SetProcessDPIAware() | Out-Null
$proc = Get-Process claudesktop -ErrorAction Stop |
    Where-Object { $_.MainWindowHandle -ne 0 } |
    Sort-Object StartTime -Descending | Select-Object -First 1
$hwnd = $proc.MainWindowHandle
# 置顶再操作：否则输入会被盖在上面的窗口（比如 Kimi 本体）吃掉
[W4]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0003) | Out-Null
[W4]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 400

if ($Keys -ne "") {
    $shell = New-Object -ComObject WScript.Shell
    $shell.SendKeys($Keys) | Out-Null
    Start-Sleep -Milliseconds $Wait
} elseif ($X -ge 0) {
    [W4]::SetCursorPos($X, $Y) | Out-Null
    Start-Sleep -Milliseconds 150
    if ($Wheel -ne 0) {
        # mouse_event MOUSEEVENTF_WHEEL，dwData 正值向上
        for ($i = 0; $i -lt [Math]::Abs($Wheel); $i++) {
            [W4]::mouse_event(0x0800, 0, 0, 120 * [Math]::Sign($Wheel), 0)
            Start-Sleep -Milliseconds 80
        }
    } else {
        [W4]::mouse_event(0x02, 0, 0, 0, 0)
        [W4]::mouse_event(0x04, 0, 0, 0, 0)
    }
    Start-Sleep -Milliseconds $Wait
}

$wr = New-Object W4+RECT
[W4]::GetWindowRect($hwnd, [ref]$wr) | Out-Null
$cr = New-Object W4+RECT
[W4]::GetClientRect($hwnd, [ref]$cr) | Out-Null
$pt = New-Object W4+POINT
[W4]::ClientToScreen($hwnd, [ref]$pt) | Out-Null
$w = $cr.Right - $cr.Left; $h = $cr.Bottom - $cr.Top
$dx = $pt.X - $wr.Left; $dy = $pt.Y - $wr.Top
$full = New-Object System.Drawing.Bitmap ($wr.Right - $wr.Left), ($wr.Bottom - $wr.Top)
$fg = [System.Drawing.Graphics]::FromImage($full)
$hdc = $fg.GetHdc()
$ok = [W4]::PrintWindow($hwnd, $hdc, 2)
$fg.ReleaseHdc($hdc); $fg.Dispose()
$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
if ($ok) {
    $g.DrawImage($full, (New-Object System.Drawing.Rectangle 0, 0, $w, $h), (New-Object System.Drawing.Rectangle $dx, $dy, $w, $h), [System.Drawing.GraphicsUnit]::Pixel)
} else {
    $g.CopyFromScreen($pt.X, $pt.Y, 0, 0, $bmp.Size)
}
$full.Dispose()
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
[W4]::SetWindowPos($hwnd, [IntPtr](-2), 0, 0, 0, 0, 0x0003) | Out-Null
Write-Output ("saved {0} {1}x{2} at {3},{4} click={5}" -f $Out, $w, $h, $pt.X, $pt.Y, ($X -ge 0))
