# 设计说明：接入 DeepSeek 官方搜索

> 状态：**已实施**（2026-10）。代码落点：`crates/builtin/src/websearch.rs`（搜索实现与后端选择）、
> `crates/builtin/src/provider.rs`（`ProviderConfig::deepseek_key`）、`crates/builtin/src/lib.rs`
> （起内核时 `websearch::set_key_hint` / `set_data_dir`）、`apps/desktop/src/settings.rs`
> （`deepseek_search_key`、`is_deepseek_endpoint`）、`apps/desktop/src/app/settings_view.rs`
> （设置 →「权限」页签 →「网页搜索」一栏）。
>
> **第二节的三条已核实**（结论见第九节），是独立端点，不需要走第六节第 1 条那条保守路线。

## 一、要解决什么

`web_search` 现在只有两条路（`websearch.rs`）：

1. **Brave Search API**——质量好，但要用户自己申请 key（`CLAUDESKTOP_BRAVE_KEY` 或数据目录的 `brave_key` 文件）；
2. **DuckDuckGo HTML 版**——不要 key，但**经常被限流 / 返回空**，而且是抓 HTML，格式一变就失效。

对一个国内用户为主的桌面 harness 来说，这两条都不理想：Brave 要翻墙 + 办卡，DDG 时好时坏。
用户既然已经在用 DeepSeek（官方 API 就在国内、按量计费、key 已经在设置里），**把 DeepSeek 的搜索引擎接成第三个后端**是最划算的：
零额外配置、中文结果好、走的是已经填好的那把 key。

目标：

1. 设置页的「搜索」多一个后端选项，`DeepSeek` 复用当前 provider 的 DeepSeek API key（也可单独填一把）。
2. `web_search` 的后端选择顺序可配置：**DeepSeek → Brave → DuckDuckGo**（默认保持「有 Brave key 走 Brave」，不悄悄换用户的账单）。
3. 失败时**降级而不是报错**：DeepSeek 限流 / 超时 / 不支持时自动落到下一档，结果里如实说明用了哪个后端。

不做（本版）：

- 让 DeepSeek 的搜索结果**直接进模型上下文**（服务端搜索那种「一次请求里搜完」）。我们的内核是客户端循环，
  搜索是 `Action::Search` 一次工具调用，保持这个形状最省事，也便于权限流程和日志。
- 抓取（`web_fetch`）也换 DeepSeek。DeepSeek 的抓取接口是独立端点，等搜索落地稳定了再说（见第六节）。

## 二、DeepSeek 官方搜索的接口形状（**已核实**，结论见第九节）

最初列的三条不确定项与核实结果：

1. **是独立端点还是对话接口的服务端工具？** 需要的是**独立搜索 API**（「query in → 标题 / 网址 / 摘要 out」）——
   自己的模型循环要先拿到结果再决定下一步。
   → **两种都有；`POST /v1/tools/search` 就是独立端点**，采用它。
2. **鉴权与配额** → 同一把 `sk-` 平台 key；按次计费；超 QPS 返回 429（详见第九节）。
3. **返回结构** → `results[]`，字段 `title` / `url` / `snippet`（短）/ `summary`（长），正文要显式要；上限 50 条；无分页。

## 三、数据模型（实际实现与差异见第十节）

`websearch.rs` 原来是「有 Brave key 走 Brave，否则 DDG」的隐式规则（`search()` → `run(query, brave_key)`）。
现在改成显式的后端枚举，**降级顺序由 `SearchConfig::order()` 一处算**：

```rust
/// 声明顺序 = 固定的降级顺序（DeepSeek → Brave → DuckDuckGo）
pub enum SearchBackend { #[serde(rename = "deepseek")] DeepSeek, Brave, DuckDuckGo }

/// 存在数据目录的 `search.json`，只存选择，不存 key
pub struct SearchConfig {
    pub prefer: SearchBackend,              // 先用哪个
    pub allow_deepseek: bool,               // 默认关；有没有 key 不算允许
    pub allow_brave: bool,
    pub allow_duckduckgo: bool,
}
```

- 配置存在**数据目录**的 `search.json`，读写在 `websearch.rs`，`set_data_dir` 挂上去；
  `load()` 每次搜索现读，设置页改完即生效（搜索是即时读取的，不经 `Kernel::set_*`）。
- **默认值**：`prefer = Brave`、`allow_brave` / `allow_duckduckgo` 都开——和加后端之前的行为一致，
  升级不会悄悄花钱。DeepSeek 要在设置页选了（「也允许 DeepSeek」或「先用」它）才进降级链，
  **配了 key 不算允许**——不然「服务商」里有 DeepSeek 档案的用户，DuckDuckGo 一被限流就开始按次扣费。
- **key 的取法**（DeepSeek 后端）：数据目录的 `deepseek_key` 文件 / 环境变量 `CLAUDEDESKTOP_DEEPSEEK_KEY`
  → provider 配置里 DeepSeek 官方端点档案那把（只认 host 是 `api.deepseek.com` 的，`is_deepseek_endpoint`：
  搜索固定发往官方，中转站的 key 发过去用不了，还把凭据交给了别的服务器；`Settings::deepseek_search_key` 在起内核时经
  `ProviderConfig::deepseek_key` → `websearch::set_key_hint` 交过去）。
  第二条是「零配置」的关键：用户已经填过 DeepSeek 的 provider，就不该再填一次。
  取不到 key 时**跳过 DeepSeek、直接降级**，不报错（设置页状态行里有说明）。

## 四、实现

`websearch.rs` 加一个后端分支，形状和现有 Brave 的一致（都是「起线程 → 发请求 → 解析成 `Vec<Hit>`」）：

```rust
fn run(query: &str, order: &[SearchBackend]) -> Result<(SearchBackend, Vec<Hit>, Vec<String>), String> { … }
fn deepseek(query: &str, key: &str) -> Result<Vec<Hit>, String> { … }
```

几条约定，和现有代码保持一致：

1. **超时 20 秒**（复用 `SEARCH_TIMEOUT`），响应体上限 `SEARCH_MAX_BYTES`，结果最多 `SEARCH_MAX_RESULTS = 8` 条，
   摘要截到 `SNIPPET_CHARS = 300`。
2. **错误要译成能照着做的话**（AGENTS.md 的约定）：401 → 「key 无效，去设置页检查」；
   429 → 「限流了，稍等一会儿或换别的后端」；5xx → 「暂时不可用」。
   三个后端共用 `explain_search_error(backend, status, detail)`，有单测。
3. **降级要留痕**：`ToolOutput.content` **最前面**写一行，例如
   `(Brave skipped: no API key)` / `(DeepSeek failed: DeepSeek rate limited (HTTP 429)；…)`；
   工具行的汇总也变成「N 条结果（Brave，降级过）」——不点开也看得见降级了。
   模型看得见，用户也看得见——别让用户以为一直是 DeepSeek 在搜。
4. **不是 MCP**：DeepSeek 搜索是**我们自己的后端**，不走 `mcp__*` 那套，也不进权限规则里单列一条——
   它和 `web_search` 是同一个动作（`Action::Search`），权限、`hard_block`、日志、`ToolStep` 都不用改。
   这是选「在 `websearch.rs` 里加后端」而不是「做一个内置 MCP 服务器」的原因：后者要过 `McpHub`、
   要处理重连和 `McpStatus`，为一个 HTTP GET 不值得。
5. **不进 `QUIET_ENV`**：搜索是进程内的 HTTP，不弹窗口，和后台命令那套无关。
6. **结果格式沿用 `render`**（`1. 标题\n   网址\n   摘要`，末尾提醒可以 `web_fetch`）；
   DeepSeek 的 `summary` + `snippet` 拼起来后截到 300 字符。

## 五、设置页（已实施）

位置：设置 →「权限」页签 →「网页搜索」一栏（和「总是允许的操作」同一页）。用 `settings_kit.rs` 的
`section` + `setting_group`，顶部一行状态，下面是四行：

```
网页搜索
  当前：DeepSeek → Brave → DuckDuckGo（Brave 没配 key，会跳过它）
  先用哪个 ⓘ        [ DeepSeek | Brave | DuckDuckGo ]   ← seg_control
  也允许 Brave ⓘ                                 [开关]
  也允许 DuckDuckGo ⓘ                            [开关]
  Brave API key ⓘ    [ 输入框 ] [保存]
  DeepSeek key ⓘ     [ 输入框 ] [保存]   ← 留空就用「服务商」里 DeepSeek 那份的 key
```

- 「顺序可拖」对设置页来说太重：**「先用哪个」一个分段切换 + 三个开关「也允许 …」**（选了先用哪个，它的开关一起打开），
  允许的后端按固定顺序 `DeepSeek → Brave → DuckDuckGo` 排（`SearchConfig::order`）。默认 Brave + DuckDuckGo。
- DeepSeek key 那一行**不显示 provider 里的 key 内容**，只说「留空就用「服务商」里 DeepSeek 那份的 key」。
- 每项说明收进 ⓘ（`setting_row` 已经这样做），页面上不铺灰字。
- 状态行：`当前：Brave → DuckDuckGo`，缺 key 的后端后面补一句「（Brave 没配 key，会跳过它）」；
  一个都不允许时红字「所有后端都不可用，web_search 会直接报错」。
  **不做「测试」按钮**（要多一个异步请求、要处理窗口生命周期）；状态按「配置是否齐全」算，不联网验。
- 这一栏**不走底栏「保存」**：改了就写 `search.json`，下一轮搜索立即用新顺序（搜索是即时读取的）。

## 六、备选与不采用

1. **如果 DeepSeek 只有「对话接口里的服务端工具」**：在 `Action::Search` 里模拟一次最小的对话请求
   （system 一句「只输出 JSON 结果数组」+ user 是 query），把返回解析成 `Vec<Hit>`。
   **能不做就不做**：这是让一个模型去搜另一个模型，慢、贵、结果格式不稳，还要处理它会跑题。若确实只有这条路，
   方案是先做「DeepSeek 后端 = 让当前会话模型自己搜」，即**不加后端，改为在系统提示词里告诉模型
   『不知道的就去 web_search』**——等于放弃这条需求。
2. **不走 `websearch.rs` 的另一个做法：内置一个 MCP 服务器**（`tools/mock-mcp-server.mjs` 那套的正式版）。
   好处是用户可以自己删改、可以复用给 ACP 内核；坏处是要过 `McpHub` 的连接 / 重连 / 状态机，
   而 DeepSeek 搜索只是一个 HTTP 调用。**不采用**，除非以后要接四五家搜索并让用户自由组合。
3. **DeepSeek 官方如果以后发「服务端搜索」并支持 OpenAI 兼容协议**（像 Gemini 的 grounding）：
   那就变成 `RequestParams::server_tools` 那种形状（ANTHROPIC 那条 `SERVER_TOOLS` 的同类），
   和本方案不冲突，两个可以并存（`server_tools` 给 Anthropic 原生，grounding 给 DeepSeek）。

## 七、测试（已实施）

`websearch.rs` 的 `#[cfg(test)]`（这个文件不 `use gpui_kit::*`，可以裸 `#[test]`）：

1. `default_order_matches_old_behaviour`：默认 Brave → DDG；允许了 DeepSeek 但没选它先用时排最后；
   选它当 `prefer` 时排最前；只允许 DeepSeek、全关（空链）各一条。
2. `translates_search_errors`：401 / 402 / 429 / 5xx / 无状态码。
3. `parses_deepseek_results` / `deepseek_results_are_capped`：官方样例（脱敏）、字段缺省、超 8 条、空结果。
4. `long_snippets_are_clipped`、`fallback_is_written_into_the_result`（降级说明在正文最前面、
   汇总带「降级过」）、`search_json_round_trips_without_keys`（JSON 里没有 key，少字段也能读）。
5. 全部不联网：`cargo test -p claudesktop-builtin --lib -- websearch`（11 条）。
   真联网的那条挂 `#[ignore]`（和 `fetch::tests::live_fetch`、`catalog` 那套一样手动跑）——**本版还没加**。
6. `settings.rs`：`deepseek_search_key_reuses_a_deepseek_profile`（从 provider 档案挑 key）。
7. **手工（未做）**：在设置页切三种后端各搜一次中文 query（如「GPUI 渲染管线」），确认结果、耗时、降级提示。

## 八、落地顺序与工作量（实际）

1. 核实接口形状（第二节）：读官方文档，结论见第九节。
2. `websearch.rs`：`SearchBackend` / `SearchConfig` / `search.json` / 降级循环 / 错误翻译 / `deepseek()` 与解析。
3. key 传递：`ProviderConfig::deepseek_key` + `websearch::set_key_hint` + `Settings::deepseek_search_key`。
4. 设置页一栏（`settings_view.rs` 的 `render_web_search`）。
5. 单测。

原估 1.5 天，其中「第二节的核实」是最大不确定项——核完是独立端点，等于按简单那条路走完整套。

## 九、待核实清单（已回填）

- [x] **是独立端点**，不是对话接口的服务端工具：`POST https://api.deepseek.com/v1/tools/search`。
      文档：<https://api-docs.deepseek.com/guides/search>（中文版 `/zh-cn/guides/search`），
      接口参考 `/api/search`。核实日期 2026-10。同目录还有 `POST /v1/tools/fetch`（见第六节）。
- [x] 请求：`{"query": "...", "count": 8}`（`count` 1~50，默认 10；另有 `ori_question` /
      `localization` / `need_content`，本版没用）。返回：
      `{"results": [{rerank_score, title, url, snippet, summary, content, site_name, site_icon, date, authority}]}`。
      **`snippet` 短（100~200 字）、`summary` 长（600~800 字）**，两个都有；`content` 只有 `need_content: true` 才有。
      实现里摘要优先取 `summary`、后面括号带上 `snippet`，再截到 300 字（`SNIPPET_CHARS`）。
- [x] **同一把 `sk-` 平台 key**（`Authorization: Bearer`）；**按次计费**：搜索 / 抓取各 \$0.75 / 1000 次成功请求，
      失败、空结果、超时不收费，`count` 不影响单价。QPS 上限搜索 100、抓取 25（账号级，超了 429）。
      计费从 2026-11-01 00:00（北京时间）起——经 Harness / Claude Code / Codex 的服务端搜索此前免费。
      **这意味着「默认不选 DeepSeek」这条更重要**：它不是免费额度兜底，是新增账单。
- [x] 筛选参数走 **query 里的算子**，不是单独字段：`site:`（多个用 `OR` 连）、`-site:`、`-词`、`-"短语"`、
      `"短语"`、`lang:zh,en`、`after:2025-01-01`、`before:2026-06-01`。**所以 `web_search` 的 schema 不用加字段**，
      模型直接把算子写进 query 就行（需要时在工具描述里补一句）。
- [x] 单次上限 50 条（我们取 8）；**自带 `summary`**，但**不带正文**——要全文仍得 `web_fetch`，
      所以 `need_content: true` 没开（省流量，也不让工具结果里塞几千字）。
- [x] 错误形状同 OpenAI：`{"error": {message, type, param, code}}`，状态码 400 / 401 / 402（余额不足）/
      403 / 404 / 408 / 413 / 429 / 500 / 504，按官方说明 408、429、5xx 可重试。

## 十、实施时的取舍（和原方案的差异）

1. **`search.json` 只存选择，不存 key**。原方案想把它做成 `SearchConfig { order, deepseek_key, deepseek_base }`
   一起落盘，实际做成了：`search.json` 存 `prefer` / `allow_deepseek` / `allow_brave` / `allow_duckduckgo`；
   Brave key 仍在 `brave_key` 文件（不变），DeepSeek 搜索的 key 在 `deepseek_key` 文件（单独填时）；
   都没有就用 **provider 配置里 DeepSeek 档案那把**。最后这条经 `ProviderConfig::deepseek_key`
   在起内核时交给 `websearch::set_key_hint`——状态文件里放 key 既难和 provider 保持同步，
   也容易在备份 / 分享配置时漏出去。
2. **没有 `RwLock` 热更新**：`load()` 每次搜索现读 `search.json`，设置页改完即生效，少一套状态。
   只有 key 的 hint 是 `RwLock`（内核重建时换）。
3. **不用 `rename_all = "snake_case"`**：会给 `DeepSeek` 生成 `deep_seek`、`DuckDuckGo` 生成
   `duck_duck_go`，手写 JSON 很难看；改成逐项 `#[serde(rename)]`，写出来就是 `deepseek` / `brave` / `duckduckgo`。
4. **降级说明写在结果最前面**（不是末尾）：`(DeepSeek failed: DeepSeek rate limited (HTTP 429)；…)`，
   并且工具行的汇总改成「N 条结果（Brave，降级过）」，不点开也看得见降级了。
5. 错误翻译统一走 `explain_search_error(backend, status, detail)`（三个后端共用），单测覆盖 400/401/402/429/5xx/无状态码。
6. **`web_fetch` 换 DeepSeek 抓取**没做：`/v1/tools/fetch` 形状已核实（`{"url"}` → `{title, content}`），
   想接的话在 `fetch.rs` 加一个分支即可，本版留着。

## 十一、已做 / 没做

做完了：后端枚举与降级链、DeepSeek 后端、错误翻译、降级留痕、`search.json`、设置页一栏
（先用哪个 / 也允许 Brave / 也允许 DuckDuckGo / Brave key / DeepSeek key）、单测 11 条 + `deepseek_search_key` 1 条。

还没做（要真 key 才能验）：设置页切三种后端各搜一次中文 query 的**手工验证**；
以及 DeepSeek 后端只在单测里用假 JSON 验过，没发过真请求。
