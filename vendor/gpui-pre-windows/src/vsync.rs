use std::{
    sync::{
        LazyLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui::FrameRequestSource;
use gpui_util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

// claudesktop patch: motion uses the latest coalesced tick. The profiler keeps
// the first signal to measure how long a frame request waited for delivery.
pub(crate) struct LatestVsyncClock {
    origin: Instant,
    latest_ns: AtomicU64,
}

impl LatestVsyncClock {
    pub(crate) fn new() -> Self {
        Self { origin: Instant::now(), latest_ns: AtomicU64::new(0) }
    }

    pub(crate) fn record(&self, at: Instant) {
        let Some(elapsed) = at.checked_duration_since(self.origin) else {
            return;
        };
        self.latest_ns.store(elapsed.as_nanos() as u64 + 1, Ordering::Release);
    }

    pub(crate) fn take(&self) -> Option<Instant> {
        let ns = self.latest_ns.swap(0, Ordering::AcqRel).checked_sub(1)?;
        Some(self.origin + Duration::from_nanos(ns))
    }

    pub(crate) fn clear(&self) {
        self.latest_ns.store(0, Ordering::Release);
    }
}

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        let f = Box::new(|| unsafe { DwmFlush().is_ok() });
        Self { interval, f }
    }

    // claudesktop patch: reuse the native wait's end time as the motion clock.
    pub(crate) fn wait_for_vsync(&self) -> (FrameRequestSource, Option<Instant>) {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let at = Instant::now();
        let elapsed = at.duration_since(vsync_start);
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
            (FrameRequestSource::LocalSchedule, None)
        } else {
            (FrameRequestSource::NativeCallback, Some(at))
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_compositor_wait_reports_native_callback() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                true
            }),
        };

        let (source, at) = provider.wait_for_vsync();
        assert_eq!(source, FrameRequestSource::NativeCallback);
        assert!(at.is_some());
    }

    #[test]
    fn failed_compositor_wait_reports_local_schedule_even_after_threshold() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                false
            }),
        };

        assert_eq!(provider.wait_for_vsync(), (FrameRequestSource::LocalSchedule, None));
    }

    #[test]
    fn motion_clock_keeps_only_the_latest_tick_until_consumed_or_cleared() {
        let clock = LatestVsyncClock::new();
        let first = Instant::now();
        let second = first + Duration::from_millis(6);
        clock.record(first);
        clock.record(second);
        assert_eq!(clock.take(), Some(second));
        assert_eq!(clock.take(), None);
        clock.record(second);
        clock.clear();
        assert_eq!(clock.take(), None);
    }
}
