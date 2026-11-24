#!/usr/bin/env node
// 跨内核标准评测的汇总脚本：读网关日志（`gateway/<日期>.jsonl`），按 session + turn 聚合，
// 打印对比表（请求数、工具调用占比、首包 token、缓存命中率、思考 token、耗时）。
//
// 用法：
//   node tools/kernel-bench-report.mjs --file <gateway/2026-11-25.jsonl>
//   node tools/kernel-bench-report.mjs --dir <数据目录>/gateway --session <会话 ID 前缀>
//   node tools/kernel-bench-report.mjs --file a.jsonl --file b.jsonl --json
//
// 日志每行一条请求（`LogRecord`，见 crates/builtin/src/gateway.rs）。旁路小请求
// （标题生成、记忆提炼、分类器、拉模型清单）默认单列一栏，不混进主轮次平均往返数。

import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const MAIN_PURPOSES = new Set(["turn"]);

function parseArgs(argv) {
  const opts = { files: [], dir: null, session: null, json: false, since: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      const value = argv[i + 1];
      if (value === undefined) throw new Error(`${arg} 需要一个参数`);
      i += 1;
      return value;
    };
    switch (arg) {
      case "--file":
        opts.files.push(next());
        break;
      case "--dir":
        opts.dir = next();
        break;
      case "--session":
        opts.session = next();
        break;
      case "--since":
        opts.since = next();
        break;
      case "--json":
        opts.json = true;
        break;
      case "--help":
      case "-h":
        printHelp();
        process.exit(0);
      // eslint-disable-next-line no-fallthrough
      default:
        throw new Error(`认不出的参数：${arg}`);
    }
  }
  if (!opts.files.length && !opts.dir) throw new Error("至少给 --file 或 --dir");
  return opts;
}

function printHelp() {
  console.log(`用法: node tools/kernel-bench-report.mjs [选项]

  --file <path>      网关日志（可重复，用来对比两个内核）
  --dir <path>       数据目录下的 gateway 目录，取其中全部 *.jsonl
  --session <前缀>   只看会话 ID 以该前缀开头的记录
  --since <日期>     只算该日期（含）之后的文件，YYYY-MM-DD，仅配合 --dir
  --json             输出机器可读的 JSON（按输入分组）
`);
}

/** 按 `--dir` / `--since` 收集日志文件（文件名就是日期，天然有序）。 */
function collectFiles(opts) {
  if (opts.files.length) return opts.files;
  const names = readdirSync(opts.dir)
    .filter((name) => name.endsWith(".jsonl"))
    .sort();
  return names
    .filter((name) => (opts.since ? name.slice(0, 10) >= opts.since : true))
    .map((name) => join(opts.dir, name));
}

/** 逐行读日志；坏行跳过并计数（正在写的最后一行常常是半截）。 */
function readLogs(files, sessionPrefix) {
  const rows = [];
  let bad = 0;
  for (const file of files) {
    let text;
    try {
      text = readFileSync(file, "utf8");
    } catch (error) {
      console.error(`跳过 ${file}：${error.message}`);
      continue;
    }
    for (const line of text.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      let row;
      try {
        row = JSON.parse(trimmed);
      } catch {
        bad += 1;
        continue;
      }
      if (sessionPrefix && !(row.session || "").startsWith(sessionPrefix)) continue;
      rows.push(row);
    }
  }
  return { rows, bad };
}

const pct = (part, whole) => (whole > 0 ? (part / whole) * 100 : 0);
const num = (value) => (typeof value === "number" && Number.isFinite(value) ? value : 0);

/** 一条记录属于哪个「回合」：同 session + turn 的请求归一组。 */
function groupKey(row) {
  const session = row.session || "(无会话)";
  const turn = Number.isInteger(row.turn) ? row.turn : -1;
  return `${session}\u0000${turn}`;
}

function summarize(rows) {
  const main = rows.filter((row) => MAIN_PURPOSES.has(row.purpose));
  const side = rows.filter((row) => !MAIN_PURPOSES.has(row.purpose));

  const groups = new Map();
  for (const row of main) {
    const key = groupKey(row);
    const group = groups.get(key) || { session: "", turn: -1, requests: 0 };
    group.session = row.session || "(无会话)";
    group.turn = Number.isInteger(row.turn) ? row.turn : -1;
    group.requests += 1;
    groups.set(key, group);
  }
  const turnCount = groups.size;
  const requestsPerTurn = turnCount > 0 ? main.length / turnCount : 0;

  const total = (list, field) => list.reduce((sum, row) => sum + num(row[field]), 0);
  const input = total(main, "input");
  const cacheRead = total(main, "cache_read");
  // 首包输入：一轮里第一次请求的 input（后续请求是带着历史的追加）
  const firstInput = [];
  for (const group of groups.values()) {
    const first = main
      .filter((row) => groupKey(row) === `${group.session}\u0000${group.turn}`)
      .sort((a, b) => num(a.at_ms) - num(b.at_ms))[0];
    if (first) firstInput.push(num(first.input));
  }
  const avgFirstInput =
    firstInput.length > 0 ? firstInput.reduce((a, b) => a + b, 0) / firstInput.length : 0;

  const durations = main
    .map((row) => num(row.total_ms))
    .filter((value) => value > 0)
    .sort((a, b) => a - b);

  return {
    requests: rows.length,
    mainRequests: main.length,
    sideRequests: side.length,
    turns: turnCount,
    requestsPerTurn: Number(requestsPerTurn.toFixed(2)),
    inputTokens: input,
    outputTokens: total(main, "output"),
    reasoningTokens: total(main, "reasoning"),
    cacheReadTokens: cacheRead,
    cacheHitPct: Number(pct(cacheRead, input + cacheRead).toFixed(1)),
    avgFirstInputTokens: Math.round(avgFirstInput),
    toolCalls: total(main, "tools"),
    errors: main.filter((row) => num(row.status) >= 400 || row.error).length,
    medianMs: durations.length ? durations[Math.floor(durations.length / 2)] : 0,
    p95Ms: durations.length ? durations[Math.min(durations.length - 1, Math.floor(durations.length * 0.95))] : 0,
    sideByPurpose: countBy(side, (row) => row.purpose || "(无)"),
    models: countBy(main, (row) => row.model || "(未记)"),
  };
}

function countBy(rows, keyOf) {
  const counts = {};
  for (const row of rows) {
    const key = keyOf(row);
    counts[key] = (counts[key] || 0) + 1;
  }
  return counts;
}

function printTable(label, summary, bad) {
  const pad = (name, value) => `${name.padEnd(22, " ")}${value}`;
  console.log(`\n== ${label}`);
  console.log(pad("请求数（总 / 主轮 / 旁路）", `${summary.requests} / ${summary.mainRequests} / ${summary.sideRequests}`));
  console.log(pad("轮数", summary.turns));
  console.log(pad("平均往返 / 轮", summary.requestsPerTurn));
  console.log(pad("输入 token", summary.inputTokens));
  console.log(pad("输出 token", summary.outputTokens));
  console.log(pad("思考 token", summary.reasoningTokens));
  console.log(pad("缓存读取 token", summary.cacheReadTokens));
  console.log(pad("缓存命中率", `${summary.cacheHitPct}%`));
  console.log(pad("首包输入 token（均值）", summary.avgFirstInputTokens));
  console.log(pad("声明的工具数（累计）", summary.toolCalls));
  console.log(pad("失败请求", summary.errors));
  console.log(pad("耗时 p50 / p95 (ms)", `${summary.medianMs} / ${summary.p95Ms}`));
  const side = Object.entries(summary.sideByPurpose);
  if (side.length) {
    console.log(pad("旁路请求", side.map(([name, count]) => `${name}:${count}`).join(" ")));
  }
  const models = Object.entries(summary.models);
  if (models.length) {
    console.log(pad("模型", models.map(([name, count]) => `${name}:${count}`).join(" ")));
  }
  if (bad > 0) console.log(pad("跳过坏行", bad));
}

function main() {
  const opts = parseArgs(process.argv.slice(2));
  const files = collectFiles(opts);
  if (!files.length) throw new Error("没找到日志文件");
  for (const file of files) {
    if (!statSync(file).isFile()) throw new Error(`${file} 不是文件`);
  }
  const results = [];
  for (const file of files) {
    const { rows, bad } = readLogs([file], opts.session);
    results.push({ label: file, summary: summarize(rows), bad });
  }
  // 多份日志时同时给一张合并的（同一个模型下对比两个内核）
  if (files.length > 1) {
    const merged = readLogs(files, opts.session);
    results.push({ label: "合并", summary: summarize(merged.rows), bad: merged.bad });
  }
  if (opts.json) {
    console.log(JSON.stringify(results, null, 2));
    return;
  }
  for (const { label, summary, bad } of results) printTable(label, summary, bad);
  if (files.length > 1) console.log("\n对比时注意：两次跑要同一模型、同一批题，否则缓存命中率不可比。");
}

main();
