//! Frame timing for the desktop app (MF-RT-001, MF-RT-005).
//!
//! [`FrameTimes`] keeps the last [`FRAME_HISTORY`] frames and records three
//! things separately for each: CPU time (the app's update, including the 3D
//! view's encode and submit), GPU time (from GPU timestamp queries, which
//! arrive a frame or more later) and the interval since the previous frame
//! started. A device without timestamp queries records
//! [`GpuTime::Unsupported`], never a zero.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How many frames [`FrameTimes`] keeps.
pub const FRAME_HISTORY: usize = 1000;

/// What is known about one frame's GPU time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuTime {
    /// The device can't time GPU work (no timestamp queries).
    Unsupported,
    /// The frame drew nothing on the GPU that is timed (the 3D view was
    /// unchanged, so it wasn't redrawn).
    NotRendered,
    /// Timed; the result hasn't been read back yet.
    Pending,
    Measured(Duration),
}

/// One frame's record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSample {
    pub index: u64,
    /// `None` until the frame ends.
    pub cpu: Option<Duration>,
    pub gpu: GpuTime,
    /// Since the previous frame started; `None` for the first frame.
    pub interval: Option<Duration>,
}

/// Percentiles of a set of durations (nearest rank).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Percentiles {
    sorted: Vec<Duration>,
}

impl Percentiles {
    /// `None` for no samples: there is no percentile of nothing.
    pub fn of(mut samples: Vec<Duration>) -> Option<Percentiles> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        Some(Percentiles { sorted: samples })
    }

    /// The `p`th percentile (0 < p ≤ 100), nearest rank.
    pub fn percentile(&self, p: f64) -> Duration {
        let n = self.sorted.len();
        let rank = ((p / 100.0) * n as f64).ceil() as usize;
        self.sorted[rank.clamp(1, n) - 1]
    }

    pub fn p50(&self) -> Duration {
        self.percentile(50.0)
    }

    pub fn p95(&self) -> Duration {
        self.percentile(95.0)
    }

    pub fn p99(&self) -> Duration {
        self.percentile(99.0)
    }

    pub fn max(&self) -> Duration {
        self.sorted[self.sorted.len() - 1]
    }

    pub fn count(&self) -> usize {
        self.sorted.len()
    }
}

/// GPU times over the kept frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuSummary {
    /// The device can't time GPU work.
    Unsupported,
    /// Supported, but no timed frame has been read back yet.
    NoSamples,
    Measured(Percentiles),
}

/// The last [`FRAME_HISTORY`] frames.
#[derive(Debug, Clone, Default)]
pub struct FrameTimes {
    frames: VecDeque<FrameSample>,
    next: u64,
    last_start: Option<Instant>,
}

impl FrameTimes {
    pub fn new() -> FrameTimes {
        FrameTimes::default()
    }

    /// A frame starts at `now`: returns its index (for [`FrameTimes::end`]
    /// and [`FrameTimes::set_gpu`]) and records the interval since the
    /// previous frame started.
    pub fn begin(&mut self, now: Instant) -> u64 {
        let index = self.next;
        self.next += 1;
        let interval = self.last_start.map(|t| now.saturating_duration_since(t));
        self.last_start = Some(now);
        if self.frames.len() == FRAME_HISTORY {
            self.frames.pop_front();
        }
        self.frames.push_back(FrameSample {
            index,
            cpu: None,
            gpu: GpuTime::Pending,
            interval,
        });
        index
    }

    /// Frame `index` finished on the CPU after `cpu`; `gpu` says whether
    /// GPU work was timed.
    pub fn end(&mut self, index: u64, cpu: Duration, gpu: GpuTime) {
        if let Some(f) = self.frame_mut(index) {
            f.cpu = Some(cpu);
            f.gpu = gpu;
        }
    }

    /// A GPU time read back for frame `index`. Ignored for a frame that
    /// is no longer kept, or that wasn't timed.
    pub fn set_gpu(&mut self, index: u64, gpu: Duration) {
        if let Some(f) = self.frame_mut(index)
            && f.gpu == GpuTime::Pending
        {
            f.gpu = GpuTime::Measured(gpu);
        }
    }

    fn frame_mut(&mut self, index: u64) -> Option<&mut FrameSample> {
        let first = self.frames.front()?.index;
        // allow-swallow: an index too far ahead to fit is simply not a kept frame
        let at = usize::try_from(index.checked_sub(first)?).ok()?;
        self.frames.get_mut(at)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn frames(&self) -> impl Iterator<Item = &FrameSample> {
        self.frames.iter()
    }

    pub fn cpu(&self) -> Option<Percentiles> {
        Percentiles::of(self.frames.iter().filter_map(|f| f.cpu).collect())
    }

    pub fn interval(&self) -> Option<Percentiles> {
        Percentiles::of(self.frames.iter().filter_map(|f| f.interval).collect())
    }

    pub fn gpu(&self) -> GpuSummary {
        if self.frames.iter().any(|f| f.gpu == GpuTime::Unsupported) {
            return GpuSummary::Unsupported;
        }
        let measured = self
            .frames
            .iter()
            .filter_map(|f| match f.gpu {
                GpuTime::Measured(d) => Some(d),
                _ => None,
            })
            .collect();
        match Percentiles::of(measured) {
            Some(p) => GpuSummary::Measured(p),
            None => GpuSummary::NoSamples,
        }
    }
}
