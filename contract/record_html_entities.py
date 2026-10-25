"""Freeze Python's HTML5 decoding data for Rust's Jira renderer; no runtime Python needed."""
import html
import json
from pathlib import Path

root = Path(__file__).resolve().parent.parent
data = {'names': html.entities.html5, 'invalidReferences': html._invalid_charrefs,
        'invalidCodepoints': sorted(html._invalid_codepoints)}
target = root / 'app/crates/core/src/jira/html-entities.json'
target.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
