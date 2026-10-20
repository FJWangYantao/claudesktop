# 各配色截图：每套配色 × 浅 / 深各起一个静默实例（CLAUDESKTOP_QUIET，不抢前台），
# 打开设置页「外观」，用 PrintWindow 抓一帧，抓完只杀自己起的那个进程。
# 明暗用独立的临时数据目录（CLAUDESKTOP_DATA_DIR）里的 settings.json 指定，
# 不碰用户自己的 settings.json；配色用 CLAUDESKTOP_THEME。
param(
    [string]$OutDir = (Join-Path $PSScriptRoot "theme-shots"),
    [string]$Open = "settings:0"
)
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\debug\claudesktop.exe"
if (-not (Test-Path $exe)) { throw "先 cargo build -p claudesktop" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$env:CLAUDESKTOP_QUIET = "1"
$env:CLAUDESKTOP_OPEN = $Open
$themes = "warm", "flexoki", "rose_pine", "everforest", "nord", "catppuccin"
foreach ($appearance in "light", "dark") {
    $data = Join-Path $env:TEMP "claudesktop-shots-$appearance"
    New-Item -ItemType Directory -Force -Path $data | Out-Null
    # 只写这一项：其余字段由 #[serde(default)] 补齐
    "{`"appearance`":`"$appearance`"}" | Set-Content -Path (Join-Path $data "settings.json") -Encoding UTF8
    $env:CLAUDESKTOP_DATA_DIR = $data
    foreach ($theme in $themes) {
        $env:CLAUDESKTOP_THEME = $theme
        $p = Start-Process -FilePath $exe -PassThru
        Start-Sleep -Seconds 7
        $out = Join-Path $OutDir ("theme-{0}-{1}.png" -f $theme, $appearance)
        & (Join-Path $root "tools\shot.ps1") -Out $out | Out-Null
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 900
        Write-Output "saved $out"
    }
}
