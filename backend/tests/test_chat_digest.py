from datetime import datetime, timedelta, timezone

import pytest

from agentflow.config import SlackConfig
from agentflow.services.chat.service import ChatService, next_digest_at
from test_chat import OPERATOR, FakeAdapter, eventually, quiet, settings, started
from test_workflow_safety import engine  # noqa: F401 - fixture

DM = f"D-{OPERATOR}"
NZ = timezone(timedelta(hours=13))


def test_the_next_digest_is_today_until_its_time_then_tomorrow():
    morning = datetime(2026, 9, 29, 8, 30, tzinfo=NZ)
    assert next_digest_at(morning, "09:00") == datetime(2026, 9, 29, 9, 0, tzinfo=NZ)
    assert next_digest_at(morning.replace(hour=9, minute=0), "09:00") == datetime(2026, 9, 30, 9, 0, tzinfo=NZ)
    assert next_digest_at(morning.replace(hour=23), "00:15") == datetime(2026, 9, 30, 0, 15, tzinfo=NZ)


def test_digest_time_must_be_a_time_of_day():
    for bad in ("9:00", "24:00", "09:60", "morning"):
        with pytest.raises(ValueError):
            SlackConfig.model_validate({"digestAt": bad})
    assert SlackConfig.model_validate({"digestAt": "07:45"}).digest_at == "07:45"
    assert SlackConfig().digest_at is None  # off by default


@pytest.mark.asyncio
async def test_the_digest_is_posted_when_its_time_comes(engine):
    adapter = FakeAdapter()
    service = ChatService(engine, adapter, settings(digestAt="09:00"), retry_delay=0.01)
    clock = iter([datetime(2026, 9, 29, 8, 59, 59, tzinfo=NZ)])
    after = datetime(2026, 9, 29, 9, 0, 1, tzinfo=NZ)
    service._now = lambda: next(clock, after)  # one second before, then just after 09:00
    service.digest_poll_seconds = 0.01
    service.start()
    await eventually(lambda: any(text.startswith("*AgentFlow digest*") for _, text, _ in adapter.sent))
    await quiet()
    digests = [(channel, thread) for channel, text, thread in adapter.sent if text.startswith("*AgentFlow digest*")]
    assert digests == [(DM, None)]  # once, as a top-level DM
    await service.stop()


@pytest.mark.asyncio
async def test_no_digest_when_nothing_is_active(engine):
    with engine.db.session() as session:
        engine._load_task(session, "TEST-1").status = "MERGED"
    adapter = FakeAdapter()
    service = await started(engine, adapter)
    await service._post_daily_digest()
    assert adapter.sent == []
    await service.stop()


@pytest.mark.asyncio
async def test_a_failed_digest_is_logged_not_raised(engine, caplog):
    adapter = FakeAdapter(fail_sends=1)
    service = await started(engine, adapter)
    await service._post_daily_digest()
    assert "Daily slack digest failed" in caplog.text
    await service.stop()
