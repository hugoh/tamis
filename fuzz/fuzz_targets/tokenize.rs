#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|raw: &[u8]| {
    let _ = tamis::classify::tokenize(raw);
});
