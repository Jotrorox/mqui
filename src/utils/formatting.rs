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

pub(crate) fn format_json(payload: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
}

#[cfg(test)]
mod tests {
    use super::format_json;

    #[test]
    fn formats_valid_json() {
        assert_eq!(
            format_json(br#"{"answer":42}"#).as_deref(),
            Some("{\n  \"answer\": 42\n}")
        );
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(format_json(b"{broken").is_none());
        assert!(format_json(&[0xff]).is_none());
    }
}
