#![no_main]

//! Fuzz target for the STL reader (MF-IO-002). Hostile input must come back
//! as a structured `IoError`, never a panic, unbounded allocation or hang:
//! `mf_io::stl::read` is called directly on arbitrary bytes, with no
//! upstream size limit, so the reader's own limits are what is under test.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = mf_io::stl::read(data);
});
