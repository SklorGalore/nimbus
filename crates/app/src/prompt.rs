//! In-app prompts: questions such as "Save changes before closing?" and
//! messages such as "Could not save", drawn by Nimbus in the theme's
//! colours instead of by the operating system.
//!
//! A native dialog (`rfd`'s `MessageDialog`) blocks: the code that asks
//! waits on the line that shows it until the user answers. A prompt drawn
//! by the app cannot, because the app has to keep running its event loop to
//! draw the prompt and read the keys that answer it. So asking is split in
//! two: the code that needs an answer opens a [`Prompt`] and returns, and
//! the answer arrives later, as a key or a click, and carries on from what
//! the prompt remembers (its `then`, which the app defines). The app's side
//! of this is `App::ask` and `App::prompt_answered` in `app.rs`.
//!
//! While a prompt is open it is modal: keys and clicks go to it and nothing
//! else, so the tabs it is about cannot change under it.

use nimbus_render::Gpu;

use crate::theme::Ui;
use crate::view::Layout;

/// What a button means. The app maps it to what to do; the prompt only
/// needs it to choose the default button, the one Escape presses, and
/// where each button sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead ("Save", "Replace", "OK"). The first such button is the
    /// default: drawn in the accent colour and focused when the prompt
    /// opens, so Enter presses it.
    Yes,
    /// Go ahead without the safe step ("Don't Save"). Drawn apart, on the
    /// left, so it is not pressed by mistake for "Save".
    No,
    /// Leave things as they were. Escape presses it.
    Cancel,
}

/// One button: its label and what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    /// The text on it. Its first letter, typed, presses it.
    pub label: &'static str,
    /// What pressing it means.
    pub answer: Answer,
}

/// A key that means something to a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Enter or Space: press the focused button.
    Press,
    /// Escape: press Cancel (or the only button).
    Escape,
    /// Tab or Right: focus the next button.
    Next,
    /// Shift+Tab or Left: focus the previous button.
    Prev,
    /// A typed letter: press the button whose label starts with it.
    Letter(char),
}

/// An open prompt: what it says, its buttons, and what the app does with
/// the answer (`then`, any type the app likes).
#[derive(Debug, Clone)]
pub struct Prompt<T> {
    /// A short heading, such as "Unsaved changes".
    pub title: String,
    /// The question or message. `\n` starts a new paragraph; long lines
    /// wrap to the box.
    pub text: String,
    /// The buttons, in the order they are listed (see [`Prompt::new`]).
    pub buttons: Vec<Button>,
    /// The button Enter presses: an index into `buttons`.
    pub focused: usize,
    /// What the answer goes on to do.
    pub then: T,
}

impl<T> Prompt<T> {
    /// A prompt with `buttons`, the default ([`Answer::Yes`]) focused.
    /// Buttons are drawn in the order given, with [`Answer::No`] buttons
    /// moved to the left edge; list the default last, as macOS and GNOME
    /// do ("Cancel", "Save").
    pub fn new(title: &str, text: String, buttons: &[Button], then: T) -> Self {
        let focused = buttons
            .iter()
            .position(|b| b.answer == Answer::Yes)
            .unwrap_or(0);
        Self {
            title: title.to_owned(),
            text,
            buttons: buttons.to_vec(),
            focused,
            then,
        }
    }

    /// Handle `key`: move the focus, or return the answer of the button it
    /// pressed.
    pub fn key(&mut self, key: Key) -> Option<Answer> {
        let n = self.buttons.len();
        if n == 0 {
            return None;
        }
        let order = self.order();
        let at = order.iter().position(|&b| b == self.focused).unwrap_or(0);
        match key {
            Key::Press => self.buttons.get(self.focused).map(|b| b.answer),
            Key::Escape => self.escape(),
            Key::Next => {
                self.focused = order[(at + 1) % n];
                None
            }
            Key::Prev => {
                self.focused = order[(at + n - 1) % n];
                None
            }
            Key::Letter(c) => {
                let c = c.to_lowercase().next()?;
                self.buttons
                    .iter()
                    .find(|b| {
                        b.label.chars().next().and_then(|f| f.to_lowercase().next()) == Some(c)
                    })
                    .map(|b| b.answer)
            }
        }
    }

    /// What Escape means: Cancel if there is one; with a single button
    /// (a message with "OK"), that button.
    fn escape(&self) -> Option<Answer> {
        if self.buttons.iter().any(|b| b.answer == Answer::Cancel) {
            return Some(Answer::Cancel);
        }
        match self.buttons.as_slice() {
            [only] => Some(only.answer),
            _ => None,
        }
    }

    /// Button indices left to right as drawn: [`Answer::No`] buttons first.
    fn order(&self) -> Vec<usize> {
        let (left, right): (Vec<usize>, Vec<usize>) =
            (0..self.buttons.len()).partition(|&i| self.buttons[i].answer == Answer::No);
        left.into_iter().chain(right).collect()
    }

    /// The answer of the button under window position `(x, y)`, if any.
    #[must_use]
    pub fn click(&self, layout: &Layout, x: f32, y: f32) -> Option<Answer> {
        let g = self.geometry(layout);
        g.buttons
            .iter()
            .position(|&[bx, by, bw, bh]| x >= bx && x < bx + bw && y >= by && y < by + bh)
            .map(|i| self.buttons[i].answer)
    }

    /// Where the box and each button are drawn, for the accessibility
    /// tree: the box, then each button's rectangle in the order of
    /// [`Prompt::buttons`].
    #[must_use]
    pub fn rects(&self, layout: &Layout) -> ([f32; 4], Vec<[f32; 4]>) {
        let g = self.geometry(layout);
        (g.rect, g.buttons)
    }

    /// Where everything goes in a window of `layout`'s size.
    fn geometry(&self, layout: &Layout) -> Geometry {
        let cell = layout.metrics.cell_width;
        let lh = layout.metrics.line_height;
        let pad = 2.0 * layout.margin;
        let width = (56.0 * cell)
            .min(layout.width - 2.0 * layout.margin)
            .max(1.0)
            .round();
        let inner = (width - 2.0 * pad).max(cell);
        let lines = wrap(&self.text, inner, |s, room| layout.ui.fit(s, room));
        let button_h = (lh + layout.margin).round();
        let text_h = lh * f(lines.len().max(1));
        let height = (pad + lh + layout.margin + text_h + pad + button_h + pad).round();
        let x = ((layout.width - width) / 2.0).round();
        let y = ((layout.height - height) / 3.0)
            .round()
            .max(layout.tabs_h + layout.margin);
        // Buttons along the bottom: "No" buttons from the left edge, the
        // rest from the right edge, `gap` apart.
        let gap = cell;
        let button_y = y + height - pad - button_h;
        let widths: Vec<f32> = self
            .buttons
            .iter()
            .map(|b| {
                (layout.ui.width(b.label) + 4.0 * cell)
                    .max(10.0 * cell)
                    .round()
            })
            .collect();
        let mut buttons = vec![[0.0; 4]; self.buttons.len()];
        let mut left = x + pad;
        let mut right = x + width - pad;
        for i in (0..self.buttons.len()).rev() {
            if self.buttons[i].answer != Answer::No {
                right -= widths[i];
                buttons[i] = [right, button_y, widths[i], button_h];
                right -= gap;
            }
        }
        for (i, b) in self.buttons.iter().enumerate() {
            if b.answer == Answer::No {
                buttons[i] = [left, button_y, widths[i], button_h];
                left += widths[i] + gap;
            }
        }
        Geometry {
            rect: [x, y, width, height],
            pad,
            lines,
            buttons,
        }
    }

    /// Draw the prompt over everything: the window faded with the theme's
    /// background colour, then the box with its title, text and buttons.
    /// Uses only rectangles and interface text, which every painter draws.
    pub fn draw(&self, gpu: &mut Gpu, ui: &Ui, layout: &Layout) {
        let geo = self.geometry(layout);
        let [x, y, w, h] = geo.rect;
        let lh = layout.metrics.line_height;
        let px = layout.scale.round().max(1.0);
        // Fading the window says "answer this first" without a colour of
        // its own: the background at partial opacity works for any theme.
        gpu.begin_layer(None);
        let mut scrim = ui.background;
        scrim[3] = 0.6;
        gpu.push_rect([0.0, 0.0], [layout.width, layout.height], scrim);
        // A new layer, so the box covers the faded window's text too.
        gpu.begin_layer(None);
        gpu.push_rect(
            [x - px, y - px],
            [w + 2.0 * px, h + 2.0 * px],
            ui.picker_border,
        );
        gpu.push_rect([x, y], [w, h], ui.picker);
        let left = x + geo.pad;
        let mut ty = y + geo.pad;
        gpu.push_ui_text([left, ty], &self.title, ui.gutter);
        ty += lh + layout.margin;
        for &(start, end) in &geo.lines {
            if let Some(line) = self.text.get(start..end) {
                gpu.push_ui_text([left, ty], line, ui.text);
            }
            ty += lh;
        }
        let default = self.buttons.iter().position(|b| b.answer == Answer::Yes);
        for (i, (button, &[bx, by, bw, bh])) in self.buttons.iter().zip(&geo.buttons).enumerate() {
            if i == self.focused {
                // The keyboard focus: a ring with a gap, so it shows on
                // the accent-coloured default button too.
                let (ring, gap) = (3.0 * px, 2.0 * px);
                gpu.push_rect(
                    [bx - ring, by - ring],
                    [bw + 2.0 * ring, bh + 2.0 * ring],
                    ui.field_focus,
                );
                gpu.push_rect(
                    [bx - gap, by - gap],
                    [bw + 2.0 * gap, bh + 2.0 * gap],
                    ui.picker,
                );
            }
            let primary = Some(i) == default;
            let (fill, ink) = if primary {
                (ui.tab_accent, ui.background)
            } else {
                (ui.field, ui.text)
            };
            if !primary {
                gpu.push_rect(
                    [bx - px, by - px],
                    [bw + 2.0 * px, bh + 2.0 * px],
                    ui.picker_border,
                );
            }
            gpu.push_rect([bx, by], [bw, bh], fill);
            let label_w = layout.ui.width(button.label);
            let lx = (bx + (bw - label_w) / 2.0).round();
            let ly = (by + (bh - lh) / 2.0).round();
            gpu.push_ui_text([lx, ly], button.label, ink);
        }
    }
}

/// Where a prompt's parts go, in window pixels.
struct Geometry {
    /// The box: x, y, width, height.
    rect: [f32; 4],
    /// Space inside the box's edges.
    pad: f32,
    /// The text's lines, as byte ranges of [`Prompt::text`].
    lines: Vec<(usize, usize)>,
    /// Each button's x, y, width, height, in the order of
    /// [`Prompt::buttons`].
    buttons: Vec<[f32; 4]>,
}

/// Break `text` into lines no wider than `room`, at spaces where it can.
/// `fit(s, room)` is how many bytes of `s` fit in `room` (the interface
/// font's [`nimbus_render::UiFont::fit`]). Each `\n` starts a new line, so
/// `"a\n\nb"` keeps its blank line. Returns byte ranges into `text`.
fn wrap(text: &str, room: f32, fit: impl Fn(&str, f32) -> usize) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut base = 0;
    for paragraph in text.split('\n') {
        let mut start = 0;
        loop {
            let rest = &paragraph[start..];
            let n = fit(rest, room);
            if n >= rest.len() {
                lines.push((base + start, base + paragraph.len()));
                break;
            }
            // Break after the last space that fits; a word longer than
            // the line is cut where it stops fitting (at least one
            // character, so this always moves on).
            let cut = match rest[..n].rfind(' ') {
                Some(space) if space > 0 => space,
                _ => n.max(rest.chars().next().map_or(1, char::len_utf8)),
            };
            lines.push((base + start, base + start + cut));
            start += cut;
            start += paragraph[start..].len() - paragraph[start..].trim_start_matches(' ').len();
            if start >= paragraph.len() {
                break;
            }
        }
        base += paragraph.len() + 1;
    }
    lines
}

/// A count as pixels.
#[expect(clippy::cast_precision_loss, reason = "a handful of lines")]
fn f(n: usize) -> f32 {
    n as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISCARD: [Button; 3] = [
        Button {
            label: "Don't Save",
            answer: Answer::No,
        },
        Button {
            label: "Cancel",
            answer: Answer::Cancel,
        },
        Button {
            label: "Save",
            answer: Answer::Yes,
        },
    ];

    /// Ten pixels a character, as the grid font measures ASCII.
    fn grid(s: &str, room: f32) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test widths"
        )]
        let n = (room / 10.0) as usize;
        s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)
    }

    fn lines(text: &str, room: f32) -> Vec<&str> {
        wrap(text, room, grid)
            .into_iter()
            .map(|(a, b)| &text[a..b])
            .collect()
    }

    #[test]
    fn wrap_breaks_at_spaces_and_keeps_paragraphs() {
        assert_eq!(
            lines("Save changes to notes.md before closing?", 160.0),
            ["Save changes to", "notes.md before", "closing?"]
        );
        assert_eq!(lines("one\n\ntwo", 160.0), ["one", "", "two"]);
        assert_eq!(lines("", 160.0), [""]);
    }

    #[test]
    fn wrap_cuts_a_word_longer_than_the_line() {
        assert_eq!(lines("abcdefghij xy", 40.0), ["abcd", "efgh", "ij", "xy"]);
        // Too narrow for even one character still moves on.
        assert_eq!(lines("ab", 0.0), ["a", "b"]);
    }

    #[test]
    fn the_default_is_focused_and_enter_presses_it() {
        let mut p = Prompt::new("Unsaved changes", "Save?".into(), &DISCARD, ());
        assert_eq!(p.buttons[p.focused].label, "Save");
        assert_eq!(p.key(Key::Press), Some(Answer::Yes));
        assert_eq!(p.key(Key::Escape), Some(Answer::Cancel));
    }

    #[test]
    fn tab_moves_left_to_right_as_drawn() {
        let mut p = Prompt::new("Unsaved changes", "Save?".into(), &DISCARD, ());
        // Drawn: [Don't Save] ... [Cancel] [Save]. From Save, Next wraps
        // to Don't Save on the left.
        assert_eq!(p.key(Key::Next), None);
        assert_eq!(p.buttons[p.focused].label, "Don't Save");
        p.key(Key::Next);
        assert_eq!(p.buttons[p.focused].label, "Cancel");
        p.key(Key::Prev);
        p.key(Key::Prev);
        assert_eq!(p.buttons[p.focused].label, "Save");
        assert_eq!(p.key(Key::Press), Some(Answer::Yes));
    }

    #[test]
    fn a_letter_presses_its_button() {
        let mut p = Prompt::new("Unsaved changes", "Save?".into(), &DISCARD, ());
        assert_eq!(p.key(Key::Letter('d')), Some(Answer::No));
        assert_eq!(p.key(Key::Letter('S')), Some(Answer::Yes));
        assert_eq!(p.key(Key::Letter('c')), Some(Answer::Cancel));
        assert_eq!(p.key(Key::Letter('x')), None);
    }

    #[test]
    fn clicks_land_on_the_buttons_as_drawn() {
        let metrics = nimbus_render::Metrics {
            size_px: 14.0,
            line_height: 20.0,
            baseline: 15.0,
            cell_width: 10.0,
        };
        let layout = Layout::new(metrics, 1000, 600, 1.0, 10);
        let p = Prompt::new("Unsaved changes", "Save?".into(), &DISCARD, ());
        let g = p.geometry(&layout);
        let centre = |[x, y, w, h]: [f32; 4]| (x + w / 2.0, y + h / 2.0);
        // Don't Save sits at the left edge, Save at the right.
        assert!(g.buttons[0][0] < g.buttons[1][0]);
        assert!(g.buttons[1][0] < g.buttons[2][0]);
        let [bx, _, bw, _] = g.rect;
        let [sx, _, sw, _] = g.buttons[2];
        assert!((sx + sw - (bx + bw - g.pad)).abs() < 0.5);
        for (i, want) in [Answer::No, Answer::Cancel, Answer::Yes]
            .into_iter()
            .enumerate()
        {
            let (x, y) = centre(g.buttons[i]);
            assert_eq!(p.click(&layout, x, y), Some(want));
        }
        // The box's text, and outside it, are not buttons.
        assert_eq!(p.click(&layout, bx + g.pad, g.rect[1] + g.pad), None);
        assert_eq!(p.click(&layout, 1.0, 1.0), None);
    }

    #[test]
    fn escape_on_a_message_presses_ok() {
        let ok = [Button {
            label: "OK",
            answer: Answer::Yes,
        }];
        let mut p = Prompt::new("Could not save", "Disk full".into(), &ok, ());
        assert_eq!(p.key(Key::Escape), Some(Answer::Yes));
        let two = [
            Button {
                label: "Go",
                answer: Answer::Yes,
            },
            Button {
                label: "Stop",
                answer: Answer::No,
            },
        ];
        let mut p = Prompt::new("t", String::new(), &two, ());
        assert_eq!(p.key(Key::Escape), None);
    }
}
