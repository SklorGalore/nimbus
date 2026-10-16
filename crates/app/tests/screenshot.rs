//! The GPU screenshot test (M8): render a fixed file headless and compare
//! the frame with a reference image, allowing small differences.
//!
//! macOS only: GPUs and font rasterizers differ by platform, so one
//! reference would not fit them all, and the owner's machine is a Mac.
//! It runs the debug binary Cargo built for the tests with `--headless`, a
//! scale of 1 and an empty config folder (sidebar hidden), so nothing about
//! the machine or the owner's settings changes the picture. Where there is
//! no usable GPU (some CI machines), it says so and passes.
//!
//! After an intended change to how text is drawn, make the reference again:
//! `NIMBUS_UPDATE_SCREENSHOT=1 cargo test -p nimbus --test screenshot`,
//! then look at `tests/screenshot/reference.bmp` before committing it.
#![cfg(target_os = "macos")]

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

#[expect(
    clippy::print_stderr,
    reason = "telling the test runner why it skipped"
)]
fn skip(why: &str) {
    eprintln!("screenshot test skipped: {why}");
}

#[test]
fn the_editor_draws_the_same_frame_as_the_reference() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/screenshot");
    let out_dir = std::env::temp_dir().join(format!("nimbus-shot-{}", std::process::id()));
    let config = out_dir.join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("state.toml"), "sidebar_visible = false\n").unwrap();
    let actual_path = out_dir.join("actual.bmp");

    let output = Command::new(env!("CARGO_BIN_EXE_nimbus"))
        .arg(here.join("sample.rs"))
        .args(["--headless", "640x360", "--cursors", "200"])
        .arg("--screenshot")
        .arg(&actual_path)
        .env("NIMBUS_CONFIG_DIR", &config)
        .env("NIMBUS_SCALE", "1")
        .env("RUST_LOG", "off")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        assert!(stderr.contains("no compatible GPU adapter"), "{stderr}");
        skip("no GPU adapter on this machine");
        return;
    }
    let actual_bytes = std::fs::read(&actual_path).unwrap();
    let reference_path: PathBuf = here.join("reference.bmp");
    if std::env::var_os("NIMBUS_UPDATE_SCREENSHOT").is_some() {
        std::fs::write(&reference_path, &actual_bytes).unwrap();
        skip("reference image written; look at it before committing");
        return;
    }
    let actual = read_bmp(&actual_bytes).unwrap();
    let reference = read_bmp(&std::fs::read(&reference_path).unwrap()).unwrap();
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
    let _ = std::fs::remove_dir_all(&out_dir);
}
