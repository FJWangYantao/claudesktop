param(
    [Parameter(Mandatory=$true)][int]$ProcessId,
    [Parameter(Mandatory=$true)][long]$Mask
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($Mask -le 0) { throw 'Affinity mask must be positive' }
$process = [Diagnostics.Process]::GetProcessById($ProcessId)
try {
    $previous = $process.ProcessorAffinity.ToInt64()
    $process.ProcessorAffinity = [IntPtr]::new($Mask)
    $process.Refresh()
    $actual = $process.ProcessorAffinity.ToInt64()
    if ($actual -ne $Mask) { throw 'Affinity mask did not match the requested value' }
    @{pid=$ProcessId; previous=$previous; mask=$actual} | ConvertTo-Json -Compress
} finally { $process.Dispose() }
