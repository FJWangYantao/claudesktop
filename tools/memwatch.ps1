# memwatch.ps1 —— 盯一个进程（默认 claudesktop）的内存，逐秒打点 + 落 CSV。
#
#   powershell -NoProfile -File tools\memwatch.ps1
#   powershell -NoProfile -File tools\memwatch.ps1 -Name claudesktop -Interval 2 -Tree
#
# 没在跑就一直等，跑起来自动开始记，进程退出后打一行汇总再退出。
# 单位统一 MB；WS = 工作集（任务管理器里那列），PB = 私有提交（更接近真实占用），
# 峰值取系统记的 PeakWorkingSet64（进程自己报的历史最高）。

# 进程名支持通配符：下载来的副本常叫 “claudesktop (6).exe”，精确名对不上。
param(
  [string]$Name = 'claudesktop*',
  [string]$PathLike = 'claudesktop',   # 名字被改过也能按路径认出来
  [double]$Interval = 1,
  [string]$Out = '',
  [switch]$Tree,          # 连同子进程（内核 / MCP 服务器 / acp agent）一起算
  [int]$Quiet = 1         # 每几秒打一行；1 = 每拍都打
)

$ErrorActionPreference = 'Stop'
if (-not $Out) { $Out = Join-Path $PSScriptRoot 'memwatch.csv' }
$cores = [Environment]::ProcessorCount

function MB([double]$b) { [math]::Round($b / 1MB, 1) }

function Get-Tree([int]$rootId) {
  $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId
  $ids = @($rootId); $frontier = @($rootId)
  while ($frontier.Count -gt 0) {
    $kids = @($all | Where-Object { $frontier -contains [int]$_.ParentProcessId } | ForEach-Object { [int]$_.ProcessId })
    $kids = @($kids | Where-Object { $ids -notcontains $_ })
    $ids += $kids; $frontier = $kids
  }
  return $ids
}

# CSV 头
if (-not (Test-Path $Out)) {
  'time,pid,ws_mb,pb_mb,peak_ws_mb,cpu_pct,threads,handles,ws_tree_mb' | Set-Content -Path $Out -Encoding UTF8
}

Write-Host ''
Write-Host "memwatch  target=$Name  interval=${Interval}s  tree=$($Tree.IsPresent)  cores=$cores" -ForegroundColor Cyan
Write-Host "log: $Out" -ForegroundColor DarkGray
Write-Host ''
$fmt = '{0,-8} {1,6} {2,8} {3,8} {4,8} {5,7} {6,6} {7,7} {8,8}'
Write-Host ($fmt -f 'time', 'pid', 'WS(MB)', 'PB(MB)', 'peak(MB)', 'cpu%', 'thr', 'hdl', 'tree(MB)') -ForegroundColor DarkGray
Write-Host ('-' * 76) -ForegroundColor DarkGray

$started = $null
$peak = 0.0
$peakPb = 0.0
$sumWs = 0.0
$samples = 0
$lastCpu = $null
$lastAt = $null
$waiting = $false
$tick = 0

while ($true) {
  $procs = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
      $_.ProcessName -like $Name -or ($_.Path -and $_.Path -like "*$PathLike*" -and $_.ProcessName -notmatch '^(powershell|pwsh|bash|cmd|conhost|WindowsTerminal)$')
    })
  if ($procs.Count -eq 0) {
    if ($started) { break }   # 记过数了，进程没了就收尾
    if (-not $waiting) {
      Write-Host ("{0}  waiting for '{1}' ..." -f (Get-Date -Format 'HH:mm:ss'), $Name) -ForegroundColor Yellow
      $waiting = $true
    }
    Start-Sleep -Milliseconds ([int]($Interval * 1000))
    continue
  }
  $waiting = $false

  $p = $procs | Sort-Object -Property WorkingSet64 -Descending | Select-Object -First 1
  $now = Get-Date
  if (-not $started) { $started = $now; $lastAt = $now; $lastCpu = $p.TotalProcessorTime }

  $ws = MB $p.WorkingSet64
  $pb = MB $p.PrivateMemorySize64
  $pk = MB $p.PeakWorkingSet64
  if ($ws -gt $peak) { $peak = $ws }
  if ($pb -gt $peakPb) { $peakPb = $pb }
  $sumWs += $ws; $samples++

  # CPU：两次采样的处理器时间差 / 墙钟 / 核数
  $cpu = 0.0
  $dt = ($now - $lastAt).TotalSeconds
  if ($dt -gt 0) { $cpu = [math]::Round((($p.TotalProcessorTime - $lastCpu).TotalSeconds / $dt / $cores) * 100, 1) }
  $lastCpu = $p.TotalProcessorTime; $lastAt = $now

  $treeWs = ''
  $treeVal = $ws
  if ($Tree) {
    $ids = Get-Tree $p.Id
    $treeVal = 0.0
    foreach ($id in $ids) {
      $q = Get-Process -Id $id -ErrorAction SilentlyContinue
      if ($q) { $treeVal += (MB $q.WorkingSet64) }
    }
    $treeWs = [math]::Round($treeVal, 1)
  }

  $tick++
  if ($Quiet -le 1 -or ($tick % $Quiet) -eq 0) {
    Write-Host ($fmt -f $now.ToString('HH:mm:ss'), $p.Id, $ws, $pb, $pk, $cpu, $p.Threads.Count, $p.HandleCount, $treeWs)
  }
  ('{0},{1},{2},{3},{4},{5},{6},{7},{8}' -f $now.ToString('yyyy-MM-dd HH:mm:ss'), $p.Id, $ws, $pb, $pk, $cpu, $p.Threads.Count, $p.HandleCount, $treeWs) |
    Add-Content -Path $Out -Encoding UTF8

  Start-Sleep -Milliseconds ([int]($Interval * 1000))
}

# 走到这里说明进程没了
$mins = if ($started) { [math]::Round(((Get-Date) - $started).TotalMinutes, 1) } else { 0 }
Write-Host ''
Write-Host ('-' * 76) -ForegroundColor DarkGray
Write-Host ("'{0}' 已退出  观察 {1} 分钟 / {2} 次采样" -f $Name, $mins, $samples) -ForegroundColor Cyan
if ($samples -gt 0) {
  Write-Host ("峰值 WS = {0} MB   峰值 PB = {1} MB   平均 WS = {2} MB" -f $peak, $peakPb, [math]::Round($sumWs / $samples, 1)) -ForegroundColor Cyan
}
Write-Host "明细: $Out" -ForegroundColor DarkGray
