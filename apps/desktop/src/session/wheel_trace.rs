//! 默认关闭的滚轮诊断。时间使用进程单调时钟的相对微秒，便于跨输入、列表和呈现对齐。

use serde_json::{Value, json};
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

thread_local! {
    static TRACE: RefCell<Option<WheelTrace>> = const { RefCell::new(None) };
}

struct WheelTrace {
    started: Instant,
    writer: BufWriter<File>,
}

pub(crate) fn init() {
    let path = std::env::var_os("CLAUDESKTOP_WHEEL_TRACE").map(PathBuf::from);
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if path.is_none() {
            *slot = None;
            return;
        }
        if slot.is_some() {
            return;
        }
        let trace = path.and_then(|path| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            let file = File::create(path).ok()?;
            Some(WheelTrace { started: Instant::now(), writer: BufWriter::new(file) })
        });
        if trace.is_none() {
            eprintln!("[wheel-trace] 无法创建 CLAUDESKTOP_WHEEL_TRACE 指定的日志");
        }
        *slot = trace;
    });
}

pub(crate) fn enabled() -> bool {
    TRACE.with(|slot| slot.borrow().is_some())
}

pub(crate) fn record(kind: &str, fields: Value) {
    TRACE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(trace) = slot.as_mut() else { return };
        let time_us = trace.started.elapsed().as_micros() as u64;
        let event = json!({"kind": kind, "time_us": time_us, "fields": fields});
        if writeln!(trace.writer, "{event}").is_err() {
            *slot = None;
        }
    });
}

pub(crate) fn step(session: u64, requested: f32, applied: f32, edge: bool, direct: bool) {
    if enabled() {
        record(
            "step",
            json!({
                "session": session,
                "requested": requested,
                "applied": applied,
                "edge": edge,
                "direct": direct,
            }),
        );
    }
}

pub(crate) fn flush() {
    TRACE.with(|slot| {
        if let Some(trace) = slot.borrow_mut().as_mut() {
            let _ = trace.writer.flush();
        }
    });
}

#[cfg(test)]
mod tests {
    #[core::prelude::v1::test]
    fn disabled_trace_is_a_noop() {
        assert!(!super::enabled());
        super::record("test", serde_json::json!({"ignored": true}));
        super::flush();
    }
}
