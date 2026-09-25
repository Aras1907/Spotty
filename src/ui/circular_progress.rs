// ponhytail: minimal circular progress ring drawn with cairo.
// colours pulled from the active theme palette at draw time (libadwaita
// accent/success/error/window-fg named colours, with hardcoded fallbacks).
// Stroke width scales with widget size.
//
// Two modes:
//   - Determinate: smooth eased arc fill toward the target fraction.
//   - Indeterminate: rotating arc segment (spinner) while no % is available.
//
// While Running the arc never fills the whole circle (see RUNNING_MAX_FILL)
// — the leftover gap is the "still working" cue. Only Done / Failed stroke a
// complete ring, i.e. 100% completed.

use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;

use gtk::prelude::*;

/// A ring in `Running` state strokes at most this fraction of the track, so
/// the orb never looks complete before the operation actually finishes.
const RUNNING_MAX_FILL: f64 = 0.90;

/// Indeterminate spinner speed (revolutions per second ≈ 1.2 s / rev).
const SPIN_SPEED: f64 = 0.85;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RingState {
    Running,
    Done,
    Failed,
}

// Shared animation state — used by both the draw func and the tick callback.
// The indeterminate rotation phase deliberately does NOT live here: it comes
// from a shared monotonic clock (see `spinner_phase`), so a ring recreated on
// every Operations-popover rebuild (~8×/s while output streams) continues the
// same rotation instead of snapping back to its start.
struct Anim {
    displayed: Cell<f64>,
    last_frame: Cell<i64>, // µs from frame clock
}

/// Reusable, updatable circular progress ring.
pub struct Ring {
    area: gtk::DrawingArea,
    target: Rc<Cell<Option<f64>>>,
    state: Rc<Cell<RingState>>,
    anim: Rc<Anim>,
}

impl Ring {
    pub fn new(size: i32) -> Self {
        let target = Rc::new(Cell::new(None::<f64>));
        let state = Rc::new(Cell::new(RingState::Running));
        let anim = Rc::new(Anim {
            displayed: Cell::new(0.0),
            last_frame: Cell::new(0),
        });

        let area = gtk::DrawingArea::builder()
            .width_request(size)
            .height_request(size)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        // ── draw ──────────────────────────────────────────────────────
        let tc = target.clone();
        let sc = state.clone();
        let ac = anim.clone();
        area.set_draw_func(move |widget, cr, w, h| {
            let s = (w as f64).min(h as f64);
            let lw = (s / 5.5).clamp(3.0, 4.0);
            let r = (s - lw) / 2.0;
            let cx = s / 2.0;
            let cy = s / 2.0;

            cr.set_line_width(lw);
            cr.set_line_cap(gtk::cairo::LineCap::Round);

            let (bg_r, bg_g, bg_b, _) = themed(widget, "window_bg_color", (0.18, 0.18, 0.18, 1.0));
            let lum = 0.299 * bg_r + 0.587 * bg_g + 0.114 * bg_b;
            let is_dark = lum < 0.5;

            // Stroke the track ring on top.
            cr.arc(cx, cy, r, 0.0, 2.0 * PI);
            if is_dark {
                cr.set_source_rgba(1.0, 1.0, 1.0, 0.35);
            } else {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
            }
            let _ = cr.stroke();

            // Accent colour (shared by both modes).
            let state = sc.get();
            let (red, green, blue, _) = match state {
                RingState::Running => themed(widget, "accent_color", (0.21, 0.52, 0.89, 1.0)),
                RingState::Done => themed(widget, "success_color", (0.18, 0.76, 0.49, 1.0)),
                RingState::Failed => themed(widget, "error_color", (0.88, 0.10, 0.14, 1.0)),
            };
            cr.set_source_rgba(red, green, blue, 1.0);

            match tc.get() {
                None => {
                    // Indeterminate spinner: rotating arc segment.
                    let sweep = 0.28 * 2.0 * PI;
                    let start = -PI / 2.0 + spinner_phase() * 2.0 * PI;
                    let end = start + sweep;
                    cr.arc(cx, cy, r, start, end);
                    cr.set_source_rgba(red, green, blue, 0.9);
                    let _ = cr.stroke();
                }
                Some(_target_frac) => {
                    // Determinate arc: ease-driven `displayed` toward target.
                    // Still running → scale the visible fill down so a gap
                    // always remains; Done/Failed stroke the full circle.
                    let mut f = ac.displayed.get();
                    if state == RingState::Running {
                        f *= RUNNING_MAX_FILL;
                    }
                    if f > 0.001 {
                        let start = -PI / 2.0;
                        cr.arc(cx, cy, r, start, start + f * 2.0 * PI);
                        let _ = cr.stroke();
                    }
                }
            }
        });

        // ── tick callback: only fires while mapped/visible ─────────────
        let tgt = target.clone();
        let _st = state.clone();
        let an = anim.clone();
        area.add_tick_callback(move |w, clock| {
            let now = clock.frame_time(); // µs
            let last = an.last_frame.replace(now);
            let dt = if last == 0 {
                0.0
            } else {
                ((now - last) as f64 / 1_000_000.0).clamp(0.0, 0.1)
            };

            match tgt.get() {
                None => {
                    // Indeterminate: the phase comes from the shared clock,
                    // so just keep the frames coming.
                    w.queue_draw();
                }
                Some(target_f) => {
                    // Determinate: ease displayed toward target.
                    let d = an.displayed.get();
                    let delta = target_f - d;
                    if delta.abs() < 0.002 {
                        if d != target_f {
                            an.displayed.set(target_f);
                            w.queue_draw();
                        }
                    } else {
                        let k = 6.0;
                        an.displayed.set(d + delta * (1.0 - (-dt * k).exp()));
                        w.queue_draw();
                    }
                }
            }
            glib::ControlFlow::Continue
        });

        Self {
            area,
            target,
            state,
            anim,
        }
    }

    /// Set the target fraction (None = indeterminate spinner).
    /// The displayed value animates toward the target.
    pub fn set(&self, fraction: Option<f64>, state: RingState) {
        self.target.set(fraction);
        self.state.set(state);
        self.area.queue_draw();
    }

    /// Snap the displayed value instantly to the target (no easing).
    /// Used by `progress_ring()` so one-shot baked rings show immediately.
    pub fn snap(&self, fraction: Option<f64>, state: RingState) {
        self.target.set(fraction);
        self.state.set(state);
        self.anim.displayed.set(fraction.unwrap_or(0.0));
        self.area.queue_draw();
    }

    pub fn area(&self) -> &gtk::DrawingArea {
        &self.area
    }
}

/// One-shot convenience: builds a ring with a baked fraction (for the
/// Operations popover that is rebuilt every nudge rather than mutated).
pub fn progress_ring(size: i32, fraction: Option<f64>, state: RingState) -> gtk::DrawingArea {
    let r = Ring::new(size);
    r.snap(fraction, state);
    r.area().set_can_target(false);
    r.area().clone()
}

/// Indeterminate spinner rotation (0..1) read from a shared monotonic clock.
/// Deriving it from time instead of a per-widget accumulator keeps the
/// motion continuous across the popover's constant rebuilds — a fresh ring
/// picks up exactly where the previous one left off.
fn spinner_phase() -> f64 {
    let secs = glib::monotonic_time() as f64 / 1_000_000.0;
    (secs * SPIN_SPEED) % 1.0
}

// themed() fetches a named colour from the widget's style context, returning
// (r, g, b, a). Falls back to `fallback` if the colour isn't defined.
#[allow(deprecated)] // StyleContext::lookup_color is deprecated but is the
                      // only numeric path to theme named colours in GTK 4.
fn themed(w: &gtk::DrawingArea, name: &str, fallback: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    w.style_context()
        .lookup_color(name)
        .map(|c| (c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64))
        .unwrap_or(fallback)
}
