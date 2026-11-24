# 调研报告：Kimi Code Desktop 前端架构与设计美学拆解

> 目标物：`E:\kimicode\Kimi Code\Kimi Code.exe`（`kimi-code-app` v1.3.4，`NOVASCENT PRIVATE LIMITED` / Moonshot AI）
> 产物位置：`E:\kimicode\Kimi Code\resources\app.asar`（主进程与 Preload）+ `E:\kimicode\Kimi Code\resources\desktop-dist\`（未加密 Vite + Vue 3 构建产物，681 个文件，约 29.7 MB，无 `.map`）
> 方法：REA 静态分析（`inspect_electron_fuses`、`analyze_javascript_application`、`inspect_analysis_view`）+ `app.asar` / `desktop-dist` 源码与 CSS Tokens 直接核对。未执行目标程序。
> 证据编号：
> - Electron Fuses：`ev_d7f9408a52b2607c59560bb8cc7867888a6a56bb9396486d7c6fcdb5625e3991`
> - `app.asar` 主进程与 Preload：`ev_f4f5249f7e3f608e0b9c97f92a4d92373b2f46c54322fa544a5cd959c26e90e9`、`ev_be2c3031e1ea65a290fe22c04d2b4be71afe4311450ef0e08d0be9ec065bfbe9`
> - `desktop-dist` 前端产物：`ev_fa5d871fc4cb02e4928f6959c61986b31c47e7d0b541208b657ed2ae9ad2201b`

---

## 一、核心结论

1. **发行形态（非典型 Electron）**：前端静态资源并不在 `app.asar` 内，而是完整明文置于 `resources\desktop-dist\`，由主进程在 `127.0.0.1` 随机端口启动的内嵌 HTTP/WebSocket 服务托管，主窗口通过 `#token=<credential>` 完成首跳鉴权后写入 `localStorage["kimi-web.server-credential"]` 并用 `history.replaceState` 擦除 URL hash。
2. **渲染技术栈**：基于 **Vue 3**（Scoped CSS `[data-v-*]`）+ **markstream-vue**（Markdown 流式渲染）+ **xterm.js**（WebGL 终端）+ **Monaco / Shiki / Mermaid / KaTeX**。
3. **设计美学核心**：**“高信息密度但不压抑的极简桌面工作台”**——无明显冷暖偏色的中性/碳灰三层表面、`0.5px` 细线（低 DPI 回退 `1px`）、`760px` 居中/左对齐阅读列、`superellipse(1.5)` 超椭圆输入框（32px 圆角 + 同心圆角推导）、三套内置可变字体（`Schibsted Grotesk Variable` + `Noto Sans SC Variable` + `JetBrains Mono Variable`）以及 `round(..., 1px)` 像素取整行高。

---

## 二、前端运行架构与窗口体系

### 2.1 Electron Fuses 与进程桥接

- `Kimi Code.exe`（`ev_d7f9408a52b2607c59560bb8cc7867888a6a56bb9396486d7c6fcdb5625e3991`）：
  - `RunAsNode = enabled`
  - `EnableNodeOptionsEnvironmentVariable = enabled`
  - `EnableNodeCliInspectArguments = enabled`（支持 `--inspect` / CDP 调试）
  - `EnableEmbeddedAsarIntegrityValidation = disabled`
  - `OnlyLoadAppFromAsar = disabled`
- 主进程引导链：`out/main.cjs` → `out/app-CAg3Zj5I.cjs` → `out/screenshot-0HyrBsGA.cjs`（`oZ()` 建窗、`eq()` 连接服务、`QG()`/`lD()` 启本地服务器）。
- 所有渲染窗口均启用 `contextIsolation: true`、`nodeIntegration: false`、`sandbox: true`，通过 5 个 Preload 暴露 5 组 `contextBridge` API（`ev_709e32a9289a019300637fc17edb2e923cfe679e6797567d5a9149702af46bf1`）：

| Preload 文件 | 暴露对象 | 规模与职责 |
|---|---|---|
| `out/preload.cjs` | `window.kimiDesktop` | 152 个成员 / 150 个 `kimi:*` IPC 通道（窗口、主题/毛玻璃、JumpList、自动更新、`node-pty` 原生终端、截图热键、内置浏览器控制、OAuth、模型拉取） |
| `out/browser-control-preload.cjs` | `window.kimiBrowserControl` | 8 个成员：内置浏览器工具条（下载、弹窗、接管 `takeOver`、活动状态） |
| `out/browser-overlay-preload.cjs` | `window.kimiBrowserOverlay` | 9 个成员：内置浏览器浮层菜单与表单弹窗 |
| `out/shotPreload.cjs` | `window.shotBridge` | 16 个成员：截图选区、取色、文本标注 |
| `out/shotPinPreload.cjs` | `window.pinBridge` | 7 个成员：截图置顶浮窗移动与边缘缩放 |

### 2.2 6 个前端 HTML 入口（`resources\desktop-dist\`）

| 入口 HTML | 主 JS / CSS | 职责 |
|---|---|---|
| `index.html` | `assets/main-BpsjxRzO.js`（3.66 MB）+ `main-BFccKBrK.css`（649 KB） | 主界面 Vue 3 SPA（`#app`） |
| `browser-control.html` | `assets/browserControl-C53riLgA.js` | 内置浏览器控制栏 |
| `browser-overlay.html` | `assets/browserOverlay-DVSmpo4h.js` | 内置浏览器浮层菜单/弹窗 |
| `screenshot/index.html` | `assets/screenshot-nIbhBlaH.js`（336 KB） | 屏幕截图与标注浮层 |
| `pin/index.html` | `assets/pin-IZLnr88_.js` | 置顶悬浮截图窗 |
| `capture-host/index.html` | `assets/captureHost--d2YPaEL.js` | MediaStream 屏幕捕获宿主页 |

### 2.3 后端通信与持久化键

- **REST**：`main-BpsjxRzO.js` 调用 `/api/v1` 与 `/api/v2`（105 处静态观察），覆盖 `/sessions`（fork/archive/restore/compact/undo/takeover/steering/file-history/goal/skills/children）、`/workspaces`、`/fs:*`、`/workspace/fs:*`、`/providers`、`/plugins`、`/mcp/auth*`、`/oauth/*`、`/remote-control*`；后端还挂有 `/openapi.json`、`/asyncapi.json`、`/healthz`。
- **WebSocket**：`ws://127.0.0.1:<port>/api/v1/ws`。
- **LocalStorage**：约 92 个 `kimi-*` / `kimi-web.*` 键，包含外观（`color-scheme`、`font-scale`、`fonts`、`accent`、`content-align`）、布局尺寸（`sidebar-width`、`sidebar-collapsed`、`file-tree-width`、`file-tree-position`、`file-preview-width`、`terminal-panel-height`）及交互模式（`plan-mode`、`swarm-mode`、`tower-mode`、`goal-mode`）。

---

## 三、主要布局与空间体系

### 3.1 四区 CSS Grid 全屏工作台

依据 `main-BFccKBrK.css`（`.app-shell[data-v-09888e88]` / `.app[data-v-09888e88]`）与 `style-BCua_rBX.css`：

```css
.app {
  display: grid;
  grid-template-columns: auto 0 minmax(0, 1fr) auto;
  grid-template-rows: minmax(0, 1fr) auto;
}
```

| 区域 | Grid 位置 | 尺寸与约束 | 交互特性 |
|---|---|---|---|
| **左侧边栏 `.side`** | `col 1, row 1 / -1` | CSS `--p-sidebar-w: 264px`；JS 默认 `270px`（范围 `220px–480px`） | 跨全高；折叠/展开 `280ms cubic-bezier(.4, 0, .2, 1)` |
| **侧栏拖拽柄 `.side-handle`** | `col 2, row 1 / -1` | 实体宽度 `0px` | 不占栅格宽度，仅作分割线与拖拽热区 |
| **主对话区 `.con`** | `col 3, row 1` | 阅读容器最大宽 `--p-content-max: 760px`（宽视图 `--p-content-wide: 920px`）；用户气泡上限 `78%` | 支持居中/左对齐切换（`content-align`）；顶部/底部 `13px` 滚动渐隐边缘 |
| **底部终端 `.terminal-panel`** | `col 3, row 2` | 高度可调，记忆于 `kimi-web.terminal-panel-height` | 嵌入 `xterm.js` WebGL 终端 |
| **右侧辅助面板** | `col 4, row 1 / -1` | 默认宽 `--panel-default-w: 460px`；Tab 最大 `168px` × 高 `28px` | 承载文件树（`--file-tree-panel-w: 224px`，范围 `160–400px`，行高 `28px`）、Diff/预览、内置浏览器；`.app.panel-expanded` 时隐藏主区与终端独占全宽 |

- **窗口与断点**：窗口最小尺寸 `900×480`；响应式断点 `--p-bp-sm: 640px`、`--p-bp-md: 980px`。

### 3.2 无边框标题栏与几何特征

- **无边框标题栏**：
  - Windows：`titleBarStyle: 'hidden'` + `titleBarOverlay`（透明背景、`height: 40px`、控件符号色 `#202020` / `#f2f2f2`）。
  - macOS：`titleBarStyle: 'hidden'` + `trafficLightPosition: { x: 16, y: 17 }`，开启 Vibrancy 时 `.app` 与 `.side-footer` 设为 `background: transparent`。
  - 顶栏与右侧面板头统一高 `--panel-head-h: 48px`，`.chat-header` 根据平台与侧栏状态动态留白（`78px` / `146px`）避让窗口控制按钮。
- **4px 间距栅格**：`--space-05: 2px`、`--space-1: 4px`、`--space-1-5: 6px`、`--space-2: 8px`、`--space-3: 12px`、`--space-4: 16px`、`--space-5: 20px`、`--space-6: 24px`、`--space-8: 32px`。
- **圆角与超椭圆（Concentric Radii）**：
  - 基础圆角：`4 / 6 / 8 / 12 / 16 / 20px`、窗口芯片 `14px`、Dock 胶囊 `10px`。
  - 输入框 `.composer-card`：**`--radius-composer: 32px` + `corner-shape: superellipse(1.5)`**，内部媒体缩略图圆角通过 `calc(var(--radius-composer) - var(--p-composer-media-inset) - var(--p-hairline))` 自动扣减，保持内外同心圆角；菜单项同理（`calc(12px - 3.5px - hairline)`）。
- **Hairline 细线**：高分屏默认 `--p-hairline: 0.5px`，`@media (max-resolution: 1.1dppx)` 自动回退 `1px`。

---

## 四、配色体系（Tokens 完整对照）

> 来源：`desktop-dist/assets/style-BCua_rBX.css`（Light 定义于 `:root`，Dark 定义于 `html[data-color-scheme=dark]` 及 `@media(prefers-color-scheme:dark)` 下的 `system`）。

### 4.1 表面层级与文字梯度（无明显冷暖偏色的极简灰）

| 语义角色 | Token | 浅色模式（Light） | 深色模式（Dark） |
|---|---|---|---|
| 主背景 | `--color-bg` / `--bg` | `#ffffff` | `#121212` |
| 画布辅助色 | `--canvas` | `#f9fbfc` | `#161717` |
| 侧边栏背景 | `--color-sidebar-bg` | `#f9fbfc` | `#0d0d0d`（最深层） |
| 次级表面/面板 | `--color-surface` / `--panel` | `#f5f5f5` | `#1f1f1f` |
| 抬升表面/气泡/代码块 | `--color-surface-raised` / `--color-user-bubble-bg` | `#ffffff` / `#f5f5f5` | `#292929` |
| 深层表面 | `--color-surface-deep` | `#f5f5f5` | `#0d0d0d` |
| 半透明顶栏 | `--color-topbar-bg-frost` | `color-mix(in srgb, var(--color-surface) 78%, transparent)` | 同公式 |
| 半透明菜单 | `--color-menu-bg` / `--color-menu-bg-frost` | `rgba(255,255,255,0.95)` / `70%` | 跟随 `--color-bg` 70% |
| 主文字 | `--color-text` | `rgba(0, 0, 0, 0.90)` | `rgba(255, 255, 255, 0.84)` |
| 强文字 | `--color-text-strong` | `#000000` | `#ffffff` |
| 次要文字 | `--color-text-muted` / `--dim` | `rgba(0, 0, 0, 0.60)` | `rgba(255, 255, 255, 0.56)` |
| 弱提示文字 | `--color-text-faint` / `--muted` | `rgba(0, 0, 0, 0.45)` | `rgba(255, 255, 255, 0.42)` |
| 四级文字 | `--color-text-quaternary` / `--faint` | `rgba(0, 0, 0, 0.30)` | `rgba(255, 255, 255, 0.26)` |
| 标准分割线 | `--color-line` | `rgba(0, 0, 0, 0.20)` | `rgba(255, 255, 255, 0.18)` |
| 弱分割线 | `--color-subtle` | `rgba(0, 0, 0, 0.05)` | `rgba(255, 255, 255, 0.05)` |
| 悬停/选中底 | `--color-hover` / `--color-selected` | `rgba(0,0,0,0.03)` / `0.05` | `rgba(255,255,255,0.05)` / `0.10` |
| 输入框描边 | `--color-composer-line` / `focus` | `rgba(0,0,0,0.13)` / `0.25` | — |
| 发送按钮 | `--color-send-bg` / `hover` | `rgba(0,0,0,0.90)` / `#252525` | — |

### 4.2 品牌主色、语义色与特效

| 类别 | Token | 浅色模式 | 深色模式 |
|---|---|---|---|
| 品牌电光蓝 | `--color-accent` / `hover` | `#1783ff` / `#167ff7` | `#1a88ff` / `#258eff` |
| 品牌浅底/边框 | `--color-accent-soft` / `bd` | `#e8f3ff` / `rgba(23,131,255,0.25)` | `rgba(26,136,255,0.10)` / `0.28` |
| 成功 | `--color-success` | `#0e7a38`（底 `#e7f6ee`） | `#3fb950`（底 `rgba(63,185,80,0.14)`） |
| 警告 | `--color-warning` | `#a9610a`（底 `#fbf1e0`） | `#d29922`（底 `rgba(210,153,34,0.14)`） |
| 危险 | `--color-danger` | `#c0392b`（底 `#fbeaea`） | `#f85149`（底 `rgba(248,81,73,0.14)`） |
| 橙色强调 | `--color-orange` | `#bc4c00`（底 `#fff1e5`） | `#f0883e`（底 `rgba(240,136,62,0.14)`） |
| 完成态（紫） | `--color-done` | `#8250df`（底 `#f3e8ff`） | `#a371f7`（底 `rgba(163,113,247,0.14)`） |
| Diff 前景/背景 | `--color-diff-add-*` / `del-*` | `#16c456` / `#ff4756`（10% 底） | 同前景 / `14%` 底 |
| 焦点环 | `--p-focus-ring` / `strong` | `0 0 0 3px var(--color-accent-soft)` | 加 `0 0 0 1px var(--color-accent)` |

- **Agent 接管浏览器流光（OKLCH）**：用 `color-mix(in oklch, var(--color-accent) 40%, #22d3ee / #8b5cf6 / #d946ef)` 生成旋转 `conic-gradient`（`8s` 周期，线宽 `2px`，外晕 `blur: 20px`）。
- **材质与阴影**：
  - 顶栏模糊：`--p-topbar-backdrop: saturate(150%) blur(12px)`；菜单模糊：`--p-menu-backdrop: blur(24px) saturate(1.8)`。
  - 菜单投影：`0 6px 18px lch(0% 0 0 / .02), 0 3px 9px lch(0% 0 0 / .04), 0 1px 1px lch(0% 0 0 / .04)`。
- **代码块高亮（Shiki）**：内置 `rose-pine`（default / dawn / moon）、`catppuccin`（latte / frappe / macchiato / mocha）、`ayu`（light / dark / mirage）等。

---

## 五、字体设计与排版阶梯

### 5.1 三套内置可变字体 + 防闪烁预注入

`index.html` 在 bundle 加载前通过内联 `<script>` 同步读取 `localStorage["kimi-web.color-scheme"]`、`localStorage["kimi-web.font-scale"]`、`localStorage["kimi-web.fonts"]`，提前将 `--user-font-ui`、`--user-font-transcript`、`--user-font-mono`、`--user-font-terminal` 写入 `document.documentElement.style`，彻底消除首屏 FOUC：

| 场景 | `kimi` 预设字体 | `system` 系统回退链 |
|---|---|---|
| UI 界面（`--font-ui`） | `Schibsted Grotesk Variable` + `Noto Sans SC Variable` | `-apple-system, BlinkMacSystemFont, "Segoe UI Variable Text", "Segoe UI", system-ui, ..., "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei UI", "Noto Sans CJK SC"` |
| 对话正文（`--font-transcript`） | 默认跟随 `ui`（可独立设置） | 同上 |
| 代码（`--font-mono`） | `JetBrains Mono Variable`（含斜体变体） | `ui-monospace, "SF Mono", SFMono-Regular, Menlo, "Cascadia Mono", Consolas, monospace` |
| 终端（`--font-terminal`） | 默认跟随 `code`（支持独立字号 `--user-terminal-size`） | 同上 |
| 数学公式 | 内置 171 个 KaTeX 字体文件（`.woff2/.woff/.ttf`） | — |

### 5.2 四档全局缩放与像素取整字阶

- **全局缩放**：`html[data-font-scale]` 支持四档：`small (12px)`、`medium (14px，默认)`、`large (16px)`、`xlarge (18px)`（旧版 `xxlarge` 自动迁移至 `xlarge`）。所有字号以 `--ui-shift: calc(var(--base-font, 14px) - 14px)` 整体平移。
- **UI 字阶（14px 基准）**：
  - `--ui-t0: 20px`（上限 `24px`）、`--ui-t1: 18px`（上限 `22px`）、`--ui-t2: 16px`
  - `--ui-b1: 15px`、`--ui-b2: 14px`（正文）、`--ui-c1: 12px`（辅助）、`--ui-c2: 10px`（微标）
  - 行高使用 CSS `round(calc(var(--ui-*) * ratio), 1px)` **强制像素取整**，避免非整数缩放带来模糊。
- **Markdown 字阶（14px 基准）**：
  - `h1: 22px`（行高 `1.63`）、`h2: 20px`（`1.60`）、`h3: 18px`（`1.56`）、`b1: 14px`（`1.625`）、`b2/b3: 13px`（`1.60 / 1.57`）、`code: 14px`（`22/14`）；终端行高 `--terminal-line-height: 1.1`。
  - 内联代码使用 `font-size-adjust: ex-height 0.5`，与正文字高光学对齐。
- **微调字重阶梯**（充分利用 Variable Font）：
  - `--weight-regular: 400`
  - `--weight-caption: 450`
  - `--weight-option-label: 475`
  - `--weight-medium: 500`
  - `--weight-ui-strong: 525`
  - `--weight-section-label: 600`
  - `--weight-semibold: 700`

---

## 六、覆盖范围与已知未知项

- **已覆盖**：`app.asar` 主进程/Preload 与 `desktop-dist` 核心 HTML/CSS/主入口 JS 的静态 Token 与布局规则。
- **未覆盖 / 局限**：
  - `app.asar` 内 4 个 Windows 版 `node-pty` 原生二进制存在 ASAR 完整性校验不一致，已在 REA Evidence 中标记为不可信字节。
  - `desktop-dist` 无 `.map` 文件，未展开 468 个语法高亮语言/Mermaid 图表 chunk；运行时动态计算后的 DOM 与 `/openapi.json`、`/asyncapi.json` 未通过 CDP 实机抓包。
