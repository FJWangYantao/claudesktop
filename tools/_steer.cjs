// 临时脚本（用完即删）：加入 steer 通道与步间注入。
const fs = require("node:fs");
const f = "crates/builtin/src/lib.rs";
let t = fs.readFileSync(f, "utf8");
function p(find, rep) {
  if (!t.includes(find)) {
    console.error("未找到: " + find.slice(0, 70));
    process.exit(2);
  }
  t = t.replace(find, rep);
}

// 1) 步间安全点：取快照、压缩之前，把 pending_steer 追加成新的用户消息
p(
  `    for _ in 0..inner.max_steps {
        // 用量快到窗口：先压缩再请求`,
  `    for _ in 0..inner.max_steps {
        // 插话的安全点：上一步的 assistant(tool_calls) 和它的全部 tool 结果都已经进历史，
        // 下一轮请求还没发。这里插入用户消息不会破坏消息配对（见 Kernel::steer）。
        for steer in inner.take_steer(session) {
            let full = if steer.attachments.is_empty() {
                steer.text.clone()
            } else {
                claudesktop_core::attach::compose(&steer.text, &steer.attachments)
            };
            inner.push_steered_message(session, full, steer.images);
            inner.emit(
                Some(session),
                KernelEvent::SteerInjected { text: steer.text.clone() },
            );
        }
        // 用量快到窗口：先压缩再请求`,
);

// 2) Inner 上的三个辅助方法，挂在 loaded_tools 旁边
p(
  `    /// 每个延迟工具最后一次被搜到 / 被调用的步序（预算淘汰排序用）。`,
  `    /// 收下运行中收到的插话（由 \`run_steps\` 在步间取出）。
    fn take_steer(&self, session: &str) -> Vec<claudesktop_core::UserPrompt> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .get_mut(session)
            .map(|state| state.pending_steer.drain(..).collect())
            .unwrap_or_default()
    }

    /// 把插话追加成一条新的用户消息（和正常发送的消息走同一套 history / user_inputs）。
    fn push_steered_message(
        &self,
        session: &str,
        text: String,
        images: Vec<claudesktop_core::PromptImage>,
    ) {
        let mut parts = vec![ContentPart::Text(text.clone())];
        for image in images {
            parts.push(ContentPart::Image { media_type: image.media_type, data: image.data });
        }
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session) {
            state.history.push(ChatMessage::with_parts("user", parts));
            state.user_inputs.push(auto::UserInput::Message(text));
            trim_user_inputs(&mut state.user_inputs);
        }
    }

    /// 每个延迟工具最后一次被搜到或被调用的步序（预算淘汰排序用）。`,
);

fs.writeFileSync(f, t, "utf8");
console.log("steer 通道已加入");
