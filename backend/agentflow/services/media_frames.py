"""Keyframes from Jira video attachments, so an Architect that can only view still images can
still follow a screen recording.

ffmpeg decodes untrusted, user-uploaded media here: fixed argv (no shell), local files only
(`-protocol_whitelist file`, so a crafted playlist can't make it fetch anything), a scrubbed
environment, and a hard timeout per video.
"""

from __future__ import annotations

import re
import shutil
import time
from pathlib import Path

from .process import ProcessError, resolve_executable, run

VIDEO_EXTENSIONS = frozenset({".mp4", ".mov", ".webm", ".mkv", ".avi", ".m4v", ".gif"})
# Scene detection often finds almost nothing in a screen recording of a mostly static page;
# below this many frames, sample evenly across the duration instead.
_MIN_SCENE_FRAMES = 4
_SCENE_THRESHOLD = 0.25
_MAX_WIDTH = 1280
_PTS_RE = re.compile(r"pts_time:\s*([0-9.]+)")


class FrameExtractionError(RuntimeError):
    pass


class ToolMissing(FrameExtractionError):
    """ffmpeg/ffprobe aren't installed; worth retrying once they are."""


def is_video(filename: str, mime_type: str | None) -> bool:
    """Animated GIFs count: image readers generally show only their first frame."""
    return (mime_type or "").startswith("video/") or Path(filename).suffix.lower() in VIDEO_EXTENSIONS


def resolve_tools(ffmpeg_path: str | None) -> tuple[str, str]:
    ffmpeg = resolve_executable(ffmpeg_path or "ffmpeg")
    if not ffmpeg:
        raise ToolMissing(f"{ffmpeg_path or 'ffmpeg'} not found" + ("" if ffmpeg_path else " on PATH"))
    if ffmpeg_path:
        sibling = Path(ffmpeg).with_name(Path(ffmpeg).name.replace("ffmpeg", "ffprobe"))
        ffprobe = str(sibling) if sibling.is_file() else resolve_executable("ffprobe")
    else:
        ffprobe = resolve_executable("ffprobe")
    if not ffprobe:
        raise ToolMissing("ffprobe not found next to ffmpeg or on PATH")
    return ffmpeg, ffprobe


def frames_done(dest: Path) -> list[str]:
    """Names of already-extracted frames; empty when this video still needs extracting."""
    return sorted(p.name for p in dest.glob("frame-*.png")) if dest.is_dir() else []


async def extract_frames(video: Path, dest: Path, *, max_frames: int, timeout: float,
                         ffmpeg_path: str | None = None) -> list[str]:
    """Write up to max_frames PNGs, in time order, to dest as frame-NN-tMMmSS.Ss.png.

    Written to a sibling temp dir first and renamed into place, so a crash or timeout never
    leaves a half-filled dest that later looks complete."""
    ffmpeg, ffprobe = resolve_tools(ffmpeg_path)
    deadline = time.monotonic() + timeout
    source = "file:" + video.resolve().as_posix()
    duration = await _duration(ffprobe, source, deadline)
    is_gif = video.suffix.lower() == ".gif"

    tmp = dest.with_name(dest.name + ".tmp")
    shutil.rmtree(tmp, ignore_errors=True)
    try:
        times = await _extract(ffmpeg, source, tmp, max_frames, deadline,
                               f"select='eq(n,0)+gt(scene,{_SCENE_THRESHOLD})'")
        if len(times) < min(_MIN_SCENE_FRAMES, max_frames) and duration:
            times = await _extract(ffmpeg, source, tmp, max_frames, deadline,
                                   f"fps={max_frames}/{duration:.3f}")
        if is_gif and len(times) < 2:
            return []  # a static GIF is just an image; the Architect can view it directly
        names = _rename(tmp, times)
        shutil.rmtree(dest, ignore_errors=True)
        tmp.rename(dest)
        return names
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _remaining(deadline: float) -> float:
    left = deadline - time.monotonic()
    if left <= 0:
        raise FrameExtractionError("frame extraction timed out")
    return left


async def _duration(ffprobe: str, source: str, deadline: float) -> float | None:
    try:
        result = await run([ffprobe, "-v", "error", "-protocol_whitelist", "file",
                            "-show_entries", "format=duration", "-of", "default=nw=1:nk=1", source],
                           timeout=_remaining(deadline))
    except ProcessError as exc:
        raise FrameExtractionError(str(exc)) from exc
    if not result.ok:
        raise FrameExtractionError("ffprobe could not read the video")
    try:
        value = float(result.stdout.strip())
    except ValueError:
        return None  # e.g. "N/A" for some streams; scene detection still runs
    return value if value > 0 else None


async def _extract(ffmpeg: str, source: str, out_dir: Path, max_frames: int, deadline: float,
                   select: str) -> list[float | None]:
    shutil.rmtree(out_dir, ignore_errors=True)
    out_dir.mkdir(parents=True)
    argv = [ffmpeg, "-hide_banner", "-nostdin", "-loglevel", "info", "-protocol_whitelist", "file",
            "-i", source, "-vf", f"{select},scale='min({_MAX_WIDTH},iw)':-2,showinfo",
            "-fps_mode", "vfr", "-frames:v", str(max_frames), (out_dir / "raw-%03d.png").as_posix()]
    try:
        result = await run(argv, timeout=_remaining(deadline))
    except ProcessError as exc:
        raise FrameExtractionError(str(exc)) from exc
    if not result.ok:
        raise FrameExtractionError(f"ffmpeg exited with code {result.returncode}")
    count = len(list(out_dir.glob("raw-*.png")))
    # showinfo logs one line per frame that reached the encoder, in output order.
    times: list[float | None] = [float(t) for t in _PTS_RE.findall(result.stderr)][:count]
    return times + [None] * (count - len(times))


def _rename(out_dir: Path, times: list[float | None]) -> list[str]:
    names = []
    for index, (raw, seconds) in enumerate(zip(sorted(out_dir.glob("raw-*.png")), times), start=1):
        stamp = "" if seconds is None else f"-t{int(seconds // 60):02d}m{seconds % 60:04.1f}s"
        name = f"frame-{index:02d}{stamp}.png"
        raw.rename(out_dir / name)
        names.append(name)
    return names
