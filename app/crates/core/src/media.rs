//! Bounded keyframes from untrusted videos; fixed argv, no shell and local protocols only.
use std::{
    collections::HashMap,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct FrameError {
    pub message: String,
    pub tool_missing: bool,
}
impl FrameError {
    fn failed(message: impl Into<String>) -> Self {
        Self { message: message.into(), tool_missing: false }
    }
}
pub fn is_video(filename: &str, mime: Option<&str>) -> bool {
    mime.is_some_and(|s| s.starts_with("video/"))
        || Path::new(filename)
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| ["mp4", "mov", "webm", "mkv", "avi", "m4v", "gif"].contains(&s.to_lowercase().as_str()))
}
pub fn frames_done(dest: &Path) -> Vec<String> {
    let mut frames = std::fs::read_dir(dest)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|f| f.file_name().into_string().ok())
        .filter(|s| s.starts_with("frame-") && s.ends_with(".png"))
        .collect::<Vec<_>>();
    frames.sort();
    frames
}
fn tools(configured: Option<&str>) -> Result<(String, String), FrameError> {
    let ffmpeg = crate::process::resolve_executable(configured.unwrap_or("ffmpeg")).ok_or_else(|| FrameError {
        message: format!(
            "{} not found{}",
            configured.unwrap_or("ffmpeg"),
            if configured.is_none() { " on PATH" } else { "" }
        ),
        tool_missing: true,
    })?;
    let sibling = ffmpeg.with_file_name(ffmpeg.file_name().unwrap().to_string_lossy().replace("ffmpeg", "ffprobe"));
    let probe = if configured.is_some() && sibling.is_file() {
        Some(sibling)
    } else {
        crate::process::resolve_executable("ffprobe")
    }
    .ok_or_else(|| FrameError { message: "ffprobe not found next to ffmpeg or on PATH".into(), tool_missing: true })?;
    Ok((crate::paths::py_str(&ffmpeg), crate::paths::py_str(&probe)))
}
async fn run(argv: Vec<String>, deadline: Instant) -> Result<crate::process::CommandResult, FrameError> {
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| FrameError::failed("frame extraction timed out"))?;
    crate::process::run(&argv, None, &HashMap::new(), timeout, None)
        .await
        .map_err(|e| FrameError::failed(e.to_string()))
}
pub async fn extract_frames(
    video: &Path,
    dest: &Path,
    max: usize,
    timeout: Duration,
    configured: Option<&str>,
) -> Result<Vec<String>, FrameError> {
    let tmp = dest.with_file_name(format!("{}.tmp", dest.file_name().unwrap().to_string_lossy()));
    if [video, dest, tmp.as_path()].iter().any(|path| crate::paths::linked(path).is_some()) {
        return Err(FrameError::failed("Unsafe video or frame directory"));
    }
    let (ffmpeg, probe) = tools(configured)?;
    let deadline = Instant::now() + timeout;
    let source = format!("file:{}", crate::harness::as_posix(&crate::paths::resolve(video)));
    let duration = run(
        [
            probe,
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file".into(),
            "-show_entries".into(),
            "format=duration".into(),
            "-of".into(),
            "default=nw=1:nk=1".into(),
            source.clone(),
        ]
        .into(),
        deadline,
    )
    .await?;
    if !duration.ok() {
        return Err(FrameError::failed("ffprobe could not read the video"));
    }
    let duration = duration.stdout.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.0);
    let outcome = async {
        let mut times =
            extract(&ffmpeg, &source, &tmp, max, deadline, "select='eq(n,0)+gt(scene,0.25)'".into()).await?;
        if times.len() < 4.min(max)
            && let Some(seconds) = duration
        {
            times = extract(&ffmpeg, &source, &tmp, max, deadline, format!("fps={max}/{seconds:.3}")).await?;
        }
        if video.extension().is_some_and(|s| s.eq_ignore_ascii_case("gif")) && times.len() < 2 {
            return Ok(Vec::new());
        }
        let mut files = std::fs::read_dir(&tmp)
            .map_err(|_| FrameError::failed("Cannot read extracted frames"))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("raw-"))
            .collect::<Vec<_>>();
        files.sort();
        let mut names = Vec::new();
        for (index, (raw, time)) in files.iter().zip(times).enumerate() {
            let stamp =
                time.map_or(String::new(), |s| format!("-t{:02}m{:04.1}s", (s / 60.0).floor() as u64, s % 60.0));
            let name = format!("frame-{:02}{stamp}.png", index + 1);
            std::fs::rename(raw, tmp.join(&name)).map_err(|_| FrameError::failed("Cannot rename extracted frame"))?;
            names.push(name);
        }
        if dest.exists() {
            std::fs::remove_dir_all(dest).map_err(|_| FrameError::failed("Cannot replace extracted frames"))?;
        }
        std::fs::rename(&tmp, dest).map_err(|_| FrameError::failed("Cannot publish extracted frames"))?;
        Ok(names)
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    outcome
}
async fn extract(
    ffmpeg: &str,
    source: &str,
    tmp: &Path,
    max: usize,
    deadline: Instant,
    select: String,
) -> Result<Vec<Option<f64>>, FrameError> {
    let _ = std::fs::remove_dir_all(tmp);
    std::fs::create_dir_all(tmp).map_err(|_| FrameError::failed("Cannot create frame directory"))?;
    let output = run(
        vec![
            ffmpeg.into(),
            "-hide_banner".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            "info".into(),
            "-protocol_whitelist".into(),
            "file".into(),
            "-i".into(),
            source.into(),
            "-vf".into(),
            format!("{select},scale='min(1280,iw)':-2,showinfo"),
            "-fps_mode".into(),
            "vfr".into(),
            "-frames:v".into(),
            max.to_string(),
            crate::harness::as_posix(&tmp.join("raw-%03d.png")),
        ],
        deadline,
    )
    .await?;
    if !output.ok() {
        return Err(FrameError::failed(format!("ffmpeg exited with code {}", output.returncode)));
    }
    let count = std::fs::read_dir(tmp)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with("raw-") && e.path().extension().is_some_and(|s| s == "png")
        })
        .count()
        .min(max);
    let mut times = regex::Regex::new(r"pts_time:\s*([0-9.]+)")
        .unwrap()
        .captures_iter(&output.stderr)
        .take(count)
        .map(|c| c[1].parse().ok())
        .collect::<Vec<_>>();
    times.resize(count, None);
    Ok(times)
}
