# 回复排版对比样本

图二（daemon 排障会话）的等价 Markdown 还原，供无头截图回归对比。
设置 `CLAUDESKTOP_OPEN=sample-reply` 可预览；截图前后使用同字号、同窗口对比。

## 1. 测量计数

先确认了现状：

- 会话数按 `SessionMeta` 占位统计，启动时只建最近 100 个的元信息
- 首屏不读 jsonl，激活后由 `ensure_history_loaded` 后台重放
- 内存里每会话保留显示数据约 12 KiB，冷回收后换占位

## 2. 冷会话回收

回收入口在 `reclaim_idle_processes`：

- 仅当 agent 握手声明 resume / load 时才可 detach
- 10 分钟无活动且无后台任务才关闭
- Chrome 里请用 **http://localhost:4390/**（等价，200）
- 备用入口是 http://127.0.0.1:4390/，两者共用同一个 daemon

日志路径：`D:\360MoveData\Users\asus\AppData\Roaming\claudesktop\logs\daemon.json`

## 3. 后续隐患

文件 `page/app.js` 里有一段未收敛的轮询，`daemon.json` 重启后计数会翻倍，
建议下一轮先修这个再继续压测。
