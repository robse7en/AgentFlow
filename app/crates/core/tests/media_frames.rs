//! Exercise the real subprocess path using local fixture tools; no network protocols.
use agentflow_core::media;
use std::time::Duration;

#[cfg(windows)]
fn fixtures(dir: &std::path::Path, fail: bool) -> String {
    std::fs::write(dir.join("ffprobe.cmd"), "@echo off\r\necho 4.0\r\n").unwrap();
    let output = dir.join("frames.tmp/raw-001.png");
    let log = dir.join("argv.txt");
    let script = format!(
        "@echo off\r\necho %* >> \"{}\"\r\necho png > \"{}\"\r\necho [showinfo] pts_time:0.0 1>&2\r\nexit /b {}\r\n",
        log.display(),
        output.display(),
        i32::from(fail)
    );
    let exe = dir.join("ffmpeg.cmd");
    std::fs::write(&exe, script).unwrap();
    exe.to_string_lossy().into()
}
#[cfg(windows)]
#[tokio::test]
async fn test_ffmpeg_is_confined_to_local_files_and_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fixtures(dir.path(), false);
    let video = dir.path().join("video.mp4");
    std::fs::write(&video, b"fixture").unwrap();
    let dest = dir.path().join("frames");
    let names = media::extract_frames(&video, &dest, 1, Duration::from_secs(10), Some(&exe)).await.unwrap();
    assert_eq!(names, ["frame-01-t00m00.0s.png"]);
    assert!(dest.join(&names[0]).is_file());
    let args = std::fs::read_to_string(dir.path().join("argv.txt")).unwrap();
    assert!(args.contains("-protocol_whitelist file"));
    assert!(args.contains("file:"));
    assert!(args.contains("-frames:v 1"));
    assert!(args.contains("-nostdin"));
    assert!(args.contains("1280"));
    assert!(!dir.path().join("frames.tmp").exists());
}
#[cfg(windows)]
#[tokio::test]
async fn test_a_failed_extraction_leaves_no_partial_frames() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fixtures(dir.path(), true);
    let dest = dir.path().join("frames");
    let error = media::extract_frames(&dir.path().join("video.mp4"), &dest, 1, Duration::from_secs(10), Some(&exe))
        .await
        .unwrap_err();
    assert!(!error.tool_missing);
    assert!(!dest.exists());
    assert!(!dir.path().join("frames.tmp").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn rejects_a_junction_at_the_temporary_frame_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("other-data");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "keep").unwrap();
    let tmp = dir.path().join("frames.tmp");
    let argv = vec![
        "cmd.exe".into(),
        "/c".into(),
        "mklink".into(),
        "/J".into(),
        tmp.to_string_lossy().into(),
        target.to_string_lossy().into(),
    ];
    assert!(
        agentflow_core::process::run(&argv, None, &Default::default(), Duration::from_secs(10), None)
            .await
            .unwrap()
            .ok()
    );
    let error = media::extract_frames(
        &dir.path().join("video.mp4"),
        &dir.path().join("frames"),
        1,
        Duration::from_secs(1),
        Some("nonexistent-agentflow-fixture-ffmpeg"),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("Unsafe"));
    assert!(!error.tool_missing);
    assert_eq!(std::fs::read_to_string(target.join("keep.txt")).unwrap(), "keep");
}
#[tokio::test]
#[ignore = "requires locally installed ffmpeg and ffprobe; run explicitly with --ignored"]
async fn test_real_ffmpeg_extracts_timestamped_keyframes() {
    let exe = agentflow_core::process::resolve_executable("ffmpeg").expect("install ffmpeg before running this test");
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("synthetic.mp4");
    let argv = vec![
        exe.to_string_lossy().into(),
        "-nostdin".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "testsrc2=size=160x90:rate=10".into(),
        "-t".into(),
        "2".into(),
        "-c:v".into(),
        "mpeg4".into(),
        video.to_string_lossy().into(),
    ];
    assert!(
        agentflow_core::process::run(&argv, None, &Default::default(), Duration::from_secs(20), None)
            .await
            .unwrap()
            .ok()
    );
    let names = media::extract_frames(
        &video,
        &dir.path().join("frames"),
        4,
        Duration::from_secs(20),
        Some(&exe.to_string_lossy()),
    )
    .await
    .unwrap();
    assert!(!names.is_empty());
    assert!(names.len() <= 4);
    assert!(names.iter().all(|s| s.contains("-t")));
}
