#![no_main]

//! Fuzz target for the OBJ reader (MF-IO-002). Arbitrary bytes are not
//! guaranteed to be UTF-8, so invalid UTF-8 is treated as "no input to
//! parse" rather than skipped: `read_model_with` does the same UTF-8 check
//! before calling `obj::read`, so this mirrors the real entry point.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = mf_io::obj::read(text);
    }
});
