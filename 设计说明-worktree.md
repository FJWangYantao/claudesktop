# 设计说明：worktree 隔离

> 状态：方案（2026-10-20），未实现。第八节的取舍已按推荐定。
> 代码落点：`crates/core/src/git.rs`（新，git 调用 + worktree 增删查）、`project.rs`（认 worktree 根）、
> `store.rs`（`SessionMeta::worktree`）、`apps/desktop/src/app/mod.rs`（建会话）、`composer.rs`（目录菜单）、
> `sidebar.rs`（分组、行标记、归档 / 删除时清理）、新 `app/worktree.rs`（会话顶栏的分支 chip 和操作）。

## 一、要解决什么

多个会话现在已经能同时跑：每个会话有自己的 `cwd`，自研内核每轮单开线程（`crates/builtin/src/lib.rs` 的 `builtin-turn`）。
但两个会话选同一个项目文件夹时，改的是同一份磁盘文件：

- 互相覆盖改动，`cargo build` / 测试跑到对方改了一半的代码；
- 检查点回退按「agent 改之前的内容」还原，会把另一个会话的改动一起冲掉；
- 逐块审查对不上磁盘现状，退成只读。

目标：新建会话时可以选「在新 worktree 中」，这个会话在同一仓库的一个独立 git worktree（独立分支）里干活，
干完再决定合并、推送还是丢掉。侧栏仍然把它算在原项目下面。

不做：多模型同任务并跑再比较（best-of-n）、自动解决合并冲突、替用户开 PR。

## 二、参考：别家怎么做

| | 放在哪 | 分支 | 收尾 |
|---|---|---|---|
| Claude Code（`--worktree` / 桌面版） | 仓库内 `.claude/worktrees/<名>` | 新建分支 | 退出时问保留还是删除；`.worktreeinclude` 列要复制过去的未跟踪文件 |
| Codex 桌面版 | `~/.codex/worktrees/` | 游离 HEAD，需要时再建分支 | 「同步回本地」把改动搬回主目录 |
| Cursor（并行 agent） | `~/.cursor/worktrees/` | 新建分支 | 「Apply」把改动合回主目录；`worktrees.json` 写初始化命令 |

共同点：一个会话一个 worktree；未跟踪文件（`.env`、依赖目录）不会自动过去，要么复制要么跑初始化命令。

## 三、放在哪、叫什么

- **位置**：数据目录 `worktrees/<仓库名>-<主仓库路径短哈希>/<名字>`，不放进仓库里。
  放仓库内（Claude Code 的做法）要往 `.git/info/exclude` 加规则，而且主目录的 IDE / 文件监视 / `cargo` 的 `target`
  会把每个 worktree 的构建产物都看进去；数据目录没有这些问题。路径长不碍事：分支 chip 有「在资源管理器中打开」。
- **名字 / 分支**：`claudesktop/<月日-时分>-<3 位随机>`，例如 `claudesktop/1020-1432-k7f`。建的时候会话还没标题，
  不拿标题命名；界面上一律显示会话标题 + 分支名，不显示目录名。
- **起点**：主仓库当前 HEAD（记下当时所在分支名作为 `base`，游离 HEAD 时记提交号）。
  主目录里没提交的改动**不带过去**，菜单项说明里写明（「从 main 的最新提交开始，未提交的改动不会带过去」）。

## 四、数据模型

```rust
// crates/core/src/store.rs
pub struct SessionMeta {
    // ……原有字段
    /// 会话跑在自己的 git worktree 里（`cwd` 就是 worktree 目录）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeMeta>,
}

pub struct WorktreeMeta {
    /// 主仓库根目录：侧栏按它分组，项目记忆 / 权限规则也按它算
    pub repo: PathBuf,
    pub branch: String,
    /// 从哪儿分出来的（分支名，游离时是提交号）：合并目标、「领先几个提交」都按它算
    pub base: String,
}
```

`Session` 上同样挂一份（还没落盘的会话也要能分组），建档时写进元信息，和 `task` 一样。

`crates/core/src/git.rs`（把 `plugin_git.rs` 里的 `git()` 挪过来共用：不弹窗、不问密码、报错译成中文）：

- `repo_root(dir) -> Option<PathBuf>`：`git rev-parse --show-toplevel`，再经 `--git-common-dir` 换成**主**仓库根；
- `add_worktree(repo, path, branch) -> Result<WorktreeMeta>`：`git worktree add -b <branch> <path> HEAD`；
- `status(meta, path) -> WorktreeStatus { dirty_files, ahead, behind, merged }`：`status --porcelain` + `rev-list --left-right --count base...branch`；
- `remove_worktree(meta, path, force)`、`delete_branch(meta, force)`、`merge_into_base(meta)`（见第六节）；
- 同一主仓库的 git 写操作按仓库加锁串行（两个会话同时建 worktree 时 git 会抢 `.git/worktrees` 的锁）。

## 五、项目根要认 worktree（顺手修一个现有的坑）

`project::project_root` 现在判断 `d.join(".git").is_dir()`。worktree 里的 `.git` 是**文件**
（内容是 `gitdir: <主仓库>/.git/worktrees/<名>`），所以用户今天自己 `git worktree add` 出来的目录就认不出项目根：
项目配置、`instructions` 指的文件、项目记忆全都落空。

- `project_root`：`.git` 是文件也算根（worktree 自己的根）。项目配置、CLAUDE.md / AGENTS.md 从这里读，
  读到的是**这个分支上的版本**，和代码一致。判据是文件内容以 `gitdir:` 开头且路径里有 `worktrees/<名>`
  ——只看「是文件」会把随便一个同名文件当成标记；只认 `gitdir:` 又会把子模块
  （`gitdir: ../.git/modules/x`）误当成 worktree。
- 新增 `project::shared_root(cwd) -> PathBuf`：是 worktree 时解析 `.git` 文件换成主仓库根（纯文件解析，不调 git，可单测）。
  **项目记忆**（`memory::project_file`）、**权限规则的存和查**（`permissions.rs` 的 `add` / `matching`、
  `lib.rs` 合并项目权限处）都用它：在 worktree 里说过的「记住……」、点过的「始终允许」，回到主目录也算数，反之亦然。
  存和查必须是**同一个**根，否则规则写进去了判断时却查不到。
- 路径要**按组件归一**：git 在 `.git` 文件里写的是正斜杠（`C:/repo/.git/worktrees/b1`），
  而别处的 `cwd` 是原生分隔符。权限规则拿 `display()` 当键，不统一就会变成两张互不可见的表。

已实现（2026-10-21），测试见 `project.rs`、`memory.rs`、`permissions.rs` 里 `worktree` / `shared_root` 相关的几条。

## 六、生命周期

1. **新建**：输入框上方目录菜单，当前目录是 git 仓库时多一项「在新 worktree 中开始」（`GitBranch` 图标）。
   点了在后台线程建 worktree（大仓库检出要几秒），目录 chip 显示转圈和「正在准备 worktree…」，
   输入框照常可写，期间发的消息排进 `Session::queued`，建好了再发。建失败（没装 git、磁盘满、分支名冲突）用 toast 说原因，会话留在原目录。
   命令面板加「在新 worktree 中新建对话」。
2. **跑**：和普通会话没有区别。`cwd` 是 worktree 目录，「工作目录内的编辑不进分类器」、检查点、逐块审查都按它算；
   agent 想改主目录的文件就是工作目录外，照常问。ACP 内核开会话时经 `cwd` 交给 agent，不用额外适配。
3. **看状态**：会话顶栏面包屑后面一个分支 chip：`⎇ claudesktop/1020-1432-k7f · 3 个文件 · 领先 2`。
   点开是 `menu_popover`：在资源管理器中打开、复制分支名、复制路径、合并回 base、删除 worktree。
   状态在一轮结束和点开菜单时刷新（跑一次 `git status`），不在 render 里跑。
4. **合并回 base**：只做最稳的那条路——
   - worktree 里还有没提交的改动 → 先提示「先让 agent 提交，或在这里一键提交」（一键提交 = `git add -A && git commit`，提交信息默认取会话标题，可改）；
   - 主目录当前不在 `base` 分支，或者有没提交的改动 → 不动它，告诉用户原因，给「复制合并命令」；
   - 否则在主目录 `git merge --no-ff <branch>`；冲突就 `git merge --abort` 退回原样，提示「有冲突，在 worktree 里先合并 base 再来」
     （可以直接让这个会话的 agent 去做：菜单里给「让 agent 合并 base」，往输入框填一句话）。
   合并成功后问一句「删除这个 worktree？」。
5. **归档 / 删除会话**：会话带 worktree 时，确认浮层多一行：
   - 干净且已合并（或一个提交都没有）→ 勾选「同时删除 worktree 和分支」，默认勾上；
   - 有没提交的改动或没合并的提交 → 默认不勾，勾上时写清「3 个文件的改动和 2 个提交会丢失」，删分支用 `-D`。
   取消归档时 worktree 还在就直接回来；已经删了见下一条。
6. **目录没了**（用户手动删了、换了电脑）：打开这个会话时发现 `cwd` 不在——分支还在就 `git worktree prune` 后重新
   `worktree add <path> <branch>` 接上（对话里 `Session::note` 一行「已重新检出 worktree」）；分支也没了就只读，输入框给出「在主目录继续」。
7. **兜底清理**：启动时对每个主仓库 `git worktree prune` 一次；从没发过消息、也没有改动的 worktree 会话被丢弃
   （`add_session` 复用空会话、或者关程序）时顺手删掉，不在数据目录里堆空目录。

## 七、界面与周边

- **侧栏**：`active_session_groups` 分组的键从 `s.cwd` 换成 `s.group_dir()`（有 worktree 用 `repo`，否则 `cwd`）。
  worktree 会话的行尾加一个小 `GitBranch` 图标（悬停 `text_tip` 显示分支名和状态），并进 `sync_sidebar` 的指纹。
  `recent_workspaces` 跳过 worktree 目录（目录菜单里不出现一串数据目录路径）。
- **新会话继承**：在 worktree 会话里点「新会话」，默认回到**主目录**（`self.cwd` 不跟着变成 worktree）；
  想在同一个 worktree 里另开对话用分支 chip 菜单里的「在这个 worktree 里新开对话」。
- **未跟踪文件**：项目配置（`.claudesktop/project.json`）加两项，建 worktree 后执行：
  `worktree.copy`（从主目录复制过去的文件 / glob，典型是 `.env*`）和 `worktree.setup`（初始化命令，如 `npm install`，
  走 `tools::QUIET_ENV` 后台跑，输出进对话的一张工具卡，失败不阻塞对话）。没配时什么都不做，界面上不提。
- **用量 / 请求页**：不受影响（按内核会话归组）。概览「项目」维度按 `group_dir` 算，worktree 会话算进主项目。
- **Windows**：数据目录路径偏长，建 worktree 时对该仓库设 `core.longpaths=true`（只写进 worktree 自己的配置，不碰全局）。

## 八、已定的取舍

1. **位置放数据目录，不放仓库里**：不污染主目录的 IDE 和构建；代价是路径不直观，用分支 chip 的「打开」弥补。
2. **建新分支，不用游离 HEAD**：分支名就是这次任务的句柄，合并、推送、开 PR 都直接用；用户不会因为游离 HEAD 丢提交。
3. **合并只做快路径，拒绝时给原因和命令**：不在主目录里切分支、不 stash 用户的改动、冲突不替用户解。复杂情况交给 agent 或用户。
4. **记忆和权限规则跨 worktree 共享，指令文件按分支读**：记忆、规则是「对这个项目」的，指令文件是代码的一部分。

## 九、测试

- `crates/core/src/git.rs`：临时目录里 `git init` 一个仓库，测建 / 状态（脏、领先、已合并）/ 合并（快进、冲突回滚、主目录不在 base 时拒绝）/
  删除（干净、脏了不带 force 失败）/ 目录丢了重新接上。机器上没有 git 时这些测试跳过（不算失败）。
- `project.rs`：`.git` 是文件时 `project_root` 认得出；`shared_root` 解析 `gitdir:`（相对路径、Windows 盘符、行尾 `\r\n`）。
- `permissions.rs`：主目录存的规则在 worktree 里命中，反之亦然。
- `crates/builtin/tests/kernel.rs`：worktree 目录当 `cwd` 开会话，项目记忆读写落到主仓库的 `.claudesktop/memory.json`。
- 界面：侧栏分组（worktree 会话进主项目组）、归档确认浮层的两种默认勾选；侧栏改动前后各跑一次
  `CLAUDESKTOP_STRESS=<行数>` 看帧间隔没变差。

## 十、工作量与顺序

1. ✅ `project_root` / `shared_root` 修正和测试（2026-10-21 已完成，第五节的修正单独提交过；
   `git.rs` 还没做，归到第 2 步一起）。落地时补了两条文档里没写的：`.git` 文件必须验 `gitdir:` 内容
   （随便一个同名文件不算标记），子模块那种 `gitdir: ../.git/modules/x` 不能当 worktree；
   以及路径归一（git 写正斜杠，权限规则拿 `display()` 当键，不统一会变成两张表）。
   权限规则的**存**（`PermissionRules::add`）也跟着改用 `shared_root`，否则写进去查不到。
2. `git.rs` + `SessionMeta::worktree`、`Session::group_dir`、侧栏分组 + 行标记（半天）。
3. 目录菜单「在新 worktree 中开始」+ 后台创建 + 排队发送 + 命令面板（半天）。
4. 分支 chip、状态刷新、合并 / 删除 / 归档清理 / 目录丢失重接（一天）。
5. `worktree.copy` / `worktree.setup`（半天，可以放到最后或下一期）。

合计约三天。第 2 步本身也不改变现有行为，可以先合。
