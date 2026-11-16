param(
    [Parameter(Mandatory=$true)][int]$ProcessId,
    [Parameter(Mandatory=$true)][string]$Directory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class GatewayBenchWindow {
    public delegate bool EnumProc(IntPtr hwnd, IntPtr unused);
    [StructLayout(LayoutKind.Sequential)]
    public struct Memory {
        public uint cb, faults;
        public UIntPtr peakWorkingSet, workingSet, peakPagedPool, pagedPool;
        public UIntPtr peakNonPagedPool, nonPagedPool, pagefile, peakPagefile, privateBytes;
    }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc callback, IntPtr unused);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int max);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessageW(IntPtr hwnd, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd, uint message, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
    [DllImport("psapi.dll")] public static extern bool GetProcessMemoryInfo(IntPtr process, ref Memory memory, uint size);
    public struct Rect { public int Left, Top, Right, Bottom; }
    public static IntPtr Find(int pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((hwnd, unused) => {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != (uint)pid) return true;
            var title = new StringBuilder(256);
            GetWindowTextW(hwnd, title, title.Capacity);
            if (title.ToString() != "claudesktop") return true;
            found = hwnd;
            return false;
        }, IntPtr.Zero);
        return found;
    }
    public static void FocusInput(IntPtr hwnd) {
        Rect rect;
        if (!GetClientRect(hwnd, out rect)) throw new Exception("GetClientRect failed");
        double scale = GetDpiForWindow(hwnd) / 96.0;
        int x = (int)(rect.Right * 0.50), y = rect.Bottom - (int)(80 * scale);
        IntPtr position = new IntPtr((y << 16) | (x & 0xffff));
        SendMessageW(hwnd, 0x200, IntPtr.Zero, position);
        SendMessageW(hwnd, 0x201, new IntPtr(1), position);
        System.Threading.Thread.Sleep(30);
        SendMessageW(hwnd, 0x202, IntPtr.Zero, position);
    }
    public static long Send(IntPtr hwnd, string text) {
        FocusInput(hwnd);
        System.Threading.Thread.Sleep(300);
        foreach (char unit in text) SendMessageW(hwnd, 0x102, new IntPtr(unit), IntPtr.Zero);
        Rect rect;
        if (!GetClientRect(hwnd, out rect)) throw new Exception("GetClientRect failed");
        double scale = GetDpiForWindow(hwnd) / 96.0;
        double logicalWidth = rect.Right / scale;
        int x = (int)((240 + Math.Min(760, logicalWidth - 240) - 55) * scale);
        int y = rect.Bottom - (int)(38 * scale);
        IntPtr position = new IntPtr((y << 16) | (x & 0xffff));
        long sent = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        SendMessageW(hwnd, 0x200, IntPtr.Zero, position);
        SendMessageW(hwnd, 0x201, new IntPtr(1), position);
        System.Threading.Thread.Sleep(30);
        SendMessageW(hwnd, 0x202, IntPtr.Zero, position);
        return sent;
    }
}
'@
[void][GatewayBenchWindow]::SetProcessDPIAware()
$window = [IntPtr]::Zero
for ($attempt = 0; $attempt -lt 400; $attempt++) {
    $window = [GatewayBenchWindow]::Find($ProcessId)
    if ($window -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 50
}
if ($window -eq [IntPtr]::Zero) { throw 'Hidden application window was not found' }
$process = [Diagnostics.Process]::GetProcessById($ProcessId)
$handle = [GatewayBenchWindow]::OpenProcess(0x410, $false, $ProcessId)
if ($handle -eq [IntPtr]::Zero) { throw 'OpenProcess failed' }
$encoding = [Text.UTF8Encoding]::new($false)
$writer = [IO.StreamWriter]::new((Join-Path $Directory 'memory.csv'), $false, $encoding)
$writer.AutoFlush = $true
$writer.WriteLine('epoch_ms,phase,pb,ws,peak_pb,peak_ws,cpu_ms,paged_pool,nonpaged_pool,peak_paged_pool,peak_nonpaged_pool')
$lastId = 0
$phase = 'startup'
try {
    $rect = New-Object GatewayBenchWindow+Rect
    [void][GatewayBenchWindow]::GetClientRect($window, [ref]$rect)
    [IO.File]::WriteAllText((Join-Path $Directory 'ready.json'), (@{pid=$ProcessId; window=$window.ToInt64(); width=$rect.Right; height=$rect.Bottom; dpi=[GatewayBenchWindow]::GetDpiForWindow($window)} | ConvertTo-Json -Compress), $encoding)
    while (!$process.HasExited) {
        if ([GatewayBenchWindow]::IsWindowVisible($window)) { throw 'Application window became visible' }
        if ([GatewayBenchWindow]::GetForegroundWindow() -eq $window) { throw 'Application reached foreground' }
        $process.Refresh()
        $memory = New-Object GatewayBenchWindow+Memory
        $memory.cb = [Runtime.InteropServices.Marshal]::SizeOf($memory)
        if (![GatewayBenchWindow]::GetProcessMemoryInfo($handle, [ref]$memory, $memory.cb)) { throw 'GetProcessMemoryInfo failed' }
        $phasePath = Join-Path $Directory 'phase.txt'
        try {
            if ([IO.File]::Exists($phasePath)) { $phase = [IO.File]::ReadAllText($phasePath).Trim() }
        } catch [System.IO.IOException] {}
        $epoch = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $cpu = $process.TotalProcessorTime.TotalMilliseconds
        $writer.WriteLine(('{0},{1},{2},{3},{4},{5},{6},{7},{8},{9},{10}' -f $epoch,$phase,$memory.privateBytes.ToUInt64(),$memory.workingSet.ToUInt64(),$memory.peakPagefile.ToUInt64(),$memory.peakWorkingSet.ToUInt64(),$cpu,$memory.pagedPool.ToUInt64(),$memory.nonPagedPool.ToUInt64(),$memory.peakPagedPool.ToUInt64(),$memory.peakNonPagedPool.ToUInt64()))
        $command = $null
        try { $command = [IO.File]::ReadAllText((Join-Path $Directory 'command.json')) | ConvertFrom-Json } catch {}
        if ($null -ne $command -and [int]$command.id -gt $lastId) {
            $lastId = [int]$command.id
            $sent = $null
            switch ([string]$command.action) {
                'status' {}
                'send' { $sent = [GatewayBenchWindow]::Send($window, [string]$command.text) }
                'close' { [void][GatewayBenchWindow]::PostMessageW($window, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
                default { throw ('Unknown action: ' + [string]$command.action) }
            }
            $response = @{
                id=$lastId; sent_at_ms=$sent; at_ms=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
                hidden=$true; foreground=$false; pid=$ProcessId
            } | ConvertTo-Json -Compress
            [IO.File]::AppendAllText((Join-Path $Directory 'commands.jsonl'), $response + "`n", $encoding)
            [IO.File]::WriteAllText((Join-Path $Directory 'response.json'), $response, $encoding)
        }
        Start-Sleep -Milliseconds 20
    }
} finally {
    $writer.Dispose()
    [void][GatewayBenchWindow]::CloseHandle($handle)
    $process.Dispose()
}
