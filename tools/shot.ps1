param(
    [string]$Out = (Join-Path $PSScriptRoot "shot.png"),
    [int]$X = -1,
    [int]$Y = -1,
    # 需要真的点一下界面时才加：会移动光标并抢焦点（会打断你手头的事）
    [switch]$Click,
    # 强制走「抢焦点 + 屏幕拷贝」的老路子；默认用 PrintWindow，不打扰当前窗口
    [switch]$Focus
)
Add-Type -AssemblyName System.Drawing
$sig = @'
using System;
using System.Runtime.InteropServices;
public class Win32 {
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hWnd, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(int dwFlags, int dx, int dy, int dwData, int dwExtraInfo);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    // 直接把窗口内容画进 DC：不需要窗口在前台，也不动光标
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
}
'@
Add-Type -TypeDefinition $sig -ReferencedAssemblies System.Drawing
# 先声明 DPI 感知：否则坐标都是缩放后的逻辑像素，
# 截出来的图会把窗口右/下边裁掉一段（缩放 125% 时少 20%）。
[Win32]::SetProcessDPIAware() | Out-Null
# 同时开了好几个实例时取「有窗口的那个」里最新的，别把数组当句柄用
$proc = Get-Process claudesktop -ErrorAction SilentlyContinue |
    Where-Object { $_.MainWindowHandle -ne 0 } |
    Sort-Object StartTime -Descending |
    Select-Object -First 1
if (-not $proc) { throw "没找到带窗口的 claudesktop 进程" }
$hwnd = $proc.MainWindowHandle

if ($Click -or $Focus) {
    [Win32]::SetForegroundWindow($hwnd) | Out-Null
    Start-Sleep -Milliseconds 400
}
if ($Click) {
    [Win32]::SetCursorPos($X, $Y) | Out-Null
    Start-Sleep -Milliseconds 120
    [Win32]::mouse_event(0x02, 0, 0, 0, 0)
    [Win32]::mouse_event(0x04, 0, 0, 0, 0)
    Start-Sleep -Milliseconds 900
}

# 只取客户区：GPUI 的内容铺在客户区里，窗口矩形比它大一圈（DWM 边框），
# 按窗口矩形抓会在左边 / 下边多出几条没画的边，看起来像黑条。
$wr = New-Object Win32+RECT
[Win32]::GetWindowRect($hwnd, [ref]$wr) | Out-Null
$cr = New-Object Win32+RECT
[Win32]::GetClientRect($hwnd, [ref]$cr) | Out-Null
$pt = New-Object Win32+POINT
[Win32]::ClientToScreen($hwnd, [ref]$pt) | Out-Null
$w = $cr.Right - $cr.Left; $h = $cr.Bottom - $cr.Top
# 客户区在窗口矩形里的偏移：PrintWindow 从窗口左上角开始画，要按这个偏移裁
$dx = $pt.X - $wr.Left
$dy = $pt.Y - $wr.Top

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$path = if ($Focus) { "screen" } else { "printwindow" }

if ($Focus) {
    # 老路子：从屏幕上拷贝客户区（物理像素，必须配 DPI 感知的坐标）。要求窗口在前台、没被挡住
    $g.CopyFromScreen($pt.X, $pt.Y, 0, 0, $bmp.Size)
} else {
    # PW_RENDERFULLCONTENT(2)：DX 合成的窗口（GPUI 走 D3D）也能抓到内容，
    # 窗口被别的窗口挡住、或不在前台都没关系
    $full = New-Object System.Drawing.Bitmap ($wr.Right - $wr.Left), ($wr.Bottom - $wr.Top)
    $fg = [System.Drawing.Graphics]::FromImage($full)
    $hdc = $fg.GetHdc()
    $ok = [Win32]::PrintWindow($hwnd, $hdc, 2)
    $fg.ReleaseHdc($hdc)
    $fg.Dispose()
    if ($ok) {
        $g.DrawImage($full, (New-Object System.Drawing.Rectangle 0, 0, $w, $h), (New-Object System.Drawing.Rectangle $dx, $dy, $w, $h), [System.Drawing.GraphicsUnit]::Pixel)
    } else {
        # 有些窗口不支持 PrintWindow：退回屏幕拷贝（这一路需要窗口在前台）
        Write-Output "PrintWindow 失败，改用屏幕拷贝"
        $path = "screen(fallback)"
        $g.CopyFromScreen($pt.X, $pt.Y, 0, 0, $bmp.Size)
    }
    $full.Dispose()
}
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
Write-Output "saved $Out ${w}x${h} at $($pt.X),$($pt.Y) via $path"
