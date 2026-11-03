#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    mediax_codec::fuzzing::decode_h264(data);
});
