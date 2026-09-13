//! A deliberately minimal TOML-subset parser for the pack manifest
//! (ticket `.kranz/tickets/pack-contract-gates-prompts.md`).
//!
//! WHY hand-rolled and minimal: the engine's dependency tree has no TOML
//! crate (verified against `Cargo.lock`), and AGENTS.md prefers the standard
//! library and existing utilities over new dependencies — the same trade
//! [`crate::contract_gates`] made for its shell-word lexer. The pack
//! manifest needs only a small, fully-accountable subset: `[table]` and
//! `[[array-of-tables]]` headers, bare keys, basic/literal strings,
//! integers, booleans, and (possibly multi-line) arrays. Everything else
//! TOML allows — dotted keys, inline tables, floats, datetimes, multi-line
//! strings — is REJECTED as an unsupported construct: the pack contract
//! fails closed on shapes this parser cannot fully account for, never
//! guesses. If a future pack genuinely needs the full grammar, adding a
//! vetted `toml` dependency is a deliberate, reviewable decision — not
//! something to grow this parser toward one feature at a time.
//!
//! The output preserves document order (gates register in declared order)
//! and reports every error with a 1-based line number so the lint surface
//! and the fail-closed engine load can name the offending field.

/// One parsed value: the four scalar/collection shapes the subset supports.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Integer(i64),
    Boolean(bool),
    Array(Vec<Value>),
}

impl Value {
    /// Human-readable type name for wrong-type errors naming the field.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "a string",
            Value::Integer(_) => "an integer",
            Value::Boolean(_) => "a boolean",
            Value::Array(_) => "an array",
        }
    }
}

/// One `[table]`'s (or one `[[array]]` item's) key/value entries, in
/// declared order. Duplicate keys were already refused at parse time.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub name: String,
    pub entries: Vec<(String, Value)>,
}

impl Table {
    /// The value for `key`, if declared.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Keys not in `known`, in declared order — the unknown-field failure
    /// class. The caller names the section so the error points at the field.
    pub fn unknown_keys(&self, known: &[&str]) -> Vec<&str> {
        self.entries
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !known.contains(k))
            .collect()
    }
}

/// A top-level section: a singleton `[table]` or one `[[array]]` family
/// with all its items.
#[derive(Debug, Clone, PartialEq)]
pub enum Section {
    Single(Table),
    Array { name: String, items: Vec<Table> },
}

/// The parsed manifest: sections in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub sections: Vec<Section>,
}

impl Document {
    /// The singleton table `name`, if present.
    pub fn single(&self, name: &str) -> Option<&Table> {
        self.sections.iter().find_map(|s| match s {
            Section::Single(t) if t.name == name => Some(t),
            _ => None,
        })
    }

    /// All items of the `[[array]]` family `name`, in declared order.
    pub fn array(&self, name: &str) -> &[Table] {
        self.sections
            .iter()
            .find_map(|s| match s {
                Section::Array { name: n, items } if n == name => Some(items.as_slice()),
                _ => None,
            })
            .unwrap_or(&[])
    }
}

/// Parse the manifest source. Every error carries `line <n>:` so the
/// caller can name the offending field; the subset's limits are stated in
/// the error text rather than silently misparsed.
pub fn parse(source: &str) -> Result<Document, String> {
    let lines: Vec<&str> = source.lines().collect();
    let mut sections: Vec<Section> = Vec::new();
    // Where the next `key = value` lands: nothing (before the first
    // header), a singleton table, or the LAST item of an array family.
    let mut target: Option<usize> = None; // section index
    let mut i = 0;
    while i < lines.len() {
        let line_no = i + 1;
        let trimmed = strip_comment(lines[i]).trim().to_string();
        i += 1;
        if trimmed.is_empty() {
            continue;
        }
        if let Some(name) = header_name(&trimmed, "[[", "]]", line_no)? {
            target = Some(push_array_item(&mut sections, &name, line_no)?);
            continue;
        }
        if let Some(name) = header_name(&trimmed, "[", "]", line_no)? {
            target = Some(push_single(&mut sections, &name, line_no)?);
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            return Err(format!(
                "line {line_no}: expected `key = value`, got `{trimmed}`"
            ));
        };
        let key = trimmed[..eq].trim();
        if !is_bare_key(key) {
            return Err(format!(
                "line {line_no}: unsupported key `{key}` (bare keys only: \
                 letters, digits, `_`, `-`; dotted and quoted keys are not in \
                 the pack TOML subset)"
            ));
        }
        // A value may span lines only as an array: keep consuming while the
        // bracket depth is positive (brackets inside strings never count).
        let mut value_text = trimmed[eq + 1..].trim().to_string();
        while bracket_depth(&value_text).map_err(|e| format!("line {line_no}: {e}"))? > 0 {
            let Some(next) = lines.get(i) else {
                return Err(format!("line {line_no}: unterminated `[` in value"));
            };
            value_text.push('\n');
            value_text.push_str(strip_comment(next));
            i += 1;
        }
        let value = parse_value(&value_text, line_no)?;
        let Some(section) = target.map(|idx| &mut sections[idx]) else {
            return Err(format!(
                "line {line_no}: field `{key}` appears before any `[table]` header"
            ));
        };
        let entries = match section {
            Section::Single(t) => &mut t.entries,
            Section::Array { items, .. } => {
                &mut items.last_mut().expect("array item pushed").entries
            }
        };
        if entries.iter().any(|(k, _)| k == key) {
            return Err(format!(
                "line {line_no}: duplicate field `{key}` in the same table"
            ));
        }
        entries.push((key.to_string(), value));
    }
    Ok(Document { sections })
}

/// The section index of a fresh singleton table; duplicate singletons (or a
/// singleton shadowing an existing array family) are refused.
fn push_single(sections: &mut Vec<Section>, name: &str, line_no: usize) -> Result<usize, String> {
    if sections.iter().any(|s| match s {
        Section::Single(t) => t.name == name,
        Section::Array { name: n, .. } => n == name,
    }) {
        return Err(format!(
            "line {line_no}: duplicate `[{name}]` table (or it already names an \
             `[[{name}]]` array)"
        ));
    }
    sections.push(Section::Single(Table {
        name: name.to_string(),
        entries: Vec::new(),
    }));
    Ok(sections.len() - 1)
}

/// The section index of the array family, with one fresh empty item pushed;
/// an array shadowing an existing singleton table is refused.
fn push_array_item(
    sections: &mut Vec<Section>,
    name: &str,
    line_no: usize,
) -> Result<usize, String> {
    if sections
        .iter()
        .any(|s| matches!(s, Section::Single(t) if t.name == name))
    {
        return Err(format!(
            "line {line_no}: `[[{name}]]` array shadows the existing `[{name}]` table"
        ));
    }
    if let Some(idx) = sections
        .iter()
        .position(|s| matches!(s, Section::Array { name: n, .. } if n == name))
    {
        if let Section::Array { items, .. } = &mut sections[idx] {
            items.push(Table {
                name: name.to_string(),
                entries: Vec::new(),
            });
        }
        return Ok(idx);
    }
    sections.push(Section::Array {
        name: name.to_string(),
        items: vec![Table {
            name: name.to_string(),
            entries: Vec::new(),
        }],
    });
    Ok(sections.len() - 1)
}

/// The table name of a header line (`[name]` / `[[name]]` given the
/// matching delimiters), or None when the line is not such a header. A line
/// that LOOKS like a header but carries a non-bare name is an error.
fn header_name(
    line: &str,
    open: &str,
    close: &str,
    line_no: usize,
) -> Result<Option<String>, String> {
    if !line.starts_with(open) {
        return Ok(None);
    }
    let Some(inner) = line
        .strip_prefix(open)
        .and_then(|rest| rest.strip_suffix(close))
    else {
        return Err(format!("line {line_no}: malformed table header `{line}`"));
    };
    let name = inner.trim();
    if !is_bare_key(name) {
        return Err(format!(
            "line {line_no}: unsupported table name `{name}` (bare names only; \
             dotted and quoted names are not in the pack TOML subset)"
        ));
    }
    Ok(Some(name.to_string()))
}

/// Bare-key shape: one or more ASCII letters, digits, `_`, or `-` (TOML's
/// bare-key grammar; camelCase keys such as `whenPaths` are bare keys).
fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Truncate `line` at the first `#` outside any string. Basic-string escapes
/// are honored so `\"#\"` never starts a comment.
fn strip_comment(line: &str) -> &str {
    let chars: Vec<char> = line.chars().collect();
    let mut in_basic = false;
    let mut in_literal = false;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if in_basic => i += 1, // skip the escaped character
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '#' if !in_basic && !in_literal => {
                return &line[..chars[..i].iter().map(|c| c.len_utf8()).sum::<usize>()];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

/// Net `[`-`]` depth outside strings; negative is an error (a `]` without
/// its `[`). Used only to decide whether a value continues on the next line.
fn bracket_depth(text: &str) -> Result<i32, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut in_basic = false;
    let mut in_literal = false;
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if in_basic => i += 1,
            '"' if !in_literal => in_basic = !in_basic,
            '\'' if !in_basic => in_literal = !in_literal,
            '[' if !in_basic && !in_literal => depth += 1,
            ']' if !in_basic && !in_literal => {
                depth -= 1;
                if depth < 0 {
                    return Err("unexpected `]` without a matching `[`".to_string());
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(depth)
}

/// Parse one complete value text (possibly multi-line for arrays).
fn parse_value(text: &str, line_no: usize) -> Result<Value, String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('"') {
        let (value, end) = parse_basic_string(rest, line_no)?;
        if !rest[end..].trim().is_empty() {
            return Err(format!(
                "line {line_no}: trailing text after the string value"
            ));
        }
        return Ok(Value::String(value));
    }
    if let Some(rest) = text.strip_prefix('\'') {
        // Literal string: no escapes, ends at the next `'`.
        let Some(end) = rest.find('\'') else {
            return Err(format!("line {line_no}: unterminated literal string"));
        };
        if !rest[end + 1..].trim().is_empty() {
            return Err(format!(
                "line {line_no}: trailing text after the string value"
            ));
        }
        return Ok(Value::String(rest[..end].to_string()));
    }
    if text.starts_with('[') {
        return parse_array(text, line_no);
    }
    if text == "true" {
        return Ok(Value::Boolean(true));
    }
    if text == "false" {
        return Ok(Value::Boolean(false));
    }
    if let Some(int) = parse_integer(text) {
        return Ok(Value::Integer(int));
    }
    Err(format!(
        "line {line_no}: unsupported value `{}` (the pack TOML subset supports \
         strings, integers, booleans, and arrays — no floats, datetimes, or \
         inline tables)",
        truncate(text, 40)
    ))
}

/// Parse a TOML integer: optional sign, digits, single underscores between
/// digits. None when the text is not exactly that shape.
fn parse_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    digits
        .replace('_', "")
        .parse::<i64>()
        .ok()
        .map(|n| if text.starts_with('-') { -n } else { n })
}

/// Parse the body of a basic string (after the opening quote), returning the
/// decoded value and the byte offset just past the closing quote. Supports
/// the standard short escapes and `\uXXXX`/`\UXXXXXXXX`.
fn parse_basic_string(rest: &str, line_no: usize) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut chars = rest.char_indices().peekable();
    while let Some((idx, c)) = chars.next() {
        match c {
            '"' => return Ok((out, idx + 1)),
            '\\' => {
                let Some((_, esc)) = chars.next() else {
                    return Err(format!("line {line_no}: unterminated escape in string"));
                };
                match esc {
                    'b' => out.push('\u{8}'),
                    't' => out.push('\t'),
                    'n' => out.push('\n'),
                    'f' => out.push('\u{c}'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'u' | 'U' => {
                        let width = if esc == 'u' { 4 } else { 8 };
                        let mut hex = String::new();
                        for _ in 0..width {
                            match chars.next() {
                                Some((_, h)) if h.is_ascii_hexdigit() => hex.push(h),
                                _ => {
                                    return Err(format!(
                                        "line {line_no}: malformed \\{esc} escape in string"
                                    ))
                                }
                            }
                        }
                        let code = u32::from_str_radix(&hex, 16).map_err(|_| {
                            format!("line {line_no}: malformed \\{esc} escape in string")
                        })?;
                        out.push(char::from_u32(code).ok_or_else(|| {
                            format!("line {line_no}: \\{esc}{hex} is not a valid code point")
                        })?);
                    }
                    other => {
                        return Err(format!(
                            "line {line_no}: unsupported escape `\\{other}` in string"
                        ))
                    }
                }
            }
            c => out.push(c),
        }
    }
    Err(format!("line {line_no}: unterminated string"))
}

/// Parse an array value: `[` elements `]` with optional trailing comma and
/// arbitrary whitespace/newlines between tokens.
fn parse_array(text: &str, line_no: usize) -> Result<Value, String> {
    debug_assert!(text.starts_with('['));
    let body = text[1..].trim_start();
    let mut items = Vec::new();
    let mut rest = body;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix(']') {
            if !after.trim().is_empty() {
                return Err(format!("line {line_no}: trailing text after the array"));
            }
            return Ok(Value::Array(items));
        }
        if rest.is_empty() {
            return Err(format!("line {line_no}: unterminated `[` in array value"));
        }
        // One element: strings consume through their closing quote; anything
        // else runs to the next top-level `,` or `]`.
        let (element, after) = if let Some(body) = rest.strip_prefix('"') {
            let (value, end) = parse_basic_string(body, line_no)?;
            (Value::String(value), &body[end..])
        } else if let Some(body) = rest.strip_prefix('\'') {
            let Some(end) = body.find('\'') else {
                return Err(format!("line {line_no}: unterminated literal string"));
            };
            (Value::String(body[..end].to_string()), &body[end + 1..])
        } else if rest.starts_with('[') {
            return Err(format!(
                "line {line_no}: nested arrays are not in the pack TOML subset"
            ));
        } else {
            let end = rest
                .find([',', ']'])
                .ok_or_else(|| format!("line {line_no}: unterminated `[` in array value"))?;
            let raw = rest[..end].trim();
            let value = parse_value(raw, line_no)?;
            (value, &rest[end..])
        };
        items.push(element);
        let after = after.trim_start();
        match after.strip_prefix(',') {
            Some(after) => rest = after,
            None => rest = after, // must now be `]` (handled at loop top)
        }
    }
}

/// First `max` chars of `text`, for echoing a rejected value in an error.
fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(source: &str) -> Document {
        parse(source).expect("test manifest parses")
    }

    #[test]
    fn pack_contract_toml_scalars_tables_and_arrays() {
        let d = doc(r#"
# a comment
[pack]
name = "fixture"        # trailing comment
schema = 3
strict = true

[[gate]]
name = 'literal'
command = "cargo test --workspace"
whenPaths = ["crates/", "apps/"]

[[gate]]
name = "second"
command = "true"
"#);
        let pack = d.single("pack").expect("[pack] table");
        assert_eq!(pack.get("name"), Some(&Value::String("fixture".into())));
        assert_eq!(pack.get("schema"), Some(&Value::Integer(3)));
        assert_eq!(pack.get("strict"), Some(&Value::Boolean(true)));
        let gates = d.array("gate");
        assert_eq!(gates.len(), 2);
        assert_eq!(
            gates[0].get("whenPaths"),
            Some(&Value::Array(vec![
                Value::String("crates/".into()),
                Value::String("apps/".into()),
            ]))
        );
        assert_eq!(gates[1].get("name"), Some(&Value::String("second".into())));
        assert!(d.array("prompt").is_empty());
    }

    #[test]
    fn pack_contract_toml_multi_line_array_with_trailing_comma() {
        let d = doc("[[checklist]]\nname = \"c\"\nitems = [\n  \"one\", # first\n  \"two\",\n]\n");
        let items = d.array("checklist")[0].get("items").expect("items");
        assert_eq!(
            items,
            &Value::Array(vec![
                Value::String("one".into()),
                Value::String("two".into())
            ])
        );
    }

    #[test]
    fn pack_contract_toml_string_escapes_and_hashes_inside_strings() {
        let d = doc("[pack]\nname = \"a \\\"quoted\\\" # not a comment\"\nschema = 2\n");
        assert_eq!(
            d.single("pack").unwrap().get("name"),
            Some(&Value::String("a \"quoted\" # not a comment".into()))
        );
    }

    #[test]
    fn pack_contract_toml_integer_underscores_and_signs() {
        let d = doc("[pack]\nname = \"x\"\nschema = 1_0\n");
        assert_eq!(
            d.single("pack").unwrap().get("schema"),
            Some(&Value::Integer(10))
        );
    }

    #[test]
    fn pack_contract_toml_rejects_unsupported_constructs() {
        for (source, needle) in [
            ("[pack]\nschema = 1.5\n", "unsupported value"),
            ("[pack]\nschema = 2026-01-01\n", "unsupported value"),
            ("[pack]\npoint.x = 1\n", "unsupported key"),
            ("[a.b]\n", "unsupported table name"),
            ("[pack]\nname = \"x\n", "unterminated string"),
            ("[pack]\nname = \"a\\q\"\n", "unsupported escape"),
            ("name = 1\n[pack]\n", "before any `[table]` header"),
            ("[pack]\nname = 1\nname = 2\n", "duplicate field `name`"),
            ("[pack]\n[pack]\n", "duplicate `[pack]` table"),
            ("[[gate]]\n[gate]\n", "duplicate `[gate]` table"),
            ("[gate]\n[[gate]]\n", "shadows the existing `[gate]` table"),
            ("[pack]\nname = [\"a\"\n", "unterminated `[`"),
            ("[pack]\njust words\n", "expected `key = value`"),
        ] {
            let err = parse(source).expect_err(&format!("{source:?} must fail"));
            assert!(
                err.contains(needle),
                "{source:?}: error {err:?} must name {needle:?}"
            );
        }
    }
}
