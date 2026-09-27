use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PublishEncoding {
    #[default]
    Text,
    Hex,
}

impl PublishEncoding {
    pub(crate) fn decode(self, input: &str) -> Result<Vec<u8>, String> {
        match self {
            Self::Text => Ok(input.as_bytes().to_vec()),
            Self::Hex => {
                let mut bytes = Vec::new();
                let mut high = None;
                for ch in input.chars().filter(|ch| !ch.is_ascii_whitespace()) {
                    let digit = ch.to_digit(16).ok_or_else(|| {
                        "Hex payload must contain only hex digits and ASCII whitespace".to_string()
                    })? as u8;
                    if let Some(first) = high.take() {
                        bytes.push((first << 4) | digit);
                    } else {
                        high = Some(digit);
                    }
                }
                if high.is_some() {
                    return Err("Hex payload must contain an even number of digits".to_string());
                }
                Ok(bytes)
            }
        }
    }

    pub(crate) fn encode(self, bytes: &[u8]) -> Result<String, String> {
        match self {
            Self::Text => std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| {
                "Payload is not valid UTF-8; keep Hex mode to preserve its bytes".into()
            }),
            Self::Hex => {
                const DIGITS: &[u8; 16] = b"0123456789abcdef";
                let mut hex = String::with_capacity(bytes.len() * 2);
                for &byte in bytes {
                    hex.push(DIGITS[(byte >> 4) as usize] as char);
                    hex.push(DIGITS[(byte & 15) as usize] as char);
                }
                Ok(hex)
            }
        }
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> (Self, String) {
        if let Ok(text) = Self::Text.encode(bytes) {
            (Self::Text, text)
        } else {
            (Self::Hex, Self::Hex.encode(bytes).unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn republish_preserves_every_byte() {
        for bytes in [
            (0..=255).collect::<Vec<u8>>(),
            b" \ttext\r\n\0 ".to_vec(),
            "\u{feff}Grüße 🌍\r\n".as_bytes().to_vec(),
            vec![],
        ] {
            let (encoding, draft) = PublishEncoding::from_bytes(&bytes);
            assert_eq!(encoding.decode(&draft).unwrap(), bytes);
        }
        assert_eq!(PublishEncoding::from_bytes(&[0xff]).0, PublishEncoding::Hex);
    }

    #[test]
    fn hex_accepts_byte_pairs_and_rejects_invalid_input() {
        assert_eq!(
            PublishEncoding::Hex.decode("00 ff\n80\t0A\r").unwrap(),
            [0, 255, 128, 10]
        );
        assert!(PublishEncoding::Hex.decode("0").is_err());
        assert!(PublishEncoding::Hex.decode("gg").is_err());
        assert!(PublishEncoding::Hex.decode("0x12").is_err());
        assert!(PublishEncoding::Hex.decode("ff\u{a0}").is_err());
        assert!(PublishEncoding::Text.encode(&[0xff]).is_err());
    }

    #[test]
    fn changing_encoding_preserves_text_bytes() {
        let bytes = "  Grüße\r\n".as_bytes();
        let hex = PublishEncoding::Hex.encode(bytes).unwrap();
        let decoded = PublishEncoding::Hex.decode(&hex).unwrap();
        assert_eq!(
            PublishEncoding::Text.encode(&decoded).unwrap().as_bytes(),
            bytes
        );
    }
}
