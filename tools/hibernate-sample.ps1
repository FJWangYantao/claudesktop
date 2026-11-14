param(
    [Parameter(Mandatory=$true)][int]$ProcessId,
    [Parameter(Mandatory=$true)][string]$Directory,
    [int]$IntervalMs = 100
)
$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class HibernateSample {
    [StructLayout(LayoutKind.Sequential)]
    public struct Memory {
        public uint cb, faults;
        public UIntPtr peakWorkingSet, workingSet, peakPagedPool, pagedPool;
        public UIntPtr peakNonPagedPool, nonPagedPool, pagefile, peakPagefile, privateBytes;
    }
    [StructLayout(LayoutKind.Sequential)]
    public struct Io {
        public ulong readOps, writeOps, otherOps, readBytes, writeBytes, otherBytes;
    }
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")]
    public static extern bool CloseHandle(IntPtr handle);
    [DllImport("psapi.dll", SetLastError=true)]
    public static extern bool GetProcessMemoryInfo(IntPtr process, ref Memory memory, uint size);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool GetProcessIoCounters(IntPtr process, out Io io);
}
'@
$out = Join-Path $Directory 'memory.csv'
$writer = New-Object System.IO.StreamWriter($out, $false, (New-Object System.Text.UTF8Encoding($false)))
$writer.AutoFlush = $true
$writer.WriteLine('epoch_ms,phase,pid,pb,ws,peak_pb,peak_ws,cpu_ms,read_bytes,write_bytes,tree_pb,tree_ws,tree_count')
$treeIds = @($ProcessId)
$treeUpdated = [datetime]::MinValue
$processHandle = [HibernateSample]::OpenProcess(0x410, $false, $ProcessId)
if ($processHandle -eq [IntPtr]::Zero) { throw ('OpenProcess failed: ' + [Runtime.InteropServices.Marshal]::GetLastWin32Error()) }
try {
    while (-not (Test-Path (Join-Path $Directory 'sample.stop'))) {
        try { $process = Get-Process -Id $ProcessId -ErrorAction Stop } catch { break }
        $process.Refresh()
        if (([datetime]::UtcNow - $treeUpdated).TotalSeconds -ge 1) {
            $all = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)
            $treeIds = @($ProcessId)
            do {
                $added = @($all | Where-Object { $treeIds -contains [int]$_.ParentProcessId -and $treeIds -notcontains [int]$_.ProcessId } | ForEach-Object { [int]$_.ProcessId })
                $treeIds += $added
            } while ($added.Count -gt 0)
            $treeUpdated = [datetime]::UtcNow
        }
        $memory = New-Object HibernateSample+Memory
        $memory.cb = [Runtime.InteropServices.Marshal]::SizeOf($memory)
        $io = New-Object HibernateSample+Io
        if (-not [HibernateSample]::GetProcessMemoryInfo($processHandle, [ref]$memory, $memory.cb)) { throw 'GetProcessMemoryInfo failed' }
        if (-not [HibernateSample]::GetProcessIoCounters($processHandle, [ref]$io)) { throw 'GetProcessIoCounters failed' }
        $treePb = [long]0
        $treeWs = [long]0
        $treeCount = 0
        foreach ($treeId in $treeIds) {
            try {
                $child = Get-Process -Id $treeId -ErrorAction Stop
                $treePb += $child.PrivateMemorySize64
                $treeWs += $child.WorkingSet64
                $treeCount++
                $child.Dispose()
            } catch {}
        }
        $phasePath = Join-Path $Directory 'phase.txt'
        $phase = if (Test-Path $phasePath) { [IO.File]::ReadAllText($phasePath).Trim() } else { 'startup' }
        $epoch = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $writer.WriteLine(('{0},{1},{2},{3},{4},{5},{6},{7},{8},{9},{10},{11},{12}' -f $epoch, $phase, $ProcessId, $memory.privateBytes.ToUInt64(), $memory.workingSet.ToUInt64(), $memory.peakPagefile.ToUInt64(), $memory.peakWorkingSet.ToUInt64(), [long]$process.TotalProcessorTime.TotalMilliseconds, $io.readBytes, $io.writeBytes, $treePb, $treeWs, $treeCount))
        $process.Dispose()
        Start-Sleep -Milliseconds $IntervalMs
    }
} finally {
    $writer.Dispose()
    [void][HibernateSample]::CloseHandle($processHandle)
}
