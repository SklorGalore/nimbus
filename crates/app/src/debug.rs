//! The debug overlay (`F12`, M8) and the timings behind it.
//!
//! Two numbers say whether the editor feels fast:
//!
//! - *Frame time*: how long building and submitting one frame takes, from
//!   the start of a redraw to the frame being handed to the screen. It
//!   bounds how quickly anything can change on screen.
//! - *Keystroke-to-present latency*: from the moment a key event arrives to
//!   the moment the frame that shows its effect is presented. That is what
//!   typing feels like (DESIGN.md §5), and it includes waiting for the next
//!   redraw as well as the frame itself.
//!
//! Both keep their last [`HISTORY`] values in a fixed ring, so recording
//! allocates nothing; the overlay shows the last value and the worst of
//! those. Rendering stays on demand: the overlay shows the numbers as of
//! the last frame and is redrawn whenever anything else is.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use nimbus_render::Gpu;

use crate::theme::Ui;
use crate::view::Layout;

/// How many recent values the overlay's "worst" covers.
pub const HISTORY: usize = 100;

/// The last [`HISTORY`] durations, in a fixed ring.
#[derive(Debug, Clone)]
pub struct Ring {
    values: [Duration; HISTORY],
    /// Where the next value goes.
    next: usize,
    /// How many values are stored, up to [`HISTORY`].
    len: usize,
}

impl Default for Ring {
    fn default() -> Self {
        Self {
            values: [Duration::ZERO; HISTORY],
            next: 0,
            len: 0,
        }
    }
}

impl Ring {
    /// Add a value, dropping the oldest once full.
    pub fn push(&mut self, d: Duration) {
        if let Some(slot) = self.values.get_mut(self.next) {
            *slot = d;
        }
        self.next = (self.next + 1) % HISTORY;
        self.len = (self.len + 1).min(HISTORY);
    }

    /// The newest value, if any.
    #[must_use]
    pub fn last(&self) -> Option<Duration> {
        (self.len > 0).then(|| self.values[(self.next + HISTORY - 1) % HISTORY])
    }

    /// The largest stored value, if any.
    #[must_use]
    pub fn worst(&self) -> Option<Duration> {
        self.values.iter().take(self.len).max().copied()
    }
}

/// Keystroke-to-present latency tracking (DESIGN.md §5).
#[derive(Debug, Default)]
pub struct Latency {
    /// When the oldest input not yet on screen arrived.
    pending: Option<Instant>,
    /// Recent latencies, for the overlay.
    pub recent: Ring,
    /// Every sample, kept only during `--bench-typing`.
    pub samples: Option<Vec<Duration>>,
}

impl Latency {
    /// An input arrived at `now`. Only the oldest input before a frame
    /// counts: several keys handled in one frame are one wait.
    pub fn input(&mut self, now: Instant) {
        self.pending.get_or_insert(now);
    }

    /// A frame was presented: the pending input, if any, is on screen.
    pub fn presented(&mut self) {
        if let Some(started) = self.pending.take() {
            let took = started.elapsed();
            self.recent.push(took);
            log::debug!("keystroke to present: {took:?}");
            if let Some(samples) = &mut self.samples {
                samples.push(took);
            }
        }
    }
}

/// What the overlay shows besides the timings: filled in by the app each
/// frame from state it already has.
#[derive(Debug, Default, Clone, Copy)]
pub struct Facts {
    /// Which buffer holds the document: `rope`, `bytes in memory` or
    /// `mapped file`.
    pub backend: &'static str,
    /// The document's length in bytes.
    pub len: usize,
    /// The syntax state's phase ([`nimbus_syntax::SyntaxState::phase_name`]).
    pub syntax: &'static str,
    /// The find bar's search is still running.
    pub searching: bool,
    /// Lines counted so far, as a percentage, while a huge file is counted.
    pub counting: Option<f64>,
    /// A save is running on a worker.
    pub saving: bool,
}

/// The overlay's state: whether it shows, the timings, and a reused line
/// buffer so drawing it allocates nothing.
#[derive(Debug, Default)]
pub struct Debug {
    /// `F12` toggles it.
    pub visible: bool,
    /// Keystroke-to-present latency.
    pub latency: Latency,
    /// Time from the start of a redraw to the frame being presented.
    pub frames: Ring,
    line: String,
}

/// Lines in the overlay panel.
const LINES: usize = 6;

/// A duration in milliseconds with one decimal, or `-` before any value.
fn ms(out: &mut String, d: Option<Duration>) {
    match d {
        Some(d) => {
            let _ = write!(out, "{:.1}", d.as_secs_f64() * 1000.0);
        }
        None => out.push('-'),
    }
}

impl Debug {
    /// The overlay, showing or not.
    #[must_use]
    pub fn new(visible: bool) -> Self {
        Self {
            visible,
            ..Self::default()
        }
    }

    /// Draw the overlay in the top right of the text area: frame time,
    /// latency, atlas use, backend and background work.
    pub fn draw(&mut self, gpu: &mut Gpu, ui: &Ui, layout: &Layout, facts: Facts) {
        if !self.visible {
            return;
        }
        let cell = layout.metrics.cell_width;
        let lh = layout.metrics.line_height;
        let pad = layout.margin;
        let cols = 34.0;
        let w = cols * cell + 2.0 * pad;
        #[expect(clippy::cast_precision_loss, reason = "a small line count")]
        let h = LINES as f32 * lh + 2.0 * pad;
        let x = (layout.width - w - pad).max(layout.left);
        let y = layout.top;
        gpu.begin_layer(None);
        gpu.push_rect([x, y], [w, h], ui.bar);
        let stats = gpu.stats();
        let mut row = 0.0;
        let mut put = |gpu: &mut Gpu, line: &str| {
            gpu.push_text_line([x + pad, y + pad + row * lh], line, ui.text);
            row += 1.0;
        };

        let line = &mut self.line;
        line.clear();
        line.push_str("frame    ");
        ms(line, self.frames.last());
        line.push_str(" ms  worst ");
        ms(line, self.frames.worst());
        put(gpu, line);

        line.clear();
        line.push_str("latency  ");
        ms(line, self.latency.recent.last());
        line.push_str(" ms  worst ");
        ms(line, self.latency.recent.worst());
        put(gpu, line);

        line.clear();
        let _ = write!(
            line,
            "atlas    {:.1}%  {} glyphs",
            stats.atlas * 100.0,
            stats.glyphs
        );
        put(gpu, line);

        line.clear();
        let _ = write!(line, "buffer   {} ", facts.backend);
        crate::view::write_size(line, facts.len);
        put(gpu, line);

        line.clear();
        let _ = write!(line, "syntax   {}", facts.syntax);
        put(gpu, line);

        line.clear();
        line.push_str("workers  ");
        let start = line.len();
        if facts.searching {
            line.push_str("search ");
        }
        if let Some(pct) = facts.counting {
            let _ = write!(line, "lines {pct:.0}% ");
        }
        if facts.saving {
            line.push_str("save ");
        }
        if line.len() == start {
            line.push_str("idle");
        }
        put(gpu, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_keeps_the_last_hundred() {
        let mut r = Ring::default();
        assert_eq!((r.last(), r.worst()), (None, None));
        for i in 1..=150u64 {
            r.push(Duration::from_millis(if i == 20 { 999 } else { i }));
        }
        assert_eq!(r.last(), Some(Duration::from_millis(150)));
        // 999 ms fell out with the 50 oldest values.
        assert_eq!(r.worst(), Some(Duration::from_millis(150)));
        r.push(Duration::from_millis(500));
        assert_eq!(r.worst(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn latency_counts_from_the_oldest_input_before_a_frame() {
        let mut l = Latency::default();
        l.presented();
        assert_eq!(l.recent.last(), None, "no input, nothing to record");
        let early = Instant::now()
            .checked_sub(Duration::from_millis(30))
            .unwrap();
        l.input(early);
        l.input(Instant::now());
        l.presented();
        assert!(l.recent.last().unwrap() >= Duration::from_millis(30));
    }
}
