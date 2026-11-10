#![allow(clippy::unwrap_used)] // tests may panic on failure

//! A worker waiting for the GPU must not stall the UI thread's GPU calls
//! (MF-RT-001, M4 final review I4).
//!
//! `device.poll(Wait)` holds wgpu-core's device resource lock (read) for
//! the whole wait. A thread freeing a resource meanwhile waits to take it
//! for writing, and the lock is fair, so every later reader (the UI thread
//! writing egui's buffers or submitting a frame) queues behind that writer
//! until the GPU is done. [`mf_render::wait_for_submitted`] must hold the
//! lock only to check.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use mf_render::offscreen;

/// GPU work that takes a while: `copies` copies of a `bytes`-byte buffer.
fn slow_gpu_work(device: &wgpu::Device, bytes: u64, copies: u32) -> wgpu::CommandBuffer {
    let buffer = |label| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    let (a, b) = (buffer("slow src"), buffer("slow dst"));
    let mut encoder = device.create_command_encoder(&Default::default());
    for i in 0..copies {
        if i % 2 == 0 {
            encoder.copy_buffer_to_buffer(&a, 0, &b, 0, bytes);
        } else {
            encoder.copy_buffer_to_buffer(&b, 0, &a, 0, bytes);
        }
    }
    encoder.finish()
}

#[test]
fn a_worker_waiting_for_the_gpu_does_not_stall_other_threads_gpu_calls() {
    let Ok((device, queue)) = offscreen::shared_device() else {
        mf_fixtures::skip::gpu_unavailable(
            "a_worker_waiting_for_the_gpu_does_not_stall_other_threads_gpu_calls",
        );
        return;
    };
    let small = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ui"),
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::VERTEX,
        mapped_at_creation: false,
    });
    // Grow the GPU work until a wait is long enough to judge (≥ 150 ms).
    let mut copies = 16;
    loop {
        let work = slow_gpu_work(&device, 64 << 20, copies);
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let worker = {
            let (device, queue) = (device.clone(), queue.clone());
            std::thread::spawn(move || {
                queue.submit([work]);
                let start = Instant::now();
                waiting_tx.send(()).unwrap();
                mf_render::wait_for_submitted(&device, &queue).unwrap();
                start.elapsed()
            })
        };
        waiting_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(10));
        // Another thread frees a resource: it takes the lock for writing.
        let freeing = {
            let device = device.clone();
            std::thread::spawn(move || {
                let doomed = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("freed"),
                    size: 256,
                    usage: wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                doomed.destroy();
            })
        };
        std::thread::sleep(Duration::from_millis(10));
        // The UI thread writes a buffer and submits, as a frame does.
        let start = Instant::now();
        queue.write_buffer(&small, 0, &[1u8; 256]);
        queue.submit([]);
        let ui = start.elapsed();
        freeing.join().unwrap();
        let gpu = worker.join().unwrap();
        eprintln!("{copies} copies: worker waited {gpu:?}; UI write and submit took {ui:?}");
        if gpu < Duration::from_millis(150) {
            assert!(copies < 1 << 14, "the GPU work never took long enough");
            copies *= 2;
            continue;
        }
        assert!(
            ui < Duration::from_millis(20),
            "the UI thread waited {ui:?} behind a worker's {gpu:?} GPU wait"
        );
        return;
    }
}
