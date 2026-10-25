//! HTML5 character references matching Python's `html.unescape` without a Python runtime.
use regex::{Captures, Regex};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entities {
    names: HashMap<String, String>,
    invalid_references: HashMap<String, String>,
    invalid_codepoints: HashSet<u32>,
}
static ENTITIES: LazyLock<Entities> =
    LazyLock::new(|| serde_json::from_str(include_str!("html-entities.json")).expect("generated HTML5 data"));
static REFERENCES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#(?:[0-9]{1,7}|[xX][0-9a-fA-F]+);?|[^\t\n\x0c <&#;]{1,32};?)").unwrap());
fn replace(entity: &str) -> String {
    if let Some(number) = entity.strip_prefix('#') {
        let number = number.trim_end_matches(';');
        let n = if number.starts_with(['x', 'X']) { u32::from_str_radix(&number[1..], 16) } else { number.parse() };
        let Ok(n) = n else { return "\u{fffd}".into() };
        if let Some(value) = ENTITIES.invalid_references.get(&n.to_string()) {
            return value.clone();
        }
        if (0xd800..=0xdfff).contains(&n) || n > 0x10ffff {
            return "\u{fffd}".into();
        }
        if ENTITIES.invalid_codepoints.contains(&n) {
            return String::new();
        }
        return char::from_u32(n).unwrap_or('\u{fffd}').to_string();
    }
    if let Some(value) = ENTITIES.names.get(entity) {
        return value.clone();
    }
    for (index, _) in entity.char_indices().rev() {
        if let Some(value) = ENTITIES.names.get(&entity[..index]) {
            return format!("{value}{}", &entity[index..]);
        }
    }
    format!("&{entity}")
}
pub(super) fn unescape(text: &str) -> String {
    let once = |s: &str| REFERENCES.replace_all(s, |c: &Captures<'_>| replace(&c[1])).into_owned();
    once(&once(text))
}
#[cfg(test)]
mod tests {
    use super::unescape;
    #[test]
    fn html5_entities_numeric_replacements_and_double_escape() {
        assert_eq!(unescape("&copy; &eacute; &NotEqualTilde;"), "© é ≂\u{338}");
        assert_eq!(unescape("&#128; &#xD800; &#0; &#11;"), "€ � � ");
        assert_eq!(unescape("&amp;copy; &unknown; &copy &notit;"), "© &unknown; © ¬it;");
    }
}
