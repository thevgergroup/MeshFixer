//! Serialisable summaries of frame times and memory (MF-RT-005), shared by
//! the desktop Metrics panel, `meshfixer bench orbit` and `--metrics`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::{FrameTimes, GpuSummary, Percentiles, ProcessMemory};

/// Milliseconds, from whole nanoseconds (no `as_secs_f64() * 1000`
/// rounding drift: 49.5 ms is exactly 49.5).
pub fn duration_ms(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1e6
}

/// Percentiles of a set of durations, in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DurationStats {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    /// The worst one.
    pub max_ms: f64,
    /// How many durations these are drawn from.
    pub frames: usize,
}

impl From<&Percentiles> for DurationStats {
    fn from(p: &Percentiles) -> DurationStats {
        DurationStats {
            p50_ms: duration_ms(p.p50()),
            p95_ms: duration_ms(p.p95()),
            p99_ms: duration_ms(p.p99()),
            max_ms: duration_ms(p.max()),
            frames: p.count(),
        }
    }
}

/// GPU time per frame from timestamp queries, always with the counts it
/// is drawn from: on Metal most samples of a heavy pass are unusable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuStats {
    /// False: the device has no timestamp queries (nothing else is set).
    pub supported: bool,
    /// Percentiles of the frames with a usable time (`None`: none yet).
    pub measured: Option<DurationStats>,
    pub unusable_frames: usize,
    pub pending_frames: usize,
}

impl GpuStats {
    pub fn of(summary: &GpuSummary) -> GpuStats {
        match summary {
            GpuSummary::Unsupported => GpuStats {
                supported: false,
                measured: None,
                unusable_frames: 0,
                pending_frames: 0,
            },
            GpuSummary::Supported {
                measured,
                unusable,
                pending,
            } => GpuStats {
                supported: true,
                measured: measured.as_ref().map(DurationStats::from),
                unusable_frames: *unusable,
                pending_frames: *pending,
            },
        }
    }

    /// Frames drawn on the GPU: measured, unusable and pending.
    pub fn drawn_frames(&self) -> usize {
        self.measured.as_ref().map_or(0, |m| m.frames) + self.unusable_frames + self.pending_frames
    }

    /// One line for people: "GPU p95 12 ms from 66 of 1000 frames; 934
    /// unusable". Never a bare figure.
    pub fn describe(&self) -> String {
        if !self.supported {
            return "GPU time: not supported on this device".into();
        }
        let drawn = self.drawn_frames();
        if drawn == 0 {
            return "GPU time: no frames drawn yet".into();
        }
        let mut rest = Vec::new();
        if self.unusable_frames > 0 {
            rest.push(format!("{} unusable", self.unusable_frames));
        }
        if self.pending_frames > 0 {
            rest.push(format!("{} pending", self.pending_frames));
        }
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!("; {}", rest.join(", "))
        };
        match &self.measured {
            Some(m) => format!(
                "GPU p95 {:.1} ms from {} of {drawn} frames{rest}",
                m.p95_ms, m.frames
            ),
            None => format!("GPU time: no usable samples (0 of {drawn} frames{rest})"),
        }
    }
}

/// The kept frames' CPU, GPU and frame-to-frame times.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrameSummary {
    /// CPU time per frame (update, encode and submit).
    pub cpu: Option<DurationStats>,
    pub gpu: GpuStats,
    /// Frame to frame (start to start), every interval, idle gaps included.
    pub interval: Option<DurationStats>,
    /// Frame to frame over the intervals shorter than [`CONTINUOUS_GAP`]:
    /// frames redrawn back to back (the app repaints on demand, so a long
    /// gap is idling, not a slow frame).
    pub continuous: Option<DurationStats>,
    /// Intervals of [`CONTINUOUS_GAP`] or more (idle).
    pub idle_gaps: usize,
    /// 1 / the median continuous interval; `None` without one.
    pub fps: Option<f64>,
}

/// A frame-to-frame gap this long or longer is the app idling between
/// repaints, not a frame taking that long.
pub const CONTINUOUS_GAP: Duration = Duration::from_millis(100);

/// Frames per second at a frame time of `d` (`None` for zero).
pub fn fps(d: Duration) -> Option<f64> {
    (d > Duration::ZERO).then(|| 1e9 / d.as_nanos() as f64)
}

impl FrameTimes {
    /// Percentiles of everything kept. O(kept frames): sorts up to
    /// [`crate::FRAME_HISTORY`] durations three times, so callers on a UI
    /// thread refresh it now and then, not every frame.
    pub fn summary(&self) -> FrameSummary {
        let intervals: Vec<Duration> = self.frames().filter_map(|f| f.interval).collect();
        let (continuous, idle): (Vec<Duration>, Vec<Duration>) =
            intervals.iter().partition(|d| **d < CONTINUOUS_GAP);
        let continuous = Percentiles::of(continuous);
        FrameSummary {
            cpu: self.cpu().as_ref().map(DurationStats::from),
            gpu: GpuStats::of(&self.gpu()),
            interval: Percentiles::of(intervals).as_ref().map(DurationStats::from),
            fps: continuous.as_ref().and_then(|p| fps(p.p50())),
            continuous: continuous.as_ref().map(DurationStats::from),
            idle_gaps: idle.len(),
        }
    }
}

/// GPU buffer bytes by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct GpuBufferBytes {
    /// Surface vertices (per face chunk).
    pub geometry: u64,
    /// Per-face state (selection and defect tints).
    pub state: u64,
    /// Boundary outlines and other line overlays.
    pub lines: u64,
    /// Render targets (colour, multisample and depth).
    pub textures: u64,
}

impl GpuBufferBytes {
    pub fn total(&self) -> u64 {
        self.geometry + self.state + self.lines + self.textures
    }
}

/// Where memory goes (MF-RT-005). Counted figures and the OS's figures
/// side by side, never one standing in for the other; a figure that isn't
/// known is `None` (with the reason), never a guess.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MemoryBreakdown {
    /// The mesh's own storage (`Mesh::heap_bytes`).
    pub mesh_storage_bytes: u64,
    /// The cached topology; `None` while it isn't computed.
    pub topology_bytes: Option<u64>,
    /// How far the heap rose above its level at the start of the last
    /// operation (counting allocator, whole process).
    pub scratch_peak_bytes: Option<u64>,
    /// Why `scratch_peak_bytes` is `None` ("no operation run", "no counting
    /// allocator").
    pub scratch_peak_note: Option<String>,
    /// GPU allocations; `None` when nothing is on a GPU.
    pub gpu: Option<GpuBufferBytes>,
    /// Resident pages (excludes compressed and swapped-out pages).
    pub process_rss_bytes: Option<u64>,
    /// macOS physical footprint: resident, compressed and swapped pages,
    /// GPU buffers included (Activity Monitor's "Memory").
    pub phys_footprint_bytes: Option<u64>,
    /// Why the OS figures are missing.
    pub rss_error: Option<String>,
}

impl MemoryBreakdown {
    /// Set the OS's figures now (or the reason there are none).
    pub fn with_process_now(mut self) -> MemoryBreakdown {
        self.set_process(crate::process_memory());
        self
    }

    pub fn set_process(&mut self, m: Result<ProcessMemory, String>) {
        match m {
            Ok(m) => {
                self.process_rss_bytes = Some(m.resident_bytes);
                self.phys_footprint_bytes = m.phys_footprint_bytes;
                self.rss_error = None;
            }
            Err(e) => {
                self.process_rss_bytes = None;
                self.phys_footprint_bytes = None;
                self.rss_error = Some(e);
            }
        }
    }
}

/// The process's memory, sampled on a thread of its own (cheap on Linux
/// and macOS, but Windows runs `tasklist`, far too slow for a UI frame).
/// Samples at once, then every `every`, until dropped.
pub struct ProcessSampler {
    latest: Arc<Mutex<Option<Result<ProcessMemory, String>>>>,
    stop: Option<std::sync::mpsc::Sender<()>>,
}

impl ProcessSampler {
    pub fn start(every: Duration) -> std::io::Result<ProcessSampler> {
        let latest = Arc::new(Mutex::new(None));
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let slot = latest.clone();
        std::thread::Builder::new()
            .name("mf-process-memory".into())
            .spawn(move || {
                loop {
                    let m = crate::process_memory();
                    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(m);
                    match stopped.recv_timeout(every) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        // Dropped (or told to stop): done.
                        _ => return,
                    }
                }
            })?;
        Ok(ProcessSampler {
            latest,
            stop: Some(stop),
        })
    }

    /// The latest sample; `None` before the first.
    pub fn latest(&self) -> Option<Result<ProcessMemory, String>> {
        self.latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Drop for ProcessSampler {
    fn drop(&mut self) {
        // Dropping the sender wakes the thread, which then returns.
        self.stop.take();
    }
}
