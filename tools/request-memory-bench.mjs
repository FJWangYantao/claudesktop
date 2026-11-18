import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { GatewayObserver, executableComparison, pairPassed, configureReceiveBuffer, configureBenchmarkClock } from './gateway-bench-observer.mjs';
import { mkdir, mkdtemp, readFile, writeFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { createWriteStream } from 'node:fs';
import { createInterface } from 'node:readline';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = Object.fromEntries(process.argv.slice(2).map(arg => {
  const [key, ...value] = arg.replace(/^--/, '').split('=');
  return [key, value.join('=')];
}));
const output = path.resolve(args.out || path.join(root, '.claudesktop/bench/request-memory/runs'));
assert(output.startsWith(path.join(root, '.claudesktop/bench') + path.sep));
await mkdir(output, { recursive: true });
assert.equal((await readdir(output)).length, 0, 'Use a new output directory.');
const versions = args.baseline && args.candidate
  ? { baseline: path.resolve(args.baseline), candidate: path.resolve(args.candidate) }
  : { baseline: path.resolve(args.exe) };
const { comparison, hashes: executableHashes } = await executableComparison(args.comparison, versions);
const toolCounts = (args['mcp-tools'] || '0').split(',').map(Number);
const loadouts = (args['loaded-tools'] || 'none').split(',');
const observationMode = args.observer || 'idle';
const highWaterMark = Number(args['high-water-mark'] || 16384);
const receiveBuffer = args['receive-buffer'] === undefined ? null : Number(args['receive-buffer']);
const controllerTimer = configureBenchmarkClock(args['precise-timers'], args['tcp-buffer-helper']);
assert(Number.isInteger(highWaterMark) && highWaterMark >= 16384 && highWaterMark <= 4 * 1024 * 1024);
assert(toolCounts.every(count => Number.isInteger(count) && count >= 0 && count <= 1000));
assert(loadouts.every(loadout => ['none', 'small', 'all'].includes(loadout)));
assert(['background', 'idle'].includes(observationMode));
const rounds = Number(args.rounds || 3);
const turns = Number(args.turns || 20);
assert.equal(process.platform, 'win32', 'This benchmark uses Windows process memory counters.');
assert(Number.isInteger(rounds) && rounds > 0);
assert(Number.isInteger(turns) && turns >= 20);
const protocols = (args.protocols || 'openai,anthropic,responses').split(',');
const sizes = (args.sizes || 'short,long').split(',');
assert(protocols.every(name => ['openai', 'anthropic', 'responses'].includes(name)));
assert(sizes.every(name => ['short', 'long'].includes(name)));
async function schemaStats(count) {
  if (count === 0) return { tools: 0, schema_bytes: 0 };
  const child = spawn(process.execPath, [path.join(root, 'tools/mock-mcp-server.mjs')], {
    env: { ...process.env, MOCK_MCP_SCHEMA_TOOLS: String(count), MOCK_MCP_SCHEMA_FIELDS: '64' },
    stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
  });
  let stderr = '';
  child.stderr.on('data', bytes => { stderr += bytes.toString(); });
  const closed = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  });
  const timeout = setTimeout(() => child.kill(), 10_000);
  const lines = createInterface({ input: child.stdout });
  try {
    child.stdin.end(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list', params: {} }) + '\n');
    let page;
    for await (const line of lines) {
      page = JSON.parse(line);
      break;
    }
    assert.equal(await closed, 0, stderr);
    assert.equal(page?.result?.tools?.length, count + 4);
    return {
      tools: page.result.tools.length,
      schema_bytes: page.result.tools.reduce((bytes, tool) => bytes + Buffer.byteLength(JSON.stringify(tool.inputSchema)), 0),
    };
  } finally {
    clearTimeout(timeout);
    lines.close();
    if (child.exitCode === null) child.kill();
  }
}
const schemas = Object.fromEntries(await Promise.all([...new Set(toolCounts)].map(async count => [count, await schemaStats(count)])));
await writeFile(path.join(output, 'schema-manifest.json'), JSON.stringify(schemas, null, 2));
global.gc?.();
const active = new Map();
const runs = [];
const percentile = (values, p) => {
  const sorted = values.toSorted((a, b) => a - b);
  assert(sorted.length > 0);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)];
};
function response(protocol, index) {
  const tool = index % 3 < 2;
  const id = `bench-call-${index}`;
  const parameters = JSON.stringify({ path: 'sample.txt' });
  if (protocol === 'openai') {
    const delta = tool
      ? { tool_calls: [{ index: 0, id, type: 'function', function: { name: 'read_file', arguments: parameters } }] }
      : { content: 'benchmark complete' };
    return [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] },
      { choices: [], usage: { prompt_tokens: 100, completion_tokens: 10 } },
    ];
  }
  if (protocol === 'anthropic') {
    const blocks = tool ? [
      { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: 'request thought', signature: 'request-signature' } },
      { type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id, name: 'read_file', input: {} } },
      { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: parameters } },
    ] : [
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'benchmark complete' } },
    ];
    return [
      { type: 'message_start', message: { usage: { input_tokens: 100 } } }, ...blocks,
      { type: 'message_delta', delta: { stop_reason: tool ? 'tool_use' : 'end_turn' }, usage: { output_tokens: 10 } },
      { type: 'message_stop' },
    ];
  }
  return [
    ...(tool ? [
      { type: 'response.output_item.added', item: { type: 'function_call', id, call_id: id, name: 'read_file', arguments: parameters } },
      { type: 'response.output_item.done', item: { type: 'function_call', id } },
    ] : [{ type: 'response.output_text.delta', delta: 'benchmark complete' }]),
    { type: 'response.completed', response: { status: 'completed', usage: { input_tokens: 100, output_tokens: 10 } } },
  ];
}

const server = createServer({ highWaterMark }, async (request, reply) => {
  try {
    const id = request.url.split('/')[2];
    const run = active.get(id);
    assert(run, request.url);
    const { buffer, length, observation } = await run.observer.receive(request, 'iterator');
    observation.socket_receive_buffer = receiveBufferOf(request.socket);
    const index = run.requests.length;
    run.requests.push(observation);
    run.observer.inspect(buffer, length, observation);
    await new Promise(resolve => setTimeout(resolve, 40));
    const body = response(run.protocol, index).map(event => `data: ${JSON.stringify(event)}\n\n`).join('') + 'data: [DONE]\n\n';
    reply.writeHead(200, { 'content-type': 'text/event-stream', 'content-length': Buffer.byteLength(body) });
    reply.end(body);
  } catch (error) {
    reply.writeHead(500, { 'content-type': 'application/json' });
    reply.end(JSON.stringify({ error: { message: String(error) } }));
  }
});
const receiveBufferOf = configureReceiveBuffer(server, receiveBuffer, args['tcp-buffer-helper']);
let connections = 0;
server.on('connection', () => { connections++; });
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

async function run(version, round, protocol, size, toolCount, loadout) {
  const id = `${protocol}-${size}-tools${toolCount}-${loadout}-${round}-${version}`;
  const directory = path.join(output, id);
  await mkdir(directory);
  const isolated = await mkdtemp(path.join(tmpdir(), 'claudesktop-request-memory-'));
  const loadedCount = loadout === 'all' ? toolCount : loadout === 'small' ? Math.min(toolCount, 3) : 0;
  const state = { directory, cwd: path.join(isolated, 'cwd'), protocol, requests: [] };
  state.observer = new GatewayObserver(state.cwd, observationMode, { rawOnly: false });
  const mcpConfig = path.join(isolated, 'mcp.json');
  if (toolCount > 0) {
    await writeFile(mcpConfig, JSON.stringify({ mcpServers: { bench: {
      command: process.execPath,
      args: [path.join(root, 'tools/mock-mcp-server.mjs')],
      env: { MOCK_MCP_SCHEMA_TOOLS: String(toolCount), MOCK_MCP_SCHEMA_FIELDS: '64' },
    } } }));
  }
  const initialConnections = connections;
  active.set(id, state);
  const stdout = createWriteStream(path.join(directory, 'stdout.log'));
  const stderr = createWriteStream(path.join(directory, 'stderr.log'));
  const child = spawn(versions[version], ['--ignored', '--exact', 'request_memory::real_request_memory', '--nocapture', '--test-threads=1'], {
    cwd: root,
    windowsHide: true,
    env: {
      ...Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('CLAUDESKTOP_'))),
      HTTP_PROXY: '', HTTPS_PROXY: '', ALL_PROXY: '', http_proxy: '', https_proxy: '', all_proxy: '',
      NO_PROXY: '127.0.0.1,localhost', no_proxy: '127.0.0.1,localhost',
      CLAUDESKTOP_DATA_DIR: path.join(isolated, 'data'),
      CLAUDESKTOP_REQUEST_BENCH_CWD: state.cwd,
      CLAUDESKTOP_REQUEST_BENCH_TURNS: String(turns),
      CLAUDESKTOP_REQUEST_BENCH_URL: `http://127.0.0.1:${server.address().port}/run/${id}/v1`,
      CLAUDESKTOP_REQUEST_BENCH_DIR: directory,
      CLAUDESKTOP_REQUEST_BENCH_PROTOCOL: protocol,
      CLAUDESKTOP_REQUEST_BENCH_SIZE: size,
      ...(toolCount ? { CLAUDESKTOP_REQUEST_BENCH_MCP_CONFIG: mcpConfig } : {}),
      CLAUDESKTOP_REQUEST_BENCH_MCP_TOOLS: String(toolCount),
      CLAUDESKTOP_REQUEST_BENCH_LOADED_TOOLS: String(loadedCount),
      CLAUDESKTOP_REQUEST_BENCH_OBSERVER: observationMode,
    },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  child.stdout.pipe(stdout);
  child.stderr.pipe(stderr);
  let idleChecks = Promise.resolve();
  let stdoutBuffer = '';
  let observerError;
  child.stdout.on('data', bytes => {
    stdoutBuffer += bytes.toString();
    let end;
    while ((end = stdoutBuffer.indexOf('\n')) >= 0) {
      const line = stdoutBuffer.slice(0, end).trim();
      stdoutBuffer = stdoutBuffer.slice(end + 1);
      if (!/GATEWAY_BENCH_IDLE \d+$/.test(line)) continue;
      idleChecks = idleChecks.then(async () => {
        await state.observer.finish();
        child.stdin.write('\n');
      }).catch(error => { observerError = error; child.kill(); });
    }
  });
  const timeout = setTimeout(() => child.kill(), 120_000);
  let code;
  let collected = false;
  try {
    code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
    await idleChecks;
    if (observerError) throw observerError;
    await state.observer.finish();
    collected = code === 0;
  } finally {
    clearTimeout(timeout);
    await state.observer.close();
    active.delete(id);
    await Promise.all([stdout, stderr].map(stream => new Promise(resolve => stream.end(resolve))));
    await writeFile(path.join(directory, 'observed-requests.json'), JSON.stringify(state.requests, null, 2));
    if (collected) await rm(isolated, { recursive: true, force: true });
    else await writeFile(path.join(directory, 'retained-fixture.json'), JSON.stringify({ isolated }, null, 2));
  }
  assert.equal(code, 0, `${id}: see ${directory}`);
  assert.equal(state.requests.length, turns * 3, id);
  assert(state.requests.every(request => request.stats.mcp_tools === loadedCount), `${id}: wrong MCP loadout`);
  const metrics = JSON.parse(await readFile(path.join(directory, 'metrics.json'), 'utf8'));
  assert.equal(metrics.request_ready.length, turns * 3, id);
  const sending = metrics.samples.filter(sample => sample.phase % 2 === 1);
  const result = {
    version, round, protocol, size, tool_count: toolCount, loadout, loaded_count: loadedCount,
    tcp_connections: connections - initialConnections,
    history_bytes: metrics.history_bytes, schema_bytes: schemas[toolCount].schema_bytes,
    before_pb: metrics.before.pb,
    peak_pb: metrics.after.peak_pb,
    peak_ws: metrics.after.peak_ws,
    active_pb: Math.max(...sending.map(sample => sample.pb)),
    after_pb: metrics.after.pb,
    after_ws: metrics.after.ws,
    idle_pb: Array.from({ length: turns }, (_, index) => percentile(metrics.samples.filter(sample => sample.phase === index * 2 + 2).map(sample => sample.pb), .5)),
    cpu_ms: metrics.after.cpu_ms - metrics.before.cpu_ms,
    preflight_p95_ms: percentile(metrics.turns.map((turn, index) => metrics.request_ready[index * 3].epoch_ms - turn.sent_at_ms), .95),
    transport_p95_ms: percentile(state.requests.map((request, index) => request.at_ms - metrics.request_ready[index].epoch_ms), .95),
    send_p95_ms: percentile(metrics.turns.map((turn, index) => state.requests[index * 3].at_ms - turn.sent_at_ms), .95),
    turn_p95_ms: percentile(metrics.turns.map(turn => turn.elapsed_ms), .95),
    requests: state.requests,
  };
  await writeFile(path.join(directory, 'result.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify({ id, peak_mib: result.peak_pb / 1048576, cpu_ms: result.cpu_ms, send_p95_ms: result.send_p95_ms, tcp_connections: result.tcp_connections }));
  return result;
}

try {
  for (let round = 1; round <= rounds; round++) for (const protocol of protocols) for (const size of sizes) for (const toolCount of toolCounts) for (const loadout of toolCount ? loadouts : ['none']) {
    const order = Object.keys(versions);
    if (round % 2 === 0) order.reverse();
    for (const version of order) {
      runs.push(await run(version, round, protocol, size, toolCount, loadout));
      await writeFile(path.join(output, 'runs.json'), JSON.stringify(runs, null, 2));
      global.gc?.();
    }
  }
  const pairs = [];
  if (versions.candidate) for (const baseline of runs.filter(run => run.version === 'baseline')) {
    const candidate = runs.find(run => run.version === 'candidate' && run.round === baseline.round && run.protocol === baseline.protocol && run.size === baseline.size && run.tool_count === baseline.tool_count && run.loadout === baseline.loadout);
    const same = baseline.requests.every((request, index) => request.hash === candidate.requests[index].hash);
    const sameBytes = baseline.requests.every((request, index) => request.normalized_raw_hash === candidate.requests[index].normalized_raw_hash);
    const pair = {
      round: baseline.round, protocol: baseline.protocol, size: baseline.size,
      tool_count: baseline.tool_count, loadout: baseline.loadout,
      identical_requests: same, identical_request_bytes: sameBytes,
      peak_saved_mib: (baseline.peak_pb - candidate.peak_pb) / 1048576,
      active_saved_mib: (baseline.active_pb - candidate.active_pb) / 1048576,
      peak_ratio: candidate.peak_pb / baseline.peak_pb,
      extra_send_p95_ms: candidate.send_p95_ms - baseline.send_p95_ms,
      extra_preflight_p95_ms: candidate.preflight_p95_ms - baseline.preflight_p95_ms,
      extra_turn_p95_ms: candidate.turn_p95_ms - baseline.turn_p95_ms,
      cpu_ratio: baseline.cpu_ms > 0 ? candidate.cpu_ms / baseline.cpu_ms : null,
      baseline_idle_growth_mib: (baseline.idle_pb.at(-1) - baseline.idle_pb[0]) / 1048576,
      candidate_idle_growth_mib: (candidate.idle_pb.at(-1) - candidate.idle_pb[0]) / 1048576,
    };
    pair.passed = same && pairPassed(pair, comparison, { requireLongMemory: true })
      && (comparison === 'aa' || pair.tool_count === 0 || pair.loadout !== 'all' || pair.peak_saved_mib > 0);
    pairs.push(pair);
  }
  const summary = {
    versions, executable_hashes: executableHashes, comparison, rounds, turns,
    tool_counts: toolCounts, loadouts, schemas, observer: observationMode, high_water_mark: highWaterMark,
    endpoint_receive_buffer: receiveBuffer, controller_timer_ms: controllerTimer,
    tcp_buffer_helper: args['tcp-buffer-helper'] ?? null,
    request_comparison: 'JSON semantics and raw bytes with the isolated fixture cwd normalized',
    runs, pairs, passed: pairs.length ? pairs.every(pair => pair.passed) : null,
  };
  await writeFile(path.join(output, 'summary.json'), JSON.stringify(summary, null, 2));
  console.log(JSON.stringify({ passed: summary.passed, pairs }, null, 2));
  if (summary.passed === false) process.exitCode = 1;
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
