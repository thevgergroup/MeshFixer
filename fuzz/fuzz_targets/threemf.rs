#![no_main]

//! Fuzz target for the 3MF reader (MF-IO-002): arbitrary bytes as a zip
//! archive, including hostile ones (declared sizes far beyond their
//! compressed size, deeply nested or wildly disproportionate entries).
//! `threemf::read` must reject these with a structured error, never hang
//! or allocate unboundedly.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = mf_io::threemf::read(data);
});
