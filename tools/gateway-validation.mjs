import {spawn} from 'node:child_process';
import {createWriteStream} from 'node:fs';
import {mkdir,open,readFile,writeFile,readdir,stat,statfs,realpath} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {hostname,cpus,totalmem,freemem,release,uptime} from 'node:os';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import assert from 'node:assert/strict';
import {sha256File,pairPassed} from './gateway-bench-observer.mjs';

const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const protocols=['openai','anthropic','responses'];
const kinds=['kernel','desktop'];
const contract={rounds:3,turns:20,latency_ms:20,peak_ratio:1.1,kernel_long_peak_must_drop:true,aa_absolute_latency:true};
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const json=file=>readFile(file,'utf8').then(text=>JSON.parse(text.replace(/^\uFEFF/,'')));
const save=(file,value)=>writeFile(file,JSON.stringify(value,null,2)+'\n');

function relativeFile(name) {
  assert(typeof name==='string'&&name.length>0&&!name.includes('\\')&&!name.includes(':')&&!path.posix.isAbsolute(name));
  assert(!name.split('/').some(part=>!part||part==='.'||part==='..'));
  const file=path.resolve(root,...name.split('/'));
  assert(file.startsWith(root+path.sep));
  return file;
}

async function command(file,args,{cwd=root,log,env=process.env}={}) {
  const child=spawn(file,args,{cwd,env,windowsHide:true,stdio:['ignore','pipe','pipe']});
  let stdout='',stderr='';
  const writer=log?createWriteStream(log):null;
  child.stdout.on('data',bytes=>{stdout+=bytes.toString('utf8');writer?.write(bytes)});
  child.stderr.on('data',bytes=>{stderr+=bytes.toString('utf8');writer?.write(bytes)});
  let code;
  try {code=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('close',resolve)});}
  finally {if(writer) await new Promise(resolve=>writer.end(resolve));}
  return {code,stdout,stderr};
}

async function environment() {
  assert.equal(process.platform,'win32','需要 Windows x64。');
  assert.equal(process.arch,'x64','需要 x64 Node.js。');
  const script=`[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $ErrorActionPreference='Stop'; $c=Get-CimInstance Win32_ComputerSystem; $o=Get-CimInstance Win32_OperatingSystem; $u=(Get-CimInstance Win32_ComputerSystemProduct).UUID; $g=@(Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion); @{uuid=[string]$u;os=$o.Caption;build=$o.BuildNumber;manufacturer=$c.Manufacturer;model=$c.Model;hypervisor=$c.HypervisorPresent;graphics=$g} | ConvertTo-Json -Depth 4 -Compress`;
  const result=await command('powershell.exe',['-NoLogo','-NoProfile','-NonInteractive','-Command',script]);
  assert.equal(result.code,0,'无法读取 Windows 验证环境信息。');
  let details;
  try {details=JSON.parse(result.stdout.replace(/^\uFEFF/,''));}
  catch {throw new Error('Windows 环境信息格式不正确。');}
  const hostId=hash(`gateway-host-v1\0${String(details.uuid).trim().toLowerCase()}\0${hostname().toLowerCase()}`);
  delete details.uuid;
  return {host_id:hostId,identity_basis:'system-uuid-and-hostname-sha256',...details,node:process.version,arch:process.arch,windows_release:release(),cpu:cpus()[0]?.model,logical_cpus:cpus().length,total_memory:totalmem(),free_memory:freemem(),uptime_seconds:uptime(),at:new Date().toISOString()};
}

async function assertPe(file) {
  const handle=await open(file,'r');
  try {
    const head=Buffer.alloc(64);assert.equal((await handle.read(head,0,64,0)).bytesRead,64);
    assert.equal(head.toString('ascii',0,2),'MZ',`不是 Windows exe：${path.basename(file)}`);
    const offset=head.readUInt32LE(60);assert(offset>=64&&offset<1024*1024);
    const pe=Buffer.alloc(6);assert.equal((await handle.read(pe,0,6,offset)).bytesRead,6);
    assert.equal(pe.readUInt32LE(0),0x4550);assert.equal(pe.readUInt16LE(4),0x8664,'需要 Windows x64 产物。');
  } finally {await handle.close();}
}

export function makePlan(kind,stage) {
  assert(kinds.includes(kind));assert(['smoke','aa','ab'].includes(stage));
  return {kind,stage,rounds:stage==='smoke'?1:3,turns:20,protocols:[...protocols],sizes:stage==='ab'&&kind==='kernel'?['short','long']:[stage==='smoke'&&kind==='kernel'?'short':'long'],saves:stage==='ab'?['on','off']:['on'],versions:stage==='smoke'?['baseline']:['baseline','candidate']};
}

function matrix(plan,withVersion) {
  const keys=[];
  for(let round=1;round<=plan.rounds;round++) for(const protocol of plan.protocols) for(const size of plan.sizes) for(const save of plan.saves) {
    const prefix=[round,protocol,size,save].join('/');
    if(withVersion) for(const version of plan.versions) keys.push(`${prefix}/${version}`);else keys.push(prefix);
  }
  return keys;
}

export function assessComponent(summary,plan,expectedHashes,condition) {
  assert.equal(summary.rounds,plan.rounds);assert.equal(summary.turns,20);
  assert.equal(summary.comparison,plan.stage==='aa'?'aa':'ab');
  assert.deepEqual(summary.executable_sha256,expectedHashes);
  assert.equal(summary.observer,'pooled-idle');assert.equal(summary.observer_gc,true);
  assert.equal(summary.high_water_mark,1048576);
  assert.equal(summary.endpoint_receive_buffer,condition==='calibrated'?1048576:null);
  assert.equal(summary.controller_timer_ms,condition==='calibrated'?1:null);
  assert(!summary.profiling,'不接受带分段打点的桌面构建。');
  const expectedRuns=matrix(plan,true);
  const actualRuns=summary.runs.map(run=>[run.round,run.protocol,run.size,run.save,run.version].join('/'));
  assert.deepEqual([...actualRuns].sort(),[...expectedRuns].sort(),'运行矩阵不完整或有重复。');
  const requestCount=3*(20+(plan.kind==='desktop'?1:0));
  for(const run of summary.runs) {
    assert.equal(run.requests.length,requestCount);assert.equal(run.verified_statistics,requestCount);
    assert.equal(run.verified_saved_bodies,run.save==='on'?requestCount:0);
    assert(run.requests.every(request=>/^[0-9a-f]{64}$/.test(request.raw_hash)&&request.bytes>0));
    if(run.size==='long') assert(run.requests.every(request=>request.bytes>=10*1024*1024));
    for(const field of ['peak_pb','peak_ws','cpu_ms','send_p95_ms','turn_p95_ms']) assert(Number.isFinite(run[field])&&run[field]>=0,`无效指标：${field}`);
    assert(run.peak_pb>0);
    if(plan.kind==='desktop') {
      assert.equal(run.hidden,true);assert.equal(run.foreground,false);assert.equal(run.profile_records,0);
      assert.equal(run.diagnostic_data_in_output,false);assert.equal(run.side_requests.length,1);
    }
    if(condition==='calibrated') assert(run.requests.every(request=>request.socket_receive_buffer?.after>=1048576));
  }
  const expectedPairs=plan.stage==='smoke'?[]:matrix(plan,false);
  const actualPairs=summary.pairs.map(pair=>[pair.round,pair.protocol,pair.size,pair.save].join('/'));
  assert.deepEqual([...actualPairs].sort(),[...expectedPairs].sort(),'配对矩阵不完整或有重复。');
  const failed=summary.pairs.filter(pair=>!pairPassed(pair,summary.comparison,{requireLongMemory:plan.kind==='kernel'}));
  assert(summary.pairs.every(pair=>pair.passed===pairPassed(pair,summary.comparison,{requireLongMemory:plan.kind==='kernel'})),'组件门槛与验收门槛不同。');
  assert.equal(summary.passed,plan.stage==='smoke'?null:failed.length===0);
  return {functional_passed:true,passed:plan.stage==='smoke'?true:failed.length===0,main_requests:summary.runs.length*requestCount,saved_body_pairs:summary.runs.reduce((sum,run)=>sum+run.verified_saved_bodies,0),pairs:summary.pairs.length,failed_pairs:failed};
}

export function canStartAb(results,selected) {
  return selected.every(kind=>results.some(result=>result.kind===kind&&result.stage==='aa'&&result.passed===true&&result.functional_passed===true));
}

async function verifyBundle() {
  const manifestBytes=await readFile(relativeFile('manifest.json'));
  const manifest=JSON.parse(manifestBytes.toString('utf8'));
  assert.equal(manifest.schema,1);assert.deepEqual(manifest.contract,contract);
  assert.equal(process.version,manifest.runtime.version,'请使用包内 runtime/node.exe。');
  const sums=new Map((await readFile(relativeFile('SHA256SUMS.txt'),'utf8')).trim().split(/\r?\n/).map(line=>{
    const match=/^([0-9a-f]{64})  (.+)$/.exec(line);assert(match,'校验清单格式错误。');return [match[2],match[1]];
  }));
  assert.equal(sums.get('manifest.json'),hash(manifestBytes),'manifest.json 校验失败。');
  const names=new Set();
  for(const entry of manifest.files) {
    const key=entry.path.toLowerCase();assert(!names.has(key),'文件清单有重复。');names.add(key);
    const file=relativeFile(entry.path);assert.equal((await stat(file)).size,entry.bytes,`文件长度不符：${entry.path}`);
    assert.equal(sums.get(entry.path),entry.sha256,`清单不符：${entry.path}`);
    assert.equal(await sha256File(file),entry.sha256,`文件校验失败：${entry.path}`);
  }
  assert.equal(sums.size,manifest.files.length+1,'校验清单项目数不同。');
  const roleFiles=Object.values(manifest.binaries).flatMap(group=>Object.values(group));
  for(const name of [...roleFiles,manifest.runtime.path]) assert(names.has(name.toLowerCase()),'产物不在校验清单内。');
  if((await realpath(process.execPath)).toLowerCase()!==(await realpath(relativeFile(manifest.runtime.path))).toLowerCase()) {
    const runtime=manifest.files.find(entry=>entry.path===manifest.runtime.path);
    assert.equal(await sha256File(process.execPath),runtime.sha256,'运行时与包内 Node.js 不同。');
  }
  return manifest;
}

async function pack(args) {
  assert.equal(process.platform,'win32');assert.equal(process.arch,'x64');
  const builds=await json(path.resolve(root,args.builds??'.claudesktop/bench/gateway-memory/receive-fixed-builds.json'));
  assert(args['baseline-desktop']&&args['node-license'],'指定 --baseline-desktop 和 --node-license。');
  const output=path.resolve(root,args.out??'.claudesktop/bench/gateway-validation-kit.zip');
  assert(output.startsWith(path.join(root,'.claudesktop','bench')+path.sep),'打包产物放在项目 bench 目录。');
  const staging=path.join(path.dirname(output),`gateway-kit-input-${Date.now()}-${process.pid}`);
  await mkdir(staging,{recursive:true});
  const entries=[];
  async function add(name,source,expected) {
    source=path.resolve(root,source);
    assert(!entries.some(entry=>entry.path.toLowerCase()===name.toLowerCase()));
    if(name.endsWith('.exe')||name.endsWith('.node')) await assertPe(source);
    const bytes=(await stat(source)).size;const sha256=await sha256File(source);
    if(expected) assert.equal(sha256,expected,`冻结产物已变化：${name}`);
    entries.push({path:name,source:path.resolve(source),bytes,sha256});
  }
  const binaries={desktop:{baseline:'artifacts/desktop/baseline/claudesktop.exe',candidate:'artifacts/desktop/candidate/claudesktop.exe',parser_baseline:'artifacts/desktop/parser-baseline/claudesktop.exe'},kernel:{baseline:'artifacts/kernel/baseline.exe',candidate:'artifacts/kernel/candidate.exe',parser_baseline:'artifacts/kernel/parser-baseline.exe'}};
  await add(binaries.desktop.baseline,path.resolve(args['baseline-desktop']),builds.baseline_sha256);
  await add(binaries.desktop.candidate,builds.desktop.destination,builds.desktop.sha256);
  await add(binaries.desktop.parser_baseline,builds.parse_only_baseline.desktop,builds.parse_only_baseline.desktop_sha256);
  await add(binaries.kernel.baseline,builds.idle_baseline_kernel.destination,builds.idle_baseline_kernel.sha256);
  await add(binaries.kernel.candidate,builds.snapshot_kernel.destination,builds.snapshot_kernel.sha256);
  await add(binaries.kernel.parser_baseline,builds.parse_only_baseline.kernel,builds.parse_only_baseline.kernel_sha256);
  await add('runtime/node.exe',process.execPath);
  await add('licenses/node.LICENSE',path.resolve(args['node-license']));
  await add('licenses/claudesktop.LICENSE',path.join(root,'LICENSE'));
  for(const name of ['gateway-validation.mjs','gateway-validation-archive.ps1','gateway-desktop-bench.mjs','gateway-memory-bench.mjs','gateway-bench-observer.mjs','gateway-desktop-driver.ps1','gateway-bench-affinity.ps1','request-memory-worker.mjs']) await add(`tools/${name}`,path.join(root,'tools',name));
  await add('native/tcp-buffer-helper.node',path.join(root,'.claudesktop/bench/gateway-memory/tcp-buffer-helper.node'));
  await add('native/tcp-buffer-helper.cc',path.join(root,'.claudesktop/bench/gateway-memory/tcp-buffer-helper.cc'));
  const launcher=path.join(staging,'run.cmd');
  await writeFile(launcher,'@echo off\r\n"%~dp0runtime\\node.exe" "%~dp0tools\\gateway-validation.mjs" %*\r\nexit /b %errorlevel%\r\n');
  await add('run.cmd',launcher);
  const manifest={schema:1,created_at:new Date().toISOString(),purpose:'网关内存候选的独立 Windows 验收；不代表已通过性能门槛',contract,runtime:{path:'runtime/node.exe',version:process.version,arch:process.arch,license_source:`https://github.com/nodejs/node/blob/v${process.versions.node}/LICENSE`},reference_environment:await environment(),binaries,sets:{combined:'旧 Value 统计和默认接收缓冲，对比按需统计和 1 MiB 接收缓冲',parser:'两边固定 1 MiB 网关接收缓冲，只比较统计解析方式'},files:entries.map(({source,...entry})=>entry)};
  const manifestFile=path.join(staging,'manifest.json');await save(manifestFile,manifest);await add('manifest.json',manifestFile);
  const checksums=path.join(staging,'SHA256SUMS.txt');
  await writeFile(checksums,entries.map(entry=>`${entry.sha256}  ${entry.path}`).join('\n')+'\n');await add('SHA256SUMS.txt',checksums);
  const archivePlan=path.join(staging,'archive-plan.json');await save(archivePlan,entries);
  const disk=await statfs(path.dirname(output));
  assert(disk.bavail*disk.bsize>entries.reduce((sum,entry)=>sum+entry.bytes,0)+64*1024*1024,'空间不足：先释放本任务的可重建输出。');
  const result=await command('powershell.exe',['-NoLogo','-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(root,'tools/gateway-validation-archive.ps1'),'-Plan',archivePlan,'-Output',output]);
  assert.equal(result.code,0,result.stderr||result.stdout);
  const archive=JSON.parse(result.stdout.replace(/^\uFEFF/,''));
  const record={archive:output,...archive,manifest_sha256:await sha256File(manifestFile),files:entries.length,reference_host:manifest.reference_environment.host_id};
  await save(`${output}.json`,record);console.log(JSON.stringify(record,null,2));
}

async function runSuite(mode,args) {
  const manifest=await verifyBundle();
  const scope=args.scope??'all';const set=args.set??'combined';const condition=args.condition??'reference';
  assert(['all',...kinds].includes(scope));assert(['combined','parser'].includes(set));assert(['reference','calibrated'].includes(condition));
  const selected=scope==='all'?kinds:[scope];
  const output=path.resolve(root,args.out??`.claudesktop/bench/validation-${Date.now()}`);
  assert(output.startsWith(path.join(root,'.claudesktop','bench')+path.sep),'结果必须位于包内 bench 目录。');
  await mkdir(output,{recursive:true});assert.equal((await readdir(output)).length,0,'请使用新的结果目录。');
  const result={mode,scope,set,condition,contract,started_at:new Date().toISOString(),independent:false,status:'initializing',results:[],ab_started:false,performance_passed:mode==='smoke'?null:false,full_combined_passed:false};
  const fileEntries=new Map(manifest.files.map(entry=>[entry.path,entry]));
  async function component(kind,stage,role) {
    const plan=makePlan(kind,stage);const group=manifest.binaries[kind];
    const candidate=stage==='smoke'?group[role]:group.candidate;
    const baseline=stage==='aa'?group.candidate:group[set==='parser'?'parser_baseline':'baseline'];
    const directory=path.join(output,`${stage}-${kind}${role?`-${role}`:''}`);
    const expectedHashes=stage==='smoke'?{baseline:fileEntries.get(candidate).sha256}:{baseline:fileEntries.get(baseline).sha256,candidate:fileEntries.get(candidate).sha256};
    if(stage==='ab') assert.notEqual(expectedHashes.baseline,expectedHashes.candidate,'A/B 不能使用相同产物。');
    const flags=[`--out=${directory}`,`--rounds=${plan.rounds}`,'--turns=20',`--protocols=${plan.protocols.join(',')}`,`--sizes=${plan.sizes.join(',')}`,`--save=${plan.saves.join(',')}`,'--observer=idle','--high-water-mark=1048576',`--comparison=${stage==='aa'?'aa':'ab'}`];
    if(stage==='smoke') flags.push(`--exe=${relativeFile(candidate)}`);else flags.push(`--baseline=${relativeFile(baseline)}`,`--candidate=${relativeFile(candidate)}`);
    if(condition==='calibrated') flags.push('--receive-buffer=1048576','--precise-timers=1',`--tcp-buffer-helper=${relativeFile('native/tcp-buffer-helper.node')}`);
    console.log(`${stage} ${kind}${role?` ${role}`:''}: 开始`);
    const script=relativeFile(`tools/${kind==='kernel'?'gateway-memory-bench.mjs':'gateway-desktop-bench.mjs'}`);
    const child=await command(process.execPath,['--expose-gc',script,...flags],{log:`${directory}.log`});
    let summary;
    try {summary=await json(path.join(directory,'summary.json'));}
    catch {throw new Error(`${stage} ${kind} 未完成内容校验，查看 ${path.basename(directory)}.log`);}
    const assessed=assessComponent(summary,plan,expectedHashes,condition);
    assert.equal(child.code,stage==='smoke'||assessed.passed?0:1,'组件退出状态异常。');
    const record={kind,stage,...(role?{role}:{}),...assessed,directory:path.relative(output,directory).replaceAll('\\','/')};
    result.results.push(record);await save(path.join(output,'summary.json'),result);
    console.log(`${stage} ${kind}${role?` ${role}`:''}: ${assessed.passed?'通过':'未通过'}`);
    return record;
  }
  try {
    const host=await environment();result.independent=host.host_id!==manifest.reference_environment.host_id;
    await save(path.join(output,'environment.json'),host);
    if(mode==='run'&&!result.independent) {result.status='blocked_reference_host';throw new Error('当前仍是原测量机器，独立性能验收未启动；smoke 只做功能快查。');}
    const disk=await statfs(output);assert(disk.bavail*disk.bsize>(mode==='run'?1024:128)*1024*1024,'结果盘可用空间不足。');
    if(mode==='smoke') {
      for(const kind of selected) for(const role of ['candidate','baseline','parser_baseline']) await component(kind,'smoke',role);
      result.status='functional_smoke_passed';
    } else {
      result.status='calibrating';
      for(const kind of selected) {
        if(!(await component(kind,'aa')).passed) {result.status='calibration_failed';break;}
      }
      if(canStartAb(result.results,selected)) {
        result.ab_started=true;result.status='benchmarking';
        for(const kind of selected) await component(kind,'ab');
        result.performance_passed=selected.every(kind=>result.results.some(row=>row.kind===kind&&row.stage==='ab'&&row.passed));
        result.full_combined_passed=result.performance_passed&&scope==='all'&&set==='combined'&&condition==='reference';
        result.status=result.performance_passed?'performance_passed':'performance_failed';
      }
    }
  } catch(error) {
    if(result.status!=='blocked_reference_host') result.status='verification_failed';
    result.error=String(error.message??error);
  }
  result.finished_at=new Date().toISOString();
  await save(path.join(output,'summary.json'),result);
  console.log(JSON.stringify(result,null,2));
  if(mode==='smoke'?result.status!=='functional_smoke_passed':!result.performance_passed) process.exitCode=1;
}

function help() {
  console.log(`独立 Windows 验收包\n\n  run.cmd verify\n  run.cmd smoke --scope=all\n  run.cmd run --scope=all --set=combined --condition=reference\n\nverify 核对全部文件和 Node.js。smoke 在本机只做真实功能快查。\nrun 要求与原机器不同；先做三轮 A/A，任一组绝对差超过 20ms 就停止，不启动 A/B。\nA/B 保留候选额外等待 20ms、峰值比 1.1、内核长历史峰值必须下降的门槛。\n默认覆盖内核与普通隐藏桌面、三协议、长短历史和原文开关。\n--set=parser 只隔离统计解析；--condition=calibrated 显式启用端点缓冲与控制时钟，结果单独标记。\n包内运行时和产物均被冻结，不会重新构建或修改用户应用设置。`);
}

async function main() {
  const mode=process.argv[2]??'help';
  if(mode==='help'||mode==='--help') {help();return;}
  assert(['pack','verify','smoke','run'].includes(mode),'使用 pack / verify / smoke / run。');
  const allowed=mode==='pack'?['builds','baseline-desktop','node-license','out']:mode==='verify'?[]:['scope','set','condition','out'];
  const args={};
  for(const item of process.argv.slice(3)) {
    const match=/^--([^=]+)=(.+)$/.exec(item);assert(match&&allowed.includes(match[1]),`未知参数：${item}`);
    assert(!(match[1] in args),`重复参数：${match[1]}`);args[match[1]]=match[2];
  }
  if(mode==='pack') return pack(args);
  if(mode==='verify') {
    const manifest=await verifyBundle();console.log(JSON.stringify({integrity_passed:true,files:manifest.files.length,node:manifest.runtime.version,performance_passed:null},null,2));return;
  }
  return runSuite(mode,args);
}

if(process.argv[1]&&path.resolve(process.argv[1]).toLowerCase()===fileURLToPath(import.meta.url).toLowerCase()) main().catch(error=>{console.error(String(error.message??error));process.exitCode=1;});
