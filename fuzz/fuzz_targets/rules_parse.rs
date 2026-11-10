#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &[u8]| {
    let _ = tamis::rules::Rules::parse(source);
});
