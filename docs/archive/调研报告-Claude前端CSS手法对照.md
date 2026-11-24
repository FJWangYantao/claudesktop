# 调研报告：Claude 桌面端前端样式拆解 → claudesktop 的对照与取舍

> 来源：`D:\360MoveData\Users\asus\Desktop\claude-ui-study`（对 Claude 桌面端 3.6556.1.0 的
> `ion-dist` 构建产物做的 CSS 拆解，16 篇文档 + 令牌表）。那份材料讲的是**浏览器里怎么画**，
> 这份讲**哪些能搬到 gpui、怎么搬、哪些搬不动**。
>
> 结论已按「已对齐 / 待落地 / 不可移植」三类归好，**没有改动任何代码**；待办条目在 `TODO.md`，
> 能力边界（做不到的那几条）记在 `AGENTS.md` 的「gpui-kit 踩坑备忘」最后一节。
>
> 核对依据：`gpui-pre 0.3.9`（`vendor/gpui-pre`，上游同版本）与 `gpui-component 0.7.3` 的源码，
> 路径相对 `%CARGO_HOME%\registry\src\index.crates.io-*\`。

---

## 零、一句话结论

1. **色彩、动效、组件契约这三块已经到位**（部分比原版还克制），不要因为看了那份材料回去重抄一遍。
2. **真正该改的是三件「一致性」**：焦点环只有两层、块间距写死像素不跟字号缩放、行高用比例。
3. 那套手法里有 **5 类在 gpui 里做不到**（hairline / 真蒙版 / `opsz` / 字距 / `text-wrap`），
   有 **2 条它判断错了**（`aria-*` 其实有；滚动渐隐其实几乎免费）。见第二节 P1-1 / P1-4 和第四节。

---

## 一、已经对齐的（别重复抄）

| 那份材料的建议 | 本项目现状 | 证据 |
|---|---|---|
| 灰阶「浅色暖、深色中性」的褪色曲线 | 趋势一致：浅色端 S 25–33%、H 43–48；深色端 S 2.7–4.3%。**浅色端比原版更暖**（原版 S 11–14%），这是刻意的纸感；深色端已接近中性（原版 0%） | `theme.rs:134-164` |
| 面之间只差 2–4% 亮度 | `PAPER/CARD` 1.05:1、`PAPER/SAND` 1.10:1、`SAND/LINE` 1.13:1（原版实测 1.05 / 1.09 / 1.26） | 同上的色值算得 |
| `LINE` 取灰阶 100 档 | `LINE #E5E1D5` L=86.7%，原版 `gray-100` L=86.67% | `theme.rs:146` |
| 品牌色只给两个值，用映射固化约束 | `CORAL`（图标）/ `CORAL_DEEP`（带文字）/ `CORAL_DEEP_HOVER` / `ON_ACCENT`，和 `--cds-fill-brand` 同一套思路 | `theme.rs:349-356` |
| 动效收敛成几个时长 + 几条曲线 | `MotionTokens`：120 / 180 / 280ms + 3 条曲线 + 2 个弹簧（原版 5 个时长 3 条曲线） | `gpui-component-0.7.3/src/theme/motion.rs:22-43` |
| 悬停不挤动布局 | `motion::hover_opacity` 只切透明度 | `app/motion.rs:420` |
| 思考默认折叠、流式扫光 | `motion::shimmer_text`（按像素分档，不是逐字） | `app/motion.rs:321` |
| 权限卡应答后收成一行 | 已做（应答后结果并进工具行，权限行不再画） | `session.rs:1087` 的 `merge_permission` |
| 菜单两行式 + 数字键 + 字体预览 | `menu_item` / `MenuList::pick` / `font_menu_item`（原版没有字体预览） | `app/menu.rs` |
| 减少动态效果两层（系统 + 应用内） | `MotionPref`（`settings.rs:741`）+ `cx.reduce_motion()` | `app/motion.rs:586` |
| 组件只开少量「部件」 | 没有 `data-cds` 那层，但布局状态集中在 `Session::row_key` / `RowLayout` | `session.rs:598` |

**别动的地方**：`palette_contrast` 已经在守 6 套 × 2 外观 × 11 对对比度，是全仓库质量最高的一段代码；
改色值只会让它变难维护，收益是负的。

---

## 二、待落地的（按「收益 ÷ 成本」排）

### P0-1　焦点环升级成三层

**现状**（`app/composer.rs:464-473`）是两层：

```rust
.border_color(rgb(if focused { INK_FAINT } else { LINE }))
.when(!focused, |d| d.hover(|d| d.border_color(rgb(LINE_STRONG))))
.map(|d| if focused { d.shadow_md() } else { d.shadow_sm() })
```

**依据**：原版的 `--cds-focus-shadow` 是三层——① `inset 0 0 0 1px` 挖出 1px 底色间隙、② `0 0 0 1px` 实描边、
③ `0 0 6px 1px` 外光晕（40% 强调色）。

**gpui 里能一比一移植**，因为绘制顺序固定（`gpui-pre-0.3.9/src/style.rs:711` 先画 drop shadow、
`:740` 背景之后画 inset shadow），三层都成立：

```rust
// ① inset 0 0 0 1px PAPER       → BoxShadow::new(0,0,PAPER).spread_radius(px(1.)).inset()
// ② 0 0 0 1px ACCENT            → spread 1px、blur 0，只露背景外那一圈
// ③ 0 0 6px 1px ACCENT@40%      → blur_radius 取 3px（见下）
```

**关键换算**：`BoxShadow::blur_radius` 在 gpui 里是高斯 σ，而 CSS 的 blur 是 2σ。
`gpui-component` 自己的 `popover_shadow` 就把 Tailwind 的 6px 写成 3px
（`gpui-component-0.7.3/src/styled.rs:46-52`）。**照抄 6px 会糊成一团灰雾。**

顺带一条现在没做到的：原版把 `:focus` 和 `:focus-visible` 分开（鼠标点进来不出环、键盘 Tab 才出）。
gpui 有现成钩子 `div.rs:1295` 的 `.focus_visible(..)`，内部判 `window.last_input_was_keyboard()`。

**成本**：一个 helper（`theme.rs` 里给 `focus_shadows(color)`），调用点两三处。
**风险**：`.shadow_md()` 会被换掉，输入框的「浮起感」要重新调。

### P0-2　块间距从 px 改成跟字号缩放

**现状**（`app/chat.rs:31-38`）：

```rust
const STEP_GAP: Pixels = px(6.);
pub(super) const BLOCK_GAP: Pixels = px(16.);
const ICON_COL: Pixels = px(16.);
pub(super) const STEP_INDENT: Pixels = px(24.);
```

`Pixels` 是绝对像素，**不跟根字号缩放**；同一页里 `gap_2` / `px_6` / `rounded_2xl` 全是 rem。
字号调到 140% 时正文和控件长 40%，块间距还是 16px，整页节奏会塌。

**依据**：原版所有间距 `× var(--cds-rem-scale)`，且 `item-gap = 行高 × 0.5`、`turn-gap = 行高 × 2`。

**做法**：改 `Rems`（或由 `TEXT_LARGE` 派生）。数值照抄不照抄是审美问题——
按原版公式 `15px × 1.6 = 24px` 算出来是 STEP_GAP 12 / BLOCK_GAP 48，比现在松很多；
**先换单位、数值不动**，再单独决定要不要放松。

`ICON_COL` 是固定图标格，可以留 px（`AGENTS.md` 已把它列为例外）。

**成本**：改类型 + 几处 `to_pixels(window.rem_size())`。

### P0-3　行高：比例 vs 绝对值

**现状**（`theme.rs:442`）：`pub const LEADING: DefiniteLength = relative(1.6);`

它是比例，所以 `TEXT_SMALL(13px)` 的行高 20.8px、`TEXT_LARGE(15px)` 24px。
原版明确把行高写成绝对值（`--cds-leading-body: 1.25rem`），理由是**不同字号混排时基线才对齐**。

**结论**：不用全改。比例行高在单一字号的正文里没问题；问题出在 `TextView` 里
markdown 各级标题和正文混排、以及工具行 / diff 头这种相邻不同字号的区域。

**做法**：给「混排区」单独给绝对值（gpui 的 `line_height_in_pixels()` 会 round，不会亚像素糊字）。
**成本**：低，但要逐个区域判断，别一刀切。

### P0-4　尺寸 token 的 rem / px 分工（这条现在有实际挤压）

**现状**（`theme.rs:663-673` + `app/chrome.rs:20`）：

| 常量 | 值 | 该不该跟字号缩放 |
|---|---|---|
| `READING_W` 760 | 阅读列 / 输入框 | **不该**（按字符数算的 measure） |
| `FORM_W` 720 | 设置页内容 | **不该** |
| `OVERVIEW_W` 724 | 概览面板 | **不该** |
| `SIDEBAR_W` 256 | 侧栏 | **该**（标题 + 相对时间挤在一起会截断） |
| `SETTINGS_NAV_W` 200 | 设置页导航 | **该** |
| `CHROME_H` 44 | 顶栏高度 | **该**（放大后塞不下 19.6px 的字） |

原版所有尺寸都 `× rem-scale`，包括侧栏。**阅读列留 px，其余改 rem。**

### P1-1　滚动渐隐跟手（那份材料说 gpui 做不到，其实几乎免费）

**现状**（`app/content.rs:195-201`）是两条**静态**渐变：16px 顶、24px 底，永远都在。

**依据**：原版滚到顶时顶部淡出高度→0、滚到底时底部→0，用 `animation-timeline: scroll(self)` 驱动。
材料第 10 篇判断「gpui 每帧重画遮罩不划算」——**在这里不成立**：`step_wheel`（`chat.rs:1726`）
滚动期间**本来就每帧 `content_view.update(cx, notify)`**，内容区已经在重画了。

**做法**（按材料自己的「分档」建议，别做连续值）：

```
贴顶   → 顶 0  / 底 24
中间   → 顶 16 / 底 24
贴底   → 顶 16 / 底 0
```

三个状态由已有的 `scroll_top(&s.list)`（`chat.rs:2161`）和 `distance_to_end(&s.list)`
（`chat.rs:2165`）算，跨档才换；存一个枚举在 `Session` 里。
**不要并进 `row_key`**——那是布局指纹，会触发整表重测。

侧栏底部那条（`sidebar.rs:705`）同理，但侧栏是缓存 view，要并进 `sync_sidebar` 的指纹。

### P1-2　渐隐条换成感知线性

`linear_gradient()` 的 `Background` 里是 `colors: [LinearColorStop; 2]`
（`gpui-pre-0.3.9/src/color.rs:784`）——**gpui 的渐变只有两个色标**，原版的 4 段 / 16 段写法搬不过来。

两个替代：

1. **`.color_space(ColorSpace::Oklab)`**（`color.rs:923`）。sRGB 线性插值在低 alpha 段发灰，
   Oklab 平滑很多。**一行改动，先试这个。**
2. 真要感知线性就**叠条**：24px 淡出切成 8 条 3px，每条一个固定 alpha
   （按 `0d / 26 / 4d / 80 / b3 / d9 / ff` 那组比例取样）。8 个 quad，代价可忽略。

### P1-3　菜单面板没走 gpui-kit 的弹层外观

**现状**（`app/menu.rs:349-351`）：

```rust
popover.track_focus(focus).bg(rgb(CARD)).border_1().border_color(rgb(LINE)).rounded_lg().p_1()
```

gpui-kit 自己的弹层（Select / Combobox / DatePicker / 编辑器悬浮框）全走 `popover_style(cx)`
（`gpui-component-0.7.3/src/styled.rs:186-194`）：**无边框 + 一圈 10% 前景色 hairline ring + 两层阴影**。

结果：本项目的下拉菜单和 gpui-kit 的对话框 / 通知是**两套描边语言**。
材料第 04 篇讲的「双层 inset 做出半像素描边观感」，`popover_shadow` 已经实现了，`menu_panel` 反而绕开。

**做法**：`menu_panel` 改成 `popover_style(cx)` 打底，保留 `p_1` 和焦点。
顺带满足 `AGENTS.md` 的「组件颜色不对时先补映射，别在调用处逐个覆盖」。

### P1-4　补 `role` / `aria-*`

**材料第 09 篇说「gpui 里没有 `aria-*`」——这是错的。** `gpui-pre` 有完整的
`role(accesskit::Role)`（`div.rs:1310`）、`aria_label`（`1332`）、`aria_selected`、`aria_expanded`、
`aria_toggled`、`aria_numeric_value`、`aria_keyshortcuts`……本仓库 grep `aria` **零命中**。

原版那套 `data-cds` + `aria-pressed` 的价值不在选择器，在「**状态只有一份，样式和行为永远同步**」。
gpui 里同样成立，而且白送无障碍：

| 位置 | 加什么 |
|---|---|
| 侧栏会话行 | `role(ListItem)` + `aria_selected(active)` |
| 折叠的过程组头 | `aria_expanded(group.open)` |
| 视图模式菜单项 | `aria_selected(mode == current)` |
| 权限卡按钮组 | `aria_keyshortcuts("Alt+1")`（按键提示本来就是 Alt+数字） |
| 队列行的「现在发送」 | `aria_label` |

**成本**：一行一个。这是材料里唯一既跨框架、又能立刻提升工程质量的条目。

### P2　其余（做不做都行）

| 项 | 说明 |
|---|---|
| **60ms 悬停令牌** | 原版 `--cds-dur-fast: 60ms` 专给悬停；`MotionTokens.duration_fast` 是 120ms，悬停淡入偏肉。加一个 hover 专用令牌 |
| **密度轴** | 原版三轴（theme / mode / density），本项目只有色板和字号。现在的「字号缩放」把字号和间距绑在一起；密度应该是**只改间距不改字号**，要加一层 `density: f32` 乘在 gap / padding 上，成本中等 |
| **输入框圆角单独一档** | 原版 composer 是 20px，本项目 `rounded_2xl` = 16px（随 rem）。差 4px，要加就得在 `theme.rs` 开 token（`AGENTS.md` 不许 `rounded(px(..))`），优先级低 |
| **权限请求音效** | 原版 `sfx/tool_approval_needed.mp3` 的意义是「用户没盯屏幕时一声比红点有效」。本项目已有 `notify_attention`（`notify.rs:72`），加 `PlaySoundW` 一行；要默认关 |
| **流体问候语** | 原版 `clamp()` 让字号跟窗口宽变；gpui 能读 `window.viewport_size()`（`chrome.rs:117` 已在用），收益小 |
| **阅读列 760 vs 850** | 原版 850px。760px 配 15px 正文已经约 100 个拉丁字符/行，**不建议改**，记录差异即可 |

---

## 三、尺寸换算备忘（gpui 与 CSS 的对应）

写新样式时容易踩的几处，都是实测：

| CSS | gpui | 注意 |
|---|---|---|
| `box-shadow: 0 0 6px` | `blur_radius(px(3.))` | gpui 是 σ，CSS 是 2σ，**减半** |
| `box-shadow: ... 1px` | `spread_radius(px(1.))` + `blur_radius(0)` | 同上，`spread` 不用换算 |
| `border: 1px` | `border_1()` | 描边会被夹到**至少 1 物理像素**（见第四节 hairline）；档位只到 11，没有小数档 |
| `line-height: 1.6` | `relative(1.6)` | 是比例，不是像素 |
| `padding: 8px` | `p_2` | 后缀不是像素，是 **0.25rem 的倍数**：`1`=4px、`2`=8px、`2p5`=10px、`5`=20px |
| `border-radius: 12px` | `rounded_xl` | 12px；`2xl` 16 / `3xl` 24 / `full` 9999 |

**另一处坑**：`gpui-pre-macros` 生成的 `.shadow_xs() / .shadow_sm() / .shadow_md()`
直接用了 Tailwind 的数值（`blur_radius(px(3.))` / `px(6.)`，见 `gpui-pre-macros-0.3.9/src/styles.rs:416-463`），
**没有减半**——所以它们比 shadcn 原版宽一倍。要贴原版观感就自己写 `BoxShadow` 并减半
（gpui-kit 的 `popover_shadow` / `raised_shadow` 就是正确示范）。

---

## 四、不可移植（别再讨论，也别照抄）

| 那套手法 | gpui 的实际情况 | 结论 |
|---|---|---|
| `calc(1px / 4)` hairline | `round_stroke_to_device_pixel` 把任何非零描边**夹到 ≥1 物理像素**（`gpui-pre-0.3.9/src/util.rs:145-151`） | ❌ 做不到。gpui 已经「做对了」；要更淡就**降颜色对比度** |
| `mask-image` 真蒙版（186 处） | 只有 `ContentMask`（矩形裁切，`window.rs:2231`）+ `paint_path`，没有 alpha mask | ❌ 只能用「背景色渐变覆盖层」近似（`edge_fade` 就是这个），前提是底下是同一个纸色 |
| `font-variation-settings: "opsz" 28` | `TextStyle` 没有 variation 轴（`style.rs:438-483`）；内嵌 Inter 是**静态字重子集**（`assets/fonts/` 只有 400/500/600） | ❌ 双重做不到 |
| `letter-spacing: -0.01em` | `TextStyle` 无字距字段，`text_system` 也没有 | ❌ 标题的负字距抄不了 |
| `text-wrap: balance / pretty` | 只有 `line_clamp`（截断）和 `text_overflow`（`style.rs:475-482`） | ❌ |
| `backdrop-filter: blur(12px)` | 只有整窗级的 `WindowBackgroundAppearance::{Blurred, MicaBackdrop, MicaAltBackdrop}`（`platform.rs:2766-2785`），没有元素级模糊 | ❌ |
| `:has()` 182 条 | 无选择器引擎 | ❌ 但「哪些状态需要向上传播」的知识可以搬，本项目已用数据流做了 |
| `@container` 213 处 | 无容器查询；`widgets.rs` 的 `MeasuredWidth` 是手搓的等价物 | ❌ |
| `@property` + `animation-timeline: scroll()` | 无 CSS 动画；等价物是 Rust 状态 + `transition()` / `spring()` | ❌ 机制不同，结论相同 |
| `view-transition-class` | 无 | ❌ |
| CJK 按语言切 fallback（sc / tc / jp / kr） | `FontFallbacks` 是一个平铺列表（`text_system/font_fallbacks.rs:9`），**没有按 run 判语言的能力** | ❌ 且界面纯中文，收益为零。现在的「雅黑优先 + kinsoku 补丁」是更对的做法 |
| `:has(+ :hover)` 隐藏下一条分隔线 | 无兄弟选择器 | ❌ 要做得把「当前悬停行号」塞进状态（`sidebar_hover` 有先例），成本 > 收益 |
| `data-cds` 属性契约 | 无属性选择器 | ❌ 但要的那层「语义标记」可以用 `role` / `aria_*` 补（见 P1-4） |

---

## 五、防回退：把结论变成测试

材料是「按手法」列的，所以会出现 hairline / opsz 这种搬不动的项。真正该沉淀的是**约束**，
让测试自己守住，而不是靠文档提醒：

1. `palette_contrast`（`theme.rs:703`）已经这么做了，是范本。可以扩一条「面分层亮度差在 1.03–1.15 之间」。
2. `spacing_scales_with_font`：断言布局常量是 `Rems` 而不是 `Pixels`（`READING_W` / `FORM_W` /
   `OVERVIEW_W` 这类白名单除外）。这条能永久堵住 P0-2 / P0-4 的回退。
3. `popover_surfaces_share_one_ring`：断言 `menu_panel` 和 gpui-kit 弹层用同一套描边来源。

---

## 六、和既有文档的关系

- `设计指南-Claude风格UI.md`：讲**理念和取舍**（为什么暖、为什么一个强调色、组件该长什么样），
  第二章的色值表和第七章的落地清单仍然是权威。
- 本文件：讲**手法能不能搬**（CSS → gpui 的能力对照），补的是那份指南没有的实现层细节。
- `设计说明-多套配色.md`：色板的选型和对比度约束，本文件不改它的任何结论。
