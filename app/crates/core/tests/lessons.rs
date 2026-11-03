//! Lesson text and guidance appends against Python's own output (contract/golden/lessons-unit,
//! recorded by contract/record_python_goldens.py), plus the Python unit tests ported by name.

use std::path::Path;

use agentflow_core::engine::lessons;
use serde_json::Value;

fn golden() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/lessons-unit/cases.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn every_lesson_case_matches_python() {
    let cases = golden();
    assert!(cases.len() >= 39, "golden lessons-unit looks truncated");
    let mut failures = Vec::new();
    for case in cases {
        match case["function"].as_str().unwrap() {
            "validate" => {
                let text = case["text"].as_str().unwrap();
                if lessons::normalize(text) != case["normalized"].as_str().unwrap() {
                    failures.push(format!("normalize {text:?}: rust {:?}", lessons::normalize(text)));
                }
                let expected = case["line"].as_str().map(|l| Ok(l.to_string()));
                let expected = expected.unwrap_or_else(|| Err(case["error"].as_str().unwrap().to_string()));
                if lessons::validate(text) != expected {
                    failures
                        .push(format!("validate {text:?}: python {expected:?}, rust {:?}", lessons::validate(text)));
                }
            }
            "append_lesson" => {
                let (existing, line) = (case["existing"].as_str(), case["line"].as_str().unwrap());
                let actual = lessons::append_lesson(existing, line);
                if actual != case["result"].as_str().unwrap() {
                    failures.push(format!("append {existing:?}: python {:?}, rust {actual:?}", case["result"]));
                }
            }
            other => panic!("unknown function {other}"),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn test_a_lesson_is_one_trimmed_line() {
    assert_eq!(lessons::normalize("  Run the build\n\n  # first\t please "), "Run the build # first please");
    assert!(lessons::validate(" \n\t ").unwrap_err().contains("needs text"));
    assert!(lessons::validate(&"x".repeat(301)).unwrap_err().contains("at most 300 characters"));
    assert_eq!(lessons::validate(&"é".repeat(300)).unwrap(), "é".repeat(300));
    assert!(lessons::validate("a\0b").unwrap_err().contains("NUL"));
}

#[test]
fn test_append_adds_a_heading_once_and_keeps_line_endings() {
    assert_eq!(lessons::append_lesson(None, "First"), "## Lessons from past tasks\n\n- First\n");
    assert_eq!(lessons::append_lesson(Some(""), "First"), "## Lessons from past tasks\n\n- First\n");
    let once = lessons::append_lesson(Some("# Implementer\n\nUse tabs."), "First");
    assert_eq!(once, "# Implementer\n\nUse tabs.\n\n## Lessons from past tasks\n\n- First\n");
    assert_eq!(lessons::append_lesson(Some(&once), "Second"), once.clone() + "- Second\n");
    let crlf = "# Implementer\r\n\r\nUse tabs.\r\n";
    assert_eq!(
        lessons::append_lesson(Some(crlf), "First"),
        format!("{crlf}\r\n## Lessons from past tasks\r\n\r\n- First\r\n")
    );
    let later = "## Lessons from past tasks\n\n- Old\n\n## Testing\n\nRun pytest.\n";
    assert_eq!(lessons::append_lesson(Some(later), "New"), format!("{later}\n## Lessons from past tasks\n\n- New\n"));
}
