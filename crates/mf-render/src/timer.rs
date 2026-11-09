//! GPU time of the 3D view's render pass, from timestamp queries
//! (MF-RT-001, MF-RT-005).
//!
//! Results are read back asynchronously, so a frame's GPU time arrives a
//! frame or more after it was drawn ([`GpuTimer::collect`]). Nothing here
//! ever blocks on the GPU. A device without
//! [`wgpu::Features::TIMESTAMP_QUERY`] has no timer ([`GpuTimer::new`]
//! returns `None`), so callers report "unsupported", never a zero.
//!
//! The timestamps are the pass's own begin and end writes. A sample is
//! only kept if it is plausible: both values changed since the slot's last
//! use (Metal sometimes skips a write, leaving the old value) and the end
//! is after the start. Anything else is counted in [`GpuTimer::failed`],
//! never recorded as a time.
//!
//! On Apple GPUs (Metal) the begin write is the start of vertex work and
//! the end write the end of fragment work. For a heavy pass (a 5M-triangle
//! mesh) they come back with the end ~2.5 ms *before* the start, so nearly
//! every sample there is unusable; small passes time fine. (Tried and
//! dropped: an empty marker pass's end as the start, which Metal skips
//! about half the time.) GPU time is telemetry; the frame-time gate
//! (MF-RT-001) measures whole frames, including waiting for the GPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// Read-backs in flight at once. A frame drawn while all are busy isn't
/// timed.
const SLOTS: usize = 4;
const QUERY_BYTES: u64 = 2 * std::mem::size_of::<u64>() as u64;

const FREE: u8 = 0;
const MAPPING: u8 = 1;
const MAPPED: u8 = 2;
const FAILED: u8 = 3;

/// One frame's timestamps: its own query set (so a skipped write leaves
/// this slot's own last value, which is recognised), the buffer they are
/// resolved into, and the buffer read back.
struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// The frame whose times this slot holds (while not free).
    frame: u64,
    /// The values read from this slot last time: a query that still holds
    /// one of them wasn't written.
    last: [u64; 2],
}

pub struct GpuTimer {
    slots: Vec<Slot>,
    /// Nanoseconds per timestamp tick.
    period: f64,
    /// Slot chosen for the frame being encoded.
    encoding: Option<usize>,
    /// Samples that couldn't be used: a failed read-back (a lost device), a
    /// skipped write or timestamps out of order. Those frames have no GPU
    /// time.
    pub failed: u64,
}

impl GpuTimer {
    /// `None` when the device can't time GPU work.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<GpuTimer> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: QUERY_BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        let slots = (0..SLOTS)
            .map(|_| Slot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("mf view timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolve: buffer(
                    "mf view timestamp resolve",
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                ),
                readback: buffer(
                    "mf view timestamp readback",
                    wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                ),
                state: Arc::new(AtomicU8::new(FREE)),
                frame: 0,
                last: [0; 2],
            })
            .collect();
        Some(GpuTimer {
            slots,
            period: f64::from(queue.get_timestamp_period()),
            encoding: None,
            failed: 0,
        })
    }

    /// Start timing frame `frame`'s pass: the timestamp writes to give it,
    /// or `None` if every read-back is still busy (the frame isn't timed).
    pub(crate) fn begin(&mut self, frame: u64) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        let free = self
            .slots
            .iter()
            .position(|s| s.state.load(Ordering::Acquire) == FREE)?;
        self.encoding = Some(free);
        let slot = &mut self.slots[free];
        slot.frame = frame;
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &slot.queries,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        })
    }

    /// After the timed pass: copy its timestamps out for reading back.
    pub(crate) fn resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(slot) = self.encoding else { return };
        let slot = &self.slots[slot];
        encoder.resolve_query_set(&slot.queries, 0..2, &slot.resolve, 0);
        encoder.copy_buffer_to_buffer(&slot.resolve, 0, &slot.readback, 0, QUERY_BYTES);
    }

    /// After the submit: ask for the read-back (it completes on a later poll).
    pub(crate) fn submitted(&mut self) {
        let Some(slot) = self.encoding.take() else {
            return;
        };
        let slot = &self.slots[slot];
        slot.state.store(MAPPING, Ordering::Release);
        let state = slot.state.clone();
        slot.readback.map_async(wgpu::MapMode::Read, .., move |r| {
            state.store(if r.is_ok() { MAPPED } else { FAILED }, Ordering::Release);
        });
    }

    /// GPU times read back since the last call, as (frame, duration), in
    /// frame order. Never blocks.
    pub fn collect(&mut self, device: &wgpu::Device) -> Vec<(u64, Duration)> {
        // A failed poll means a lost device; its read-backs then fail and
        // are counted in `failed`, so nothing is hidden here.
        // allow-swallow: device loss is reported by the read-back callbacks (FAILED)
        let _ = device.poll(wgpu::PollType::Poll);
        let mut out = Vec::new();
        for slot in &mut self.slots {
            let ticks = match slot.state.load(Ordering::Acquire) {
                MAPPED => {
                    let ticks = match slot.readback.get_mapped_range(..) {
                        Ok(data) => Some(bytemuck::pod_read_unaligned::<[u64; 2]>(
                            &data[..QUERY_BYTES as usize],
                        )),
                        // Counted as failed below.
                        Err(_) => None,
                    };
                    slot.readback.unmap();
                    ticks
                }
                FAILED => None,
                _ => continue,
            };
            slot.state.store(FREE, Ordering::Release);
            match ticks {
                Some([start, end])
                    if start != slot.last[0] && end != slot.last[1] && end > start =>
                {
                    slot.last = [start, end];
                    let ns = (end - start) as f64 * self.period;
                    out.push((slot.frame, Duration::from_nanos(ns.round() as u64)));
                }
                Some(ticks) => {
                    slot.last = ticks;
                    self.failed += 1;
                }
                None => self.failed += 1,
            }
        }
        out.sort_unstable_by_key(|(frame, _)| *frame);
        out
    }
}
