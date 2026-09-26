//! Visible presentation of untrusted text. Escaping is never authorization:
//! approval paths reject ambiguous source text and retain its original digest.
use std::fmt::Write as _;

/// Characters that can reorder, conceal or overwrite an approval display.
/// Ordinary LF/tab layout is allowed; JSON renderers already quote those bytes.
pub fn ambiguous(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\t'))
        || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}'
            | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}' | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}' | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
}

pub fn has_ambiguous_json(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(s) => s.chars().any(ambiguous),
        serde_json::Value::Array(values) => values.iter().any(has_ambiguous_json),
        serde_json::Value::Object(values) => values
            .iter()
            .any(|(k, v)| k.chars().any(ambiguous) || has_ambiguous_json(v)),
        _ => false,
    }
}

/// Escape controls visibly, retaining ordinary multiline layout. JSON-compatible
/// Unicode escapes also keep a serialized JSON document valid and round-trippable.
pub fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if ambiguous(c) {
            for unit in c.encode_utf16(&mut [0; 2]) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_controls_are_visible_and_json_preserves_original_bytes() {
        let original = serde_json::json!({"command": "echo café\u{202e}\u{200b}\u{061c}\u{e0001}\u{1b}c\r\u{009b}2J"});
        assert!(has_ambiguous_json(&original));
        let rendered = visible(&serde_json::to_string_pretty(&original).unwrap());
        assert!(!rendered.chars().any(ambiguous));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
            original
        );
        assert_eq!(visible("\u{1b}c\r\u{009b}2J"), "\\u001bc\\u000d\\u009b2J");
        assert_eq!(visible("café\n\t日本語"), "café\n\t日本語");
        assert!(has_ambiguous_json(
            &serde_json::json!({"bad\u{200b}": "ok"})
        ));
    }
}
