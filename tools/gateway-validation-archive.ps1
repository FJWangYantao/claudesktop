param(
    [Parameter(Mandatory=$true)][string]$Plan,
    [Parameter(Mandatory=$true)][string]$Output
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$entries = [IO.File]::ReadAllText($Plan) | ConvertFrom-Json
if ($entries -isnot [Array]) { $entries = @($entries) }
$names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($row in $entries) {
    $name = [string]$row.path
    if ($name -match '(^/|\\|:|(^|/)\.\.(/|$))' -or !$names.Add($name)) {
        throw "Invalid or duplicate archive entry: $name"
    }
    if (!(Test-Path -LiteralPath $row.source -PathType Leaf)) { throw "Missing source: $name" }
}
$partial = $Output + '.partial'
if ([IO.File]::Exists($Output) -or [IO.File]::Exists($partial)) { throw 'Use a new archive output path.' }
$created = $false
try {
    $stream = [IO.File]::Open($partial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    $created = $true
    try {
        $zip = [IO.Compression.ZipArchive]::new($stream, [IO.Compression.ZipArchiveMode]::Create, $true)
        try {
            foreach ($row in $entries) {
                $entry = $zip.CreateEntry([string]$row.path, [IO.Compression.CompressionLevel]::Optimal)
                $sourceStream = [IO.File]::OpenRead([string]$row.source)
                try {
                    $target = $entry.Open()
                    try { $sourceStream.CopyTo($target) } finally { $target.Dispose() }
                } finally { $sourceStream.Dispose() }
            }
        } finally { $zip.Dispose() }
    } finally { $stream.Dispose() }
    $check = [IO.Compression.ZipFile]::OpenRead($partial)
    try {
        if ($check.Entries.Count -ne $entries.Count) { throw 'Archive entry count differs.' }
        foreach ($row in $entries) {
            $entry = $check.GetEntry([string]$row.path)
            if ($null -eq $entry -or $entry.Length -ne [long]$row.bytes) { throw "Archive length differs: $($row.path)" }
            $entryStream = $entry.Open()
            $hash = [Security.Cryptography.SHA256]::Create()
            try {
                $actual = [BitConverter]::ToString($hash.ComputeHash($entryStream)).Replace('-', '').ToLowerInvariant()
                if ($actual -ne [string]$row.sha256) { throw "Archive hash differs: $($row.path)" }
            } finally { $entryStream.Dispose(); $hash.Dispose() }
        }
    } finally { $check.Dispose() }
    [IO.File]::Move($partial, $Output)
    @{entries=$entries.Count; bytes=([IO.FileInfo]$Output).Length; sha256=(Get-FileHash -LiteralPath $Output -Algorithm SHA256).Hash.ToLowerInvariant()} | ConvertTo-Json -Compress
} catch {
    if ($created -and [IO.File]::Exists($partial)) { [IO.File]::Delete($partial) }
    throw
}
