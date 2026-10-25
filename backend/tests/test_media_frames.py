import shutil
import subprocess

import pytest

from agentflow.services import media_frames
from agentflow.services.process import CommandResult


def test_is_video_by_mime_or_extension():
    assert media_frames.is_video("repro.MOV", None)
    assert media_frames.is_video("clip", "video/webm")
    assert media_frames.is_video("spinner.gif", "image/gif")  # animated GIFs are treated as video
    assert not media_frames.is_video("shot.png", "image/png")


def test_missing_ffmpeg_is_a_retryable_tool_error(monkeypatch):
    monkeypatch.setattr(media_frames, "resolve_executable", lambda command: None)
    with pytest.raises(media_frames.ToolMissing, match="not found on PATH"):
        media_frames.resolve_tools(None)


@pytest.mark.asyncio
async def test_ffmpeg_is_confined_to_local_files_and_bounded(tmp_path, monkeypatch):
    video = tmp_path / "7-repro.mp4"
    video.write_bytes(b"x")
    calls = []

    async def fake_run(argv, *, timeout, **kwargs):
        calls.append((argv, timeout))
        if "ffprobe" in argv[0]:
            return CommandResult(argv, 0, "12.0\n", "")
        out_dir = tmp_path / "frames" / "7.tmp"
        for i in range(1, 6):
            (out_dir / f"raw-{i:03d}.png").write_bytes(b"png")
        stderr = "".join(f"[Parsed_showinfo_2] n:{i} pts_time:{i * 2.5} \n" for i in range(5))
        return CommandResult(argv, 0, "", stderr)

    monkeypatch.setattr(media_frames, "resolve_tools", lambda path: ("ffmpeg", "ffprobe"))
    monkeypatch.setattr(media_frames, "run", fake_run)
    names = await media_frames.extract_frames(video, tmp_path / "frames" / "7", max_frames=20, timeout=60)

    assert names[0] == "frame-01-t00m00.0s.png" and names[-1] == "frame-05-t00m10.0s.png"
    assert sorted(p.name for p in (tmp_path / "frames" / "7").iterdir()) == names
    assert not (tmp_path / "frames" / "7.tmp").exists()
    for argv, timeout in calls:
        whitelist = argv.index("-protocol_whitelist")
        assert argv[whitelist + 1] == "file"
        assert any(a.startswith("file:") for a in argv)
        assert 0 < timeout <= 60
    ffmpeg_argv = calls[1][0]
    assert ffmpeg_argv.index("-protocol_whitelist") < ffmpeg_argv.index("-i")
    assert ffmpeg_argv[ffmpeg_argv.index("-frames:v") + 1] == "20"


@pytest.mark.asyncio
async def test_a_failed_extraction_leaves_no_partial_frames(tmp_path, monkeypatch):
    video = tmp_path / "7-repro.mp4"
    video.write_bytes(b"x")

    async def fake_run(argv, **kwargs):
        if "ffprobe" in argv[0]:
            return CommandResult(argv, 0, "3.0\n", "")
        (tmp_path / "frames" / "7.tmp" / "raw-001.png").write_bytes(b"png")
        return CommandResult(argv, 1, "", "decode error")

    monkeypatch.setattr(media_frames, "resolve_tools", lambda path: ("ffmpeg", "ffprobe"))
    monkeypatch.setattr(media_frames, "run", fake_run)
    with pytest.raises(media_frames.FrameExtractionError):
        await media_frames.extract_frames(video, tmp_path / "frames" / "7", max_frames=5, timeout=60)
    assert not (tmp_path / "frames" / "7").exists() and not (tmp_path / "frames" / "7.tmp").exists()


@pytest.mark.asyncio
@pytest.mark.skipif(not (shutil.which("ffmpeg") and shutil.which("ffprobe")), reason="ffmpeg not installed")
async def test_real_ffmpeg_extracts_timestamped_keyframes(tmp_path):
    video = tmp_path / "clip.mp4"
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi",
                    "-i", "testsrc=duration=3:size=320x240:rate=10", str(video)], check=True)
    names = await media_frames.extract_frames(video, tmp_path / "frames", max_frames=6, timeout=60)
    assert 1 <= len(names) <= 6
    assert names[0].startswith("frame-01-t00m00.0s")
    assert media_frames.frames_done(tmp_path / "frames") == names
