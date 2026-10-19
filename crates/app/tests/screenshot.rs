//! The screenshot test (M8): render a fixed file headless and compare the
//! frame with a reference image, allowing small differences.
//!
//! It runs the debug binary Cargo built for the tests with `--headless`, a
//! scale of 1 and an empty config folder (sidebar hidden), so nothing about
//! the machine or the owner's settings changes the picture.
//!
//! The GPU test is macOS only: GPUs differ by platform, so one reference
//! would not fit them all, and the owner's machine is a Mac. Where there is
//! no usable GPU, Nimbus paints in compatibility mode instead, which the
//! second test covers anyway, so the first one skips.
//!
//! The compatibility-mode test (M23) runs everywhere. The CPU painter
//! blends exactly as the shaders do and swash rasterizes the same on every
//! system, so it must match the Mac's GPU reference too. That makes it the
//! one pixel check that runs on Linux and Windows CI.
//!
//! After an intended change to how text is drawn, make the reference again:
//! `NIMBUS_UPDATE_SCREENSHOT=1 cargo test -p nimbus --test screenshot`
//! on the Mac, then look at `tests/screenshot/reference.bmp` before
//! committing it.
#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// A pixel channel may differ by this much without counting.
const CHANNEL_TOLERANCE: u8 = 16;

/// At most this many pixels may differ by more. A clean run on the
/// owner's Mac differs by none; one changed letter in the sample differs
/// by about 48, so this catches a wrong glyph while allowing a GPU that
/// blends a few edge pixels differently.
const MAX_DIFFERING: usize = 20;

/// An image as rows of RGB, top row first.
struct Image {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

/// Read a 24-bit uncompressed BMP, as `--screenshot` writes it.
fn read_bmp(bytes: &[u8]) -> Option<Image> {
    let u32_at = |i: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?))
    };
    let i32_at = |i: usize| -> Option<i32> {
        Some(i32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?))
    };
    if bytes.get(..2)? != b"BM" || bytes.get(28..30)? != [24, 0] {
        return None;
    }
    let offset = usize::try_from(u32_at(10)?).ok()?;
    let width = usize::try_from(i32_at(18)?).ok()?;
    let height = usize::try_from(i32_at(22)?).ok()?;
    let row = (width * 3).div_ceil(4) * 4;
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in (0..height).rev() {
        let start = offset + y * row;
        for px in bytes.get(start..start + width * 3)?.as_chunks::<3>().0 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    Some(Image { width, height, rgb })
}

/// How many pixels differ by more than [`CHANNEL_TOLERANCE`] in a channel.
fn differing(a: &Image, b: &Image) -> usize {
    a.rgb
        .as_chunks::<3>()
        .0
        .iter()
        .zip(b.rgb.as_chunks::<3>().0)
        .filter(|(p, q)| {
            p.iter()
                .zip(*q)
                .any(|(x, y)| x.abs_diff(*y) > CHANNEL_TOLERANCE)
        })
        .count()
}

/// Render the sample headless with `extra` arguments; returns the BMP's
/// bytes and the folder it was written in, plus stderr.
fn render(extra: &[&str]) -> (Option<Vec<u8>>, PathBuf, String) {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/screenshot");
    let tag = extra.join("");
    let out_dir = std::env::temp_dir().join(format!("nimbus-shot-{}{tag}", std::process::id()));
    let config = out_dir.join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("state.toml"), "sidebar_visible = false\n").unwrap();
    let actual_path = out_dir.join("actual.bmp");

    let output = Command::new(env!("CARGO_BIN_EXE_nimbus"))
        .arg(here.join("sample.rs"))
        .args(["--headless", "640x360", "--cursors", "200"])
        .args(extra)
        .arg("--screenshot")
        .arg(&actual_path)
        .env("NIMBUS_CONFIG_DIR", &config)
        .env("NIMBUS_SCALE", "1")
        .env("RUST_LOG", "off")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let bytes = output
        .status
        .success()
        .then(|| std::fs::read(&actual_path).unwrap());
    (bytes, out_dir, stderr)
}

fn reference_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/screenshot/reference.bmp")
}

/// Compare `actual_bytes` (written in `out_dir`) with the reference.
fn assert_matches_reference(actual_bytes: &[u8], out_dir: &Path) {
    let actual_path = out_dir.join("actual.bmp");
    let actual = read_bmp(actual_bytes).unwrap();
    let reference = read_bmp(&std::fs::read(reference_path()).unwrap()).unwrap();
    assert_eq!(
        (actual.width, actual.height),
        (reference.width, reference.height),
        "frame size changed; see {}",
        actual_path.display()
    );
    let bad = differing(&actual, &reference);
    assert!(
        bad <= MAX_DIFFERING,
        "{bad} pixels differ from tests/screenshot/reference.bmp; the frame drawn is {}",
        actual_path.display()
    );
    let _ = std::fs::remove_dir_all(out_dir);
}

/// Tell the test runner why the test skipped.
#[cfg(target_os = "macos")]
fn skip(why: &str) {
    eprintln!("screenshot test skipped: {why}");
}

#[cfg(target_os = "macos")]
#[test]
fn the_editor_draws_the_same_frame_as_the_reference() {
    let (bytes, out_dir, stderr) = render(&[]);
    let actual_bytes = bytes.unwrap_or_else(|| panic!("{stderr}"));
    if stderr.contains("compatibility mode") {
        skip("no usable GPU on this machine; the compatibility test covers it");
        return;
    }
    if std::env::var_os("NIMBUS_UPDATE_SCREENSHOT").is_some() {
        std::fs::write(reference_path(), &actual_bytes).unwrap();
        skip("reference image written; look at it before committing");
        return;
    }
    assert_matches_reference(&actual_bytes, &out_dir);
}

#[test]
fn compatibility_mode_draws_the_same_frame_as_the_reference() {
    // While the GPU test is writing a new reference there is nothing to
    // compare with yet.
    if std::env::var_os("NIMBUS_UPDATE_SCREENSHOT").is_some() {
        return;
    }
    let (bytes, out_dir, stderr) = render(&["--compat"]);
    let actual_bytes = bytes.unwrap_or_else(|| panic!("{stderr}"));
    assert_matches_reference(&actual_bytes, &out_dir);
}
