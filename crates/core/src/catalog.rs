//! 设置页「推荐」里的常用 MCP 服务和插件：点一下就装。
//!
//! MCP 服务是写进 `mcp.json` 的一段配置（要填的东西——文件夹、令牌——用 `{input}` 占位）；
//! 插件取自 Anthropic 官方插件市场，只装那一个插件的目录（`plugin_git::install_subdir`）；
//! 市场清单里指向别的仓库的条目列在 [`EXTERNAL_PLUGIN_PICKS`]，按清单里的 `ref` / `sha` 钉版本装
//! （`plugin_git::install_at`）。
//! 只收跨平台、装完就能用的：插件不收要 bash / python 钩子的，也不收本应用不支持的 LSP 插件。

use serde_json::Value;

/// 推荐插件都来自这个市场仓库
pub const OFFICIAL_PLUGINS: &str = "https://github.com/anthropics/claude-plugins-official.git";

/// 装之前要用户给的一样东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickInput {
    /// 一个文件夹（弹系统的选文件夹对话框）
    Folder,
    /// 一段密钥 / 令牌（输入框）；`hint` 写去哪里拿
    Secret { label: &'static str, hint: &'static str },
}

/// 一个推荐的 MCP 服务。
#[derive(Debug, Clone, Copy)]
pub struct McpPick {
    /// 写进 mcp.json 的服务器名（也是模型看到的 `mcp__名字__工具` 里那段）
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// 装之前要准备的（「需要 Node.js」「要登录」）
    pub needs: &'static str,
    /// mcp.json 里 `mcpServers.<name>` 的值；字符串里的 `{input}` 换成用户填的
    config: &'static str,
    pub input: Option<PickInput>,
}

impl McpPick {
    /// 填好 `{input}` 的配置。
    pub fn entry(&self, input: &str) -> Value {
        // 按 JSON 字符串转义后再替换，路径里的反斜杠、引号不会把 JSON 弄坏
        let escaped = serde_json::to_string(input).unwrap_or_default();
        let escaped = &escaped[1..escaped.len() - 1];
        serde_json::from_str(&self.config.replace("{input}", escaped)).unwrap_or(Value::Null)
    }
}

/// 一个推荐插件。
///
/// 两种来源：默认是本文件顶上那个官方市场仓库里的一层目录（`path`）；`repo` 给了就是
/// **别的仓库**（市场清单里 `source` 写成 `{"source": "url" | "git-subdir", …}` 的那些），
/// 这时按 `rev`（tag / 分支 / 提交）钉一个版本装，见 `plugin_git::install_at`。
#[derive(Debug, Clone, Copy)]
pub struct PluginPick {
    /// 插件名（清单里的 name，也是装到 `plugins/` 下的文件夹名）
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// 市场仓库里的目录（`repo` 为空时用）
    pub path: &'static str,
    /// 别的仓库的地址（`https://…` 或 GitHub 的「作者/仓库」）；None = 官方市场仓库
    pub repo: Option<&'static str>,
    /// 钉哪个版本（tag）；空 = 最新
    pub rev: &'static str,
}

impl PluginPick {
    /// 装的时候克隆哪个仓库。
    pub fn repo_url(&self) -> &'static str {
        self.repo.unwrap_or(OFFICIAL_PLUGINS)
    }
}

/// 全部推荐插件：官方市场里的 + 别的仓库里的（「推荐」栏按这个顺序列）。
pub fn all_plugin_picks() -> impl Iterator<Item = &'static PluginPick> {
    PLUGIN_PICKS.iter().chain(EXTERNAL_PLUGIN_PICKS)
}

pub const MCP_PICKS: &[McpPick] = &[
    McpPick {
        name: "filesystem",
        title: "文件系统",
        description: "读写指定文件夹里的文件",
        needs: "需要 Node.js",
        config: r#"{"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "{input}"]}"#,
        input: Some(PickInput::Folder),
    },
    McpPick {
        name: "fetch",
        title: "网页抓取",
        description: "打开网址，把网页转成 Markdown 给模型读",
        needs: "需要 uv",
        config: r#"{"command": "uvx", "args": ["mcp-server-fetch"]}"#,
        input: None,
    },
    McpPick {
        name: "playwright",
        title: "Playwright 浏览器",
        description: "在后台开浏览器点页面、填表单、截图，适合测网页",
        needs: "需要 Node.js",
        // 无头：不在前台弹浏览器窗口（要看着它操作就把 --headless 删掉）
        config: r#"{"command": "npx", "args": ["-y", "@playwright/mcp@latest", "--headless"]}"#,
        input: None,
    },
    McpPick {
        name: "chrome-devtools",
        title: "Chrome 开发者工具",
        description: "控制 Chrome，看控制台、网络请求和性能",
        needs: "需要 Node.js 和 Chrome",
        config: r#"{"command": "npx", "args": ["-y", "chrome-devtools-mcp@latest", "--headless"]}"#,
        input: None,
    },
    McpPick {
        name: "context7",
        title: "Context7 文档",
        description: "查各种库和框架的最新文档、示例代码",
        needs: "远程服务，免安装",
        config: r#"{"type": "http", "url": "https://mcp.context7.com/mcp"}"#,
        input: None,
    },
    McpPick {
        name: "deepwiki",
        title: "DeepWiki",
        description: "问 GitHub 上公开仓库的结构和用法",
        needs: "远程服务，免安装",
        config: r#"{"type": "http", "url": "https://mcp.deepwiki.com/mcp"}"#,
        input: None,
    },
    McpPick {
        name: "microsoft-learn",
        title: "Microsoft Learn",
        description: "查微软官方文档（.NET、Azure、Windows 等）",
        needs: "远程服务，免安装",
        config: r#"{"type": "http", "url": "https://learn.microsoft.com/api/mcp"}"#,
        input: None,
    },
    McpPick {
        name: "github",
        title: "GitHub",
        description: "查仓库、提 issue、看和改 PR",
        needs: "要 GitHub 个人令牌",
        config: r#"{"type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": {"Authorization": "Bearer {input}"}}"#,
        input: Some(PickInput::Secret {
            label: "GitHub 个人令牌",
            hint: "在 GitHub「Settings → Developer settings → Personal access tokens」生成；会明文存进 mcp.json",
        }),
    },
    McpPick {
        name: "notion",
        title: "Notion",
        description: "搜索、读写 Notion 页面和数据库",
        needs: "要登录 Notion",
        config: r#"{"type": "http", "url": "https://mcp.notion.com/mcp"}"#,
        input: None,
    },
    McpPick {
        name: "memory",
        title: "长期记忆",
        description: "把人物、事实存成知识图谱，跨会话记住",
        needs: "需要 Node.js",
        config: r#"{"command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"]}"#,
        input: None,
    },
    McpPick {
        name: "sequential-thinking",
        title: "分步思考",
        description: "让模型把难题拆成一步步想，可以回头修改",
        needs: "需要 Node.js",
        config: r#"{"command": "npx", "args": ["-y", "@modelcontextprotocol/server-sequential-thinking"]}"#,
        input: None,
    },
    McpPick {
        name: "time",
        title: "时间与时区",
        description: "取当前时间、换算时区",
        needs: "需要 uv",
        config: r#"{"command": "uvx", "args": ["mcp-server-time"]}"#,
        input: None,
    },
];

/// 官方市场里的推荐插件（都在 `OFFICIAL_PLUGINS` 这一个仓库里，整市场仓库也列得出来）。
pub const PLUGIN_PICKS: &[PluginPick] = &[
    PluginPick {
        name: "feature-dev",
        title: "功能开发流程",
        description: "/feature-dev：先摸清代码、再设计方案、最后审查，带三个专职子 agent",
        path: "plugins/feature-dev",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "pr-review-toolkit",
        title: "PR 审查工具箱",
        description: "/review-pr：分别查注释、测试、错误处理、类型设计和代码质量",
        path: "plugins/pr-review-toolkit",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "code-review",
        title: "代码审查",
        description: "/code-review：多个 agent 审查 PR，按把握程度打分过滤",
        path: "plugins/code-review",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "code-simplifier",
        title: "代码精简",
        description: "子 agent：在不改行为的前提下让代码更清楚、更一致",
        path: "plugins/code-simplifier",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "commit-commands",
        title: "提交命令",
        description: "/commit、/commit-push-pr：一句话提交、推送、开 PR",
        path: "plugins/commit-commands",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "frontend-design",
        title: "前端设计",
        description: "技能：做出有设计感、能直接上线的网页界面",
        path: "plugins/frontend-design",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "playground",
        title: "交互演示页",
        description: "技能：生成单文件的 HTML 演示页，带控件和实时预览",
        path: "plugins/playground",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "skill-creator",
        title: "技能创作",
        description: "技能：写新技能、改进已有技能，并评测效果",
        path: "plugins/skill-creator",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "claude-md-management",
        title: "项目说明维护",
        description: "检查、更新项目说明文件（CLAUDE.md），把这次学到的记下来",
        path: "plugins/claude-md-management",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "mcp-server-dev",
        title: "MCP 服务开发",
        description: "技能：设计和实现 MCP 服务器（远程、本地、MCPB 打包）",
        path: "plugins/mcp-server-dev",
        repo: None,
        rev: "",
    },
    PluginPick {
        name: "plugin-dev",
        title: "插件开发",
        description: "/create-plugin 和七个技能：写命令、技能、子 agent、钩子",
        path: "plugins/plugin-dev",
        repo: None,
        rev: "",
    },
];

/// 别的仓库里的推荐插件（市场清单里 `source` 指向别的仓库的那一类）。
/// `path` 是那个仓库里的插件目录，`rev` 钉一个 tag（跟着市场清单里的 `ref` 走，改版了也能装回老版本）。
/// 只收装完就能用的（不要 bash / python 钩子、不要 LSP）。
pub const EXTERNAL_PLUGIN_PICKS: &[PluginPick] = &[
    PluginPick {
        name: "aws-core",
        title: "AWS 核心",
        description: "技能：在 AWS 上写基础设施代码、用核心服务、完成常见任务",
        path: "plugins/aws-core",
        repo: Some("aws/agent-toolkit-for-aws"),
        rev: "34b3eccfd8b619ea0cb4ba04a9be9dfefebd94e7",
    },
    PluginPick {
        name: "auth0",
        title: "Auth0 登录",
        description: "技能：给应用加登录、SSO、MFA 和权限控制",
        path: "plugins/auth0",
        repo: Some("auth0/agent-skills"),
        rev: "7b3821d533430eef35fd7406a87b7b8d02302bdf",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pick_is_valid_json() {
        for pick in MCP_PICKS {
            let entry = pick.entry("C:\\Users\\a \"b\"");
            assert!(entry.is_object(), "{}", pick.name);
        }
        let fs = MCP_PICKS.iter().find(|p| p.name == "filesystem").unwrap();
        assert_eq!(fs.entry("D:\\notes")["args"][2], "D:\\notes");
        let gh = MCP_PICKS.iter().find(|p| p.name == "github").unwrap();
        assert_eq!(gh.entry("tok")["headers"]["Authorization"], "Bearer tok");
        for pick in all_plugin_picks() {
            assert!(pick.path.ends_with(pick.name), "{}", pick.name);
            // 官方市场里的插件路径带 `plugins/`；别的仓库里的写自己仓库的目录
            assert!(pick.repo.is_some() || pick.path.starts_with("plugins/"), "{}", pick.name);
            assert!(pick.repo.is_none() || !pick.rev.is_empty(), "{} 没钉版本", pick.name);
        }
        // 名字不重复（启用状态按名字记）
        let mut names: Vec<&str> = all_plugin_picks().map(|p| p.name).collect();
        names.sort();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names, deduped);
    }

    /// 联网把每个推荐插件真装一遍：名字对得上、读出来有内容、没有报错。
    /// `cargo test -p claudesktop-core catalog -- --ignored`
    #[test]
    #[ignore = "要联网"]
    fn every_plugin_pick_installs_cleanly() {
        let dir = std::env::temp_dir().join(format!("cd-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for pick in all_plugin_picks() {
            crate::plugin_git::install_at(
                &dir,
                pick.repo_url(),
                pick.rev,
                Some(pick.path),
                pick.name,
            )
            .unwrap_or_else(|e| panic!("{}：{e}", pick.name));
        }
        let found = crate::plugin::discover(&dir);
        for pick in all_plugin_picks() {
            let p = found.iter().find(|p| p.name == pick.name).expect(pick.name);
            assert!(p.problems.is_empty(), "{}：{:?}", pick.name, p.problems);
            let parts = p.commands.len() + p.skills.len() + p.agents.len();
            assert!(parts > 0 && p.hooks.is_empty(), "{}", pick.name);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
