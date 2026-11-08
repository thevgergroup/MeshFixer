#![allow(unsafe_code)] // GlobalAlloc is an unsafe trait; this file only forwards to System.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);
/// Peak since process start; never reset.
static PEAK_ALL: AtomicU64 = AtomicU64::new(0);
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Counting wrapper around the system allocator.
pub struct CountingAlloc;

fn grew(bytes: u64) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
    PEAK_ALL.fetch_max(live, Ordering::Relaxed);
    if !INSTALLED.load(Ordering::Relaxed) {
        INSTALLED.store(true, Ordering::Relaxed);
    }
}

fn shrank(bytes: u64) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged and only updates counters afterwards.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size() as u64);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size() as u64);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) };
        shrank(layout.size() as u64);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: same contract as the caller's.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            shrank(layout.size() as u64);
            grew(new_size as u64);
        }
        p
    }
}

/// True once a binary has installed [`CountingAlloc`] and it has allocated.
pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

pub fn live_bytes() -> u64 {
    LIVE.load(Ordering::Relaxed)
}

pub fn peak_bytes() -> u64 {
    PEAK.load(Ordering::Relaxed)
}

/// Highest live heap since the process started.
pub fn peak_bytes_since_start() -> u64 {
    PEAK_ALL.load(Ordering::Relaxed)
}

/// Start a new peak window at the current live level.
pub fn reset_peak() {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
}
