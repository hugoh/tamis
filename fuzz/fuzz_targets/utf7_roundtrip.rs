#![no_main]

use libfuzzer_sys::fuzz_target;
use tamis::mail::utf7;

fuzz_target!(|name: &str| {
    assert_eq!(utf7::decode(&utf7::encode(name)), name);
});
