#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| usnm_fuzz::dead_letter(data));
