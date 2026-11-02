"""What a stage usually costs, from the usage of its past runs. Display only: an estimate never
feeds a budget, a launch reserve or a launch decision.

A stage's samples are the usage records of finished (COMPLETED or FAILED: a failed run is paid for
too) runs with the same stage, harness and model, most recent first. Workers share one stage.
Both backends compute the same numbers (golden estimates-unit): values are picked from the sorted
samples by nearest rank, never interpolated or averaged, and totals add up in workflow order."""
import math
from typing import Any

from .usage import cost_kind

ESTIMATE_SAMPLES = 20  # the most recent runs a stage's estimate looks at
ESTIMATE_MIN_RUNS = 3  # fewer than this and the estimate says there is not enough history
WORKER_STAGE = "worker"
# A review round that finds something to fix runs these again.
FIX_ROUND_STAGES = ("implementer", "code_review", "security_review")


def stage_key(node_key: str) -> str:
    return WORKER_STAGE if node_key.startswith("worker_") else node_key


def _rank(values: list, fraction: float):
    """Nearest rank: the smallest value at or above `fraction` of the sorted values."""
    return values[math.ceil(fraction * len(values)) - 1]


def estimate(samples: list[dict[str, Any]], billing_mode: str) -> dict[str, Any]:
    """The typical (median) and high (90th percentile) cost of one run of a stage, from its
    samples (usage records, most recent first). A subscription harness is measured in tokens, since
    its runs cost nothing extra; anything else in USD, labelled with whether the figures are
    provider-billed, list-price estimates, or a mix. Partial usage records are skipped."""
    complete = [s for s in samples if s and not s.get("partial")]
    if billing_mode == "subscription":
        unit = "tokens"
        used = [s for s in complete if s.get("totalTokens") is not None][:ESTIMATE_SAMPLES]
        values = sorted(s["totalTokens"] for s in used)
        kinds: list[str] = []
    else:
        unit = "usd"
        used = [s for s in complete if s.get("costUsd") is not None][:ESTIMATE_SAMPLES]
        values = sorted(s["costUsd"] for s in used)
        kinds = sorted({cost_kind(s) for s in used})
    enough = len(values) >= ESTIMATE_MIN_RUNS
    return {
        "unit": unit, "runs": len(values), "enough": enough,
        "typical": _rank(values, .5) if enough else None,
        "high": _rank(values, .9) if enough else None,
        "costKind": (kinds[0] if len(kinds) == 1 else "mixed") if enough and kinds else None,
    }


def _add(values) -> Any:
    """Plain left-to-right addition. Not `sum()`: for floats it compensates rounding (Python 3.12+),
    which the Rust port does not, so the totals would differ in the last digit."""
    total = 0
    for value in values:
        total += value
    return total


def _totals(stages: list[dict[str, Any]], keys: list[str]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for unit in ("usd", "tokens"):
        chosen = [s["estimate"] for s in stages
                  if s["key"] in keys and s["estimate"]["enough"] and s["estimate"]["unit"] == unit]
        result[unit] = ({"typical": _add(e["typical"] for e in chosen), "high": _add(e["high"] for e in chosen)}
                        if chosen else None)
    return result


def route(stages: list[dict[str, Any]]) -> dict[str, Any]:
    """A route's estimate from its stages ({key, label, estimate}, in workflow order): one pass
    through every stage but the workers, plus what each worker and each fix round adds, since how
    many there will be is not known upfront. Stages without enough history are named in
    `missing` and left out of the totals."""
    keys = [s["key"] for s in stages]
    return {
        "stages": stages,
        "onePass": _totals(stages, [k for k in keys if k != WORKER_STAGE]),
        "perWorker": _totals(stages, [WORKER_STAGE]) if WORKER_STAGE in keys else None,
        "perFixRound": _totals(stages, [k for k in keys if k in FIX_ROUND_STAGES]),
        "missing": [s["label"] for s in stages if not s["estimate"]["enough"]],
    }
