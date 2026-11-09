//! What View ▸ Metrics shows (MF-RT-005): a snapshot of the app's cached
//! counters, refreshed a couple of times a second while the panel is
//! open, never every frame and never by walking a mesh's faces. The
//! process RSS comes from a sampler thread (on macOS each sample runs
//! `ps`, far too slow for a UI frame).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use mf_metrics::{FrameSummary, MemoryBreakdown, RssSampler};

/// How often the snapshot is refreshed while the panel is open.
pub const REFRESH_EVERY: Duration = Duration::from_millis(500);
/// How often the process RSS is sampled while the panel is open.
pub const RSS_EVERY: Duration = Duration::from_secs(1);
/// Finished jobs listed.
pub const RECENT_JOBS: usize = 6;

/// A finished job: what it was, how long it took, and how far the heap
/// rose above its starting level meanwhile (whole app, counting
/// allocator; `None` without one).
#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub label: String,
    pub duration: Duration,
    pub peak_above_start: Option<u64>,
}

/// The last [`RECENT_JOBS`] jobs, newest last.
#[derive(Debug, Clone, Default)]
pub struct RecentJobs(VecDeque<JobRecord>);

impl RecentJobs {
    pub fn push(&mut self, job: JobRecord) {
        if self.0.len() == RECENT_JOBS {
            self.0.pop_front();
        }
        self.0.push_back(job);
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &JobRecord> {
        self.0.iter()
    }

    pub fn last(&self) -> Option<&JobRecord> {
        self.0.back()
    }
}

/// A running job's start, for its [`JobRecord`].
#[derive(Debug, Clone)]
pub struct JobStart {
    pub label: String,
    pub started: Instant,
    /// Live heap at the start (`None` without the counting allocator).
    pub live: Option<u64>,
}

impl JobStart {
    /// Starts a new peak window in the counting allocator, if installed.
    pub fn now(label: &str) -> JobStart {
        let live = mf_metrics::is_installed().then(|| {
            let live = mf_metrics::live_bytes();
            mf_metrics::reset_peak();
            live
        });
        JobStart {
            label: label.trim_end_matches('…').to_owned(),
            started: Instant::now(),
            live,
        }
    }

    pub fn finish(self) -> JobRecord {
        JobRecord {
            duration: self.started.elapsed(),
            peak_above_start: self
                .live
                .map(|live| mf_metrics::peak_bytes().saturating_sub(live)),
            label: self.label,
        }
    }
}

/// Everything the panel shows, as of `taken`.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub taken: Instant,
    pub frames: FrameSummary,
    /// The active object: its storage and cached topology; the last job's
    /// peak; GPU buffers of everything drawn plus the view's textures; RSS.
    pub memory: MemoryBreakdown,
    /// Bytes the active object's GPU mesh uploaded for its last change
    /// (geometry, face states, outlines).
    pub last_upload_bytes: u64,
    /// Mesh geometry uploaded since the app started.
    pub geometry_uploaded_bytes: u64,
    /// Highest live heap since the start (`None` without the counting
    /// allocator).
    pub peak_heap_bytes: Option<u64>,
    pub jobs: Vec<JobRecord>,
}

/// The RSS sampler, started when the panel opens, stopped when it closes.
#[derive(Default)]
pub struct Rss {
    sampler: Option<Result<RssSampler, String>>,
}

impl Rss {
    /// Keep sampling while `on`.
    pub fn keep(&mut self, on: bool) {
        match (on, self.sampler.is_some()) {
            (true, false) => {
                self.sampler = Some(RssSampler::start(RSS_EVERY).map_err(|e| e.to_string()))
            }
            (false, true) => self.sampler = None,
            _ => {}
        }
    }

    /// The latest sample; `None` before the first.
    pub fn latest(&self) -> Option<Result<u64, String>> {
        match self.sampler.as_ref()? {
            Ok(s) => s.latest(),
            Err(e) => Some(Err(format!("can't start the RSS sampler: {e}"))),
        }
    }
}

/// `1.5 GB`, `23.4 MB`, `12.0 KB`, `512 B` (binary units).
pub fn format_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    let x = b as f64;
    if x >= KB * KB * KB {
        format!("{:.2} GB", x / (KB * KB * KB))
    } else if x >= KB * KB {
        format!("{:.1} MB", x / (KB * KB))
    } else if x >= KB {
        format!("{:.1} KB", x / KB)
    } else {
        format!("{b} B")
    }
}

/// `12.3 ms` / `1.23 s`.
pub fn format_duration(d: Duration) -> String {
    if d >= Duration::from_secs(1) {
        format!("{:.2} s", d.as_secs_f64())
    } else {
        format!("{:.1} ms", mf_metrics::duration_ms(d))
    }
}
