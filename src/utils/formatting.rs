use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn format_timestamp(ts: SystemTime) -> String {
    match ts.duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{}", duration.as_secs()),
        Err(_) => "0".to_string(),
    }
}

pub(crate) fn format_payload(payload: &[u8], as_hex: bool) -> String {
    if as_hex {
        return payload
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
    }

    match String::from_utf8(payload.to_vec()) {
        Ok(text) => text,
        Err(_) => payload
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}
