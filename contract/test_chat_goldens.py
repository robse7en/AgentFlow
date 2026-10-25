"""Python remains the source for the frozen pure Slack behavior cases."""
import json

from record_chat_goldens import records
from support import ROOT


def test_python_slack_reference_has_not_drifted():
    recorded = json.loads((ROOT / 'contract/golden/chat.json').read_text(encoding='utf-8'))
    # JSON normalizes Python tuples to the same arrays consumed by Rust.
    assert recorded == json.loads(json.dumps(records()))
