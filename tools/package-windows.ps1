param(
    [string]$Executable = "target/release/claudesktop.exe",
    [string]$Destination = "dist",
    [string]$Tag = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
$repo = Split-Path $PSScriptRoot -Parent
$manifest = Get-Content (Join-Path $repo "Cargo.toml") -Raw
$version = [regex]::Match($manifest, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (!$version) { throw "Workspace version was not found." }
if ($Tag -and $Tag -ne "v$version") { throw "Tag $Tag does not match workspace version $version." }
$Tag = "v$version"
if (![IO.Path]::IsPathRooted($Executable)) { $Executable = Join-Path $repo $Executable }
if (![IO.Path]::IsPathRooted($Destination)) { $Destination = Join-Path $repo $Destination }
$Executable = (Resolve-Path $Executable).Path
$info = [Diagnostics.FileVersionInfo]::GetVersionInfo($Executable)
if ($info.ProductVersion -ne $version) { throw "Executable version $($info.ProductVersion) does not match $version." }
$reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Executable))
try {
    if ($reader.ReadUInt16() -ne 0x5a4d) { throw "Executable has no DOS header." }
    $reader.BaseStream.Position = 0x3c
    $offset = $reader.ReadUInt32()
    $reader.BaseStream.Position = $offset
    if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) {
        throw "Executable must be a Windows x64 PE file."
    }
} finally { $reader.Dispose() }

$bundle = Join-Path $Destination "claudesktop-$Tag-windows-x64"
$archive = "$bundle.zip"
if ((Test-Path $bundle) -or (Test-Path $archive)) { throw "Bundle already exists: $bundle" }
New-Item -ItemType Directory -Force -Path $bundle | Out-Null
Copy-Item $Executable (Join-Path $bundle "claudesktop.exe")
Copy-Item (Join-Path $repo "README.md"), (Join-Path $repo "LICENSE") $bundle
Compress-Archive -Path "$bundle/*" -DestinationPath $archive -CompressionLevel Optimal

Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($archive)
try {
    $expected = @("claudesktop.exe", "README.md", "LICENSE")
    if ($zip.Entries.Count -ne $expected.Count) { throw "Unexpected archive entries." }
    foreach ($name in $expected) {
        $entry = $zip.GetEntry($name)
        if (!$entry) { throw "Archive entry is missing: $name" }
        $stream = $entry.Open()
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $actual = [BitConverter]::ToString($sha.ComputeHash($stream)).Replace("-", "") }
        finally { $sha.Dispose(); $stream.Dispose() }
        if ($actual -ne (Get-FileHash (Join-Path $bundle $name) -Algorithm SHA256).Hash) {
            throw "Archive entry failed verification: $name"
        }
    }
} finally { $zip.Dispose() }

Copy-Item $Executable (Join-Path $Destination "claudesktop.exe") -Force
$checksums = @((Join-Path $Destination "claudesktop.exe"), $archive) | ForEach-Object {
    $hash = Get-FileHash $_ -Algorithm SHA256
    "$($hash.Hash.ToLowerInvariant())  $(Split-Path $hash.Path -Leaf)"
}
[IO.File]::WriteAllText((Join-Path $Destination "SHA256SUMS.txt"), ($checksums -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
Write-Output "Packaged $Tag (Windows x64); all archive entries verified."
Write-Output $checksums
