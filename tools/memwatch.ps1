# memwatch.ps1 —— 盯一个进程（默认 claudesktop）的内存，逐秒打点 + 落 CSV。
#
#   powershell -NoProfile -File tools\memwatch.ps1
#   powershell -NoProfile -File tools\memwatch.ps1 -Name claudesktop -Interval 2 -Tree
#   powershell -NoProfile -File tools\memwatch.ps1 -Tree -Children -Mark 压测前
#
# 没在跑就一直等，跑起来自动开始记，进程退出后打一行汇总再退出。
# 单位统一 MB；WS = 工作集（任务管理器里那列），PB = 私有提交（更接近真实占用），
# 峰值取系统记的 PeakWorkingSet64（进程自己报的历史最高）。
# -Tree：连同子进程（内核 / MCP 服务器 / acp agent）一起算，树 WS 和树 PB 都记
#   （树 WS 把共享页重复计入，别当精确物理占用；对账用树 PB）。
# -Children：每次打点顺带列出子进程清单（名字 PID WS PB），看哪个 sidecar 在吃内存。
# -Mark：把场景标记写进 CSV 的 mark 列和打点行末尾，跑对照实验时切场景换标记。

# 进程名支持通配符：下载来的副本常叫 “claudesktop (6).exe”，精确名对不上。
param(
  [string]$Name = 'claudesktop*',
  [string]$PathLike = 'claudesktop',   # 名字被改过也能按路径认出来
  [double]$Interval = 1,
  [string]$Out = '',
  [switch]$Tree,          # 连同子进程（内核 / MCP 服务器 / acp agent）一起算
  [switch]$Children,      # 每次打点列出子进程清单（配合 -Tree）
  [string]$Mark = '',     # 场景标记：写进 CSV 的 mark 列和打点行
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

# CSV 头（旧的 9 列 CSV 列数对不上：改名留档，重记新的）
if ((Test-Path $Out) -and -not ((Get-Content $Out -TotalCount 1) -like '*pb_tree_mb*')) {
  $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
  Move-Item $Out "$Out.$stamp.old"
}
if (-not (Test-Path $Out)) {
  'time,pid,ws_mb,pb_mb,peak_ws_mb,cpu_pct,threads,handles,ws_tree_mb,pb_tree_mb,nproc,mark' | Set-Content -Path $Out -Encoding UTF8
}

Write-Host ''
Write-Host "memwatch  target=$Name  interval=${Interval}s  tree=$($Tree.IsPresent)  mark=$Mark  cores=$cores" -ForegroundColor Cyan
Write-Host "log: $Out" -ForegroundColor DarkGray
Write-Host ''
$fmt = '{0,-8} {1,6} {2,8} {3,8} {4,8} {5,7} {6,6} {7,7} {8,8} {9,8} {10,5}'
Write-Host ($fmt -f 'time', 'pid', 'WS(MB)', 'PB(MB)', 'peak(MB)', 'cpu%', 'thr', 'hdl', 'treeWS', 'treePB', 'nprc') -ForegroundColor DarkGray
Write-Host ('-' * 88) -ForegroundColor DarkGray

$started = $null
$peak = 0.0
$peakPb = 0.0
$sumWs = 0.0
$samples = 0
$lastCpu = $null
$lastAt = $null
$waiting = $false
$tick = 0
$lastKids = ''

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
  $treePb = ''
  $nproc = ''
  if ($Tree) {
    $ids = Get-Tree $p.Id
    $wsSum = 0.0; $pbSum = 0.0
    $kids = @()
    foreach ($id in $ids) {
      $q = Get-Process -Id $id -ErrorAction SilentlyContinue
      if ($q) {
        $wsSum += $q.WorkingSet64; $pbSum += $q.PrivateMemorySize64
        if ($q.Id -ne $p.Id) { $kids += $q }
      }
    }
    $treeWs = [math]::Round($wsSum / 1MB, 1)
    $treePb = [math]::Round($pbSum / 1MB, 1)
    $nproc = $ids.Count
    # 子进程清单只在成员变了（或第一次）时列一次，别刷屏
    if ($Children) {
      $sig = ($kids | ForEach-Object { $_.ProcessName } | Sort-Object) -join ','
      if ($sig -ne $lastKids) {
        $lastKids = $sig
        if ($kids.Count -gt 0) {
          Write-Host ("  children ({0}):" -f $kids.Count) -ForegroundColor DarkGray
          foreach ($k in ($kids | Sort-Object -Property WorkingSet64 -Descending)) {
            Write-Host ("    {0,-24} pid={1,-7} WS={2,7}MB  PB={3,7}MB" -f $k.ProcessName, $k.Id, (MB $k.WorkingSet64), (MB $k.PrivateMemorySize64)) -ForegroundColor DarkGray
          }
        } else {
          Write-Host '  children: none' -ForegroundColor DarkGray
        }
      }
    }
  }

  $tick++
  if ($Quiet -le 1 -or ($tick % $Quiet) -eq 0) {
    $line = $fmt -f $now.ToString('HH:mm:ss'), $p.Id, $ws, $pb, $pk, $cpu, $p.Threads.Count, $p.HandleCount, $treeWs, $treePb, $nproc
    if ($Mark) { $line += "  [$Mark]" }
    Write-Host $line
  }
  ('{0},{1},{2},{3},{4},{5},{6},{7},{8},{9},{10},{11}' -f $now.ToString('yyyy-MM-dd HH:mm:ss'), $p.Id, $ws, $pb, $pk, $cpu, $p.Threads.Count, $p.HandleCount, $treeWs, $treePb, $nproc, $Mark) |
    Add-Content -Path $Out -Encoding UTF8

  Start-Sleep -Milliseconds ([int]($Interval * 1000))
}

# 走到这里说明进程没了
$mins = if ($started) { [math]::Round(((Get-Date) - $started).TotalMinutes, 1) } else { 0 }
Write-Host ''
Write-Host ('-' * 88) -ForegroundColor DarkGray
Write-Host ("'{0}' 已退出  观察 {1} 分钟 / {2} 次采样" -f $Name, $mins, $samples) -ForegroundColor Cyan
if ($samples -gt 0) {
  Write-Host ("峰值 WS = {0} MB   峰值 PB = {1} MB   平均 WS = {2} MB" -f $peak, $peakPb, [math]::Round($sumWs / $samples, 1)) -ForegroundColor Cyan
}
Write-Host "明细: $Out" -ForegroundColor DarkGray
