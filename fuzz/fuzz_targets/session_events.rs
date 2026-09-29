#![no_main]

use agent::{RecordedEvent, validate_events};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(events) = serde_json::from_slice::<Vec<RecordedEvent>>(data) {
        let _ = validate_events(&events);
    }
});
