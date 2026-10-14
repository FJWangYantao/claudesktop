//! Server-Sent Events 行解析（只取 OpenAI 兼容流式接口用到的子集）。
//!
//! - `data: <payload>`：一条事件；多行 `data:` 按规范以换行拼接，空行结束一条事件
//! - `: ...`：注释（如代理的 `: ping` 保活），忽略
//! - `data: [DONE]`：流结束
//! - `event:` / `id:` / `retry:`：忽略

use std::io::BufRead;

/// 一条解析出的 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Data(String),
    Done,
}

/// 逐条读取 SSE 事件。`should_stop` 每读一行检查一次（取消用）；返回 false 表示被叫停。
pub fn read_events(
    reader: impl BufRead,
    mut should_stop: impl FnMut() -> bool,
    mut on_event: impl FnMut(SseEvent) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    let mut data: Option<String> = None;
    for line in reader.lines() {
        if should_stop() {
            return Ok(false);
        }
        let line = line?;
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            if let Some(payload) = data.take() {
                if dispatch(payload, &mut on_event)? {
                    return Ok(true);
                }
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        if field == "data" {
            match &mut data {
                Some(buf) => {
                    buf.push('\n');
                    buf.push_str(value);
                }
                None => data = Some(value.to_string()),
            }
        }
    }
    // 流在最后一条事件后没有空行就断开：仍然交付
    if let Some(payload) = data.take() {
        dispatch(payload, &mut on_event)?;
    }
    Ok(true)
}

/// 交付一条事件；返回 true 表示遇到 `[DONE]`。
fn dispatch(
    payload: String,
    on_event: &mut impl FnMut(SseEvent) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    if payload.trim() == "[DONE]" {
        on_event(SseEvent::Done)?;
        return Ok(true);
    }
    on_event(SseEvent::Data(payload))?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &str) -> Vec<SseEvent> {
        let mut out = Vec::new();
        read_events(input.as_bytes(), || false, |e| {
            out.push(e);
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn parses_data_comments_and_done() {
        let input = "data: {\"a\":1}\n\n: ping\n\ndata: {\"b\":2}\r\n\r\ndata: [DONE]\n\ndata: {\"after\":1}\n\n";
        assert_eq!(
            collect(input),
            vec![
                SseEvent::Data("{\"a\":1}".into()),
                SseEvent::Data("{\"b\":2}".into()),
                SseEvent::Done,
            ]
        );
    }

    #[test]
    fn joins_multiline_data_and_flushes_tail() {
        let input = "event: x\ndata: line1\ndata:line2\n\ndata: tail";
        assert_eq!(
            collect(input),
            vec![
                SseEvent::Data("line1\nline2".into()),
                SseEvent::Data("tail".into()),
            ]
        );
    }

    #[test]
    fn stop_flag_interrupts() {
        let mut n = 0;
        let finished = read_events("data: 1\n\ndata: 2\n\n".as_bytes(), || {
            n += 1;
            n > 2
        }, |_| Ok(()))
        .unwrap();
        assert!(!finished);
    }
}
