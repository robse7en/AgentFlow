//! JSON text in the format Python's `json.dumps(value, indent=2)` writes, so files the Rust port
//! writes (flowctl's pending-complete.json, a first launch's config.json) match the Python ones.

use serde_json::{Number, Value};

/// Two-space indented, with every non-ASCII character escaped (`ensure_ascii=True`).
pub fn dumps(value: &Value) -> String {
    let mut out = String::new();
    write(value, Some(0), &mut out);
    ascii(&out)
}

/// Python's `json.dumps(value)` with no options: one line, `", "` and `": "` separators, ASCII
/// only. SQLAlchemy writes JSON columns this way.
pub fn dumps_compact(value: &Value) -> String {
    let mut out = String::new();
    write(value, None, &mut out);
    ascii(&out)
}

/// `indent` is the current depth when pretty-printing, None for one line.
fn write(value: &Value, indent: Option<usize>, out: &mut String) {
    let (open, close, items): (char, char, Vec<(Option<&String>, &Value)>) = match value {
        Value::Array(items) => ('[', ']', items.iter().map(|item| (None, item)).collect()),
        Value::Object(map) => ('{', '}', map.iter().map(|(key, item)| (Some(key), item)).collect()),
        Value::Number(number) => return out.push_str(&number_text(number)),
        scalar => return out.push_str(&scalar.to_string()),
    };
    out.push(open);
    // Python writes an empty container as `[]`/`{}` even when indenting.
    if !items.is_empty() {
        let inner = indent.map(|depth| depth + 1);
        for (i, (key, item)) in items.into_iter().enumerate() {
            match (i, inner) {
                (0, None) => {}
                (_, None) => out.push_str(", "),
                (i, Some(depth)) => {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    out.push_str(&"  ".repeat(depth));
                }
            }
            if let Some(key) = key {
                out.push_str(&Value::String(key.clone()).to_string());
                out.push_str(": ");
            }
            write(item, inner, out);
        }
        if let Some(depth) = indent {
            out.push('\n');
            out.push_str(&"  ".repeat(depth));
        }
    }
    out.push(close);
}

fn number_text(number: &Number) -> String {
    match number.as_f64() {
        Some(float) if number.is_f64() => float_repr(float),
        _ => number.to_string(),
    }
}

/// Python's `repr(float)`: the shortest round-tripping digits (as Rust's `{:e}` gives them), in
/// fixed notation unless the decimal exponent is below -4 or above 15, where it switches to
/// `1e-05` / `1.5e+16` form with at least two exponent digits.
pub fn float_repr(value: f64) -> String {
    if !value.is_finite() {
        // json.dumps writes these (allow_nan=True), although they are not valid JSON.
        return if value.is_nan() {
            "NaN"
        } else if value > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into();
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').expect("{:e} always has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exponent: i32 = exponent.parse().expect("{:e} exponent is an integer");
    let point = exponent + 1; // digits before the decimal point
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if !(-4 < point && point <= 16) {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() { String::new() } else { format!(".{rest}") };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{first}{fraction}e{exponent_sign}{:02}", exponent.abs());
    }
    let fixed = if point <= 0 {
        format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
    } else if point as usize >= digits.len() {
        format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{whole}.{fraction}")
    };
    format!("{sign}{fixed}")
}

/// Escape every non-ASCII character. They can only occur inside string literals, so escaping them
/// in the finished text is the same as escaping them while serializing.
fn ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{dumps, dumps_compact, float_repr};

    #[test]
    fn compact_matches_python_default_separators() {
        let value = json!({"status": "completed", "n": [1, 2.5, null, true], "e": {}, "s": "é\"\n"});
        assert_eq!(
            dumps_compact(&value),
            // Python: json.dumps({..., "s": "\u00e9\"\n"}) -> ... "s": "\u00e9\"\n" (escaped, ASCII only)
            "{\"status\": \"completed\", \"n\": [1, 2.5, null, true], \"e\": {}, \"s\": \"\\u00e9\\\"\\n\"}"
        );
    }

    #[test]
    fn matches_python_indent_and_ascii_escaping() {
        let value = json!({"b": 1, "a": ["é", "😀"], "empty": {}, "list": [], "nested": {"x": [{}]}});
        assert_eq!(
            dumps(&value),
            "{\n  \"b\": 1,\n  \"a\": [\n    \"\\u00e9\",\n    \"\\ud83d\\ude00\"\n  ],\n  \"empty\": {},\n  \"list\": [],\n  \
             \"nested\": {\n    \"x\": [\n      {}\n    ]\n  }\n}"
        );
    }

    #[test]
    fn floats_match_python_repr() {
        // Each pair is (value, repr(value) in CPython 3.13).
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (2.5, "2.5"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (0.000015, "1.5e-05"),
            (1.5e-7, "1.5e-07"),
            (123.456, "123.456"),
            (1e15, "1000000000000000.0"),
            (1.5e15, "1500000000000000.0"),
            (1e16, "1e+16"),
            (1.2345678901234568e20, "1.2345678901234568e+20"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (0.30000000000000004, "0.30000000000000004"),
        ];
        for (value, expected) in cases {
            assert_eq!(float_repr(*value), *expected, "{value:e}");
        }
        assert_eq!(dumps_compact(&json!([1e-5, 7, 1e16, -3])), "[1e-05, 7, 1e+16, -3]");
    }

    /// serde_json's default float parser can land one bit off for long digit strings; the
    /// workspace enables `float_roundtrip` so values read from data files survive unchanged.
    #[test]
    fn long_numbers_parse_to_the_nearest_double() {
        let parsed: serde_json::Value = serde_json::from_str("[123456789012345680000, 0.30000000000000004]").unwrap();
        assert_eq!(dumps_compact(&parsed), "[1.2345678901234568e+20, 0.30000000000000004]");
    }
}
