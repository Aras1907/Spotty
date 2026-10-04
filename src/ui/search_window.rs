//! The main search window: entry + results list + preview + operations
//! popover + progress UI.
//!
//! # Window lifecycle (read before touching any show/hide code)
//!
//! Instances are short-lived. `app.rs` creates a fresh `SearchWindow` for
//! every show and drops the old one, because on GNOME/Wayland an unmapped
//! toplevel can never regain keyboard focus (verified empirically, see
//! /tmp/opencode/passthrough_test.py). Consequences:
//!
//! - `hide()` must fully unmap the window (`set_visible(false)`), it's the
//!   only "close" we have — a minimized/hidden window is dead weight.
//! - Every show goes through `present_and_focus` / `present_keyword_mode`.
//! - The [`shown`](SearchWindow::shown) flag is the single source of truth
//!   for visible-ness: GTK's `is_visible()` stays true during the fade-out,
//!   which would make the global toggle a no-op mid-fade.
//!
//! The 120ms fade exists purely for polish. `dismiss()` checks `shown` on
//! each timer tick so a re-show during the fade aborts the fade-out instead
//! of fighting it.
use crate::clipboard::ClipboardHistory;
use crate::config::Config;
use crate::index::Indexer;
use crate::preview::PreviewPane;
use crate::search::{self, Action, SearchResult};
use crate::ui::result_row::ResultRow;
use crate::i18n::gettext;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Each draggable row in the Operations popover gets a unique CSS class so the
// shared swipe provider can target it for real-time translateX during drag.
static SWIPE_ROW_CTR: AtomicU64 = AtomicU64::new(1);

thread_local! {
    // ONE display-wide CssProvider drives every swipe transform. Creating a new
    // provider per row (and never removing it) leaked providers on every popover
    // rebuild — during an install the popover rebuilds several times a second, so
    // style recomputation would slow to a crawl and hang the app. A single shared
    // provider whose rules are rebuilt only on actual swipe interaction avoids it.
    static SWIPE_PROVIDER: gtk::CssProvider = {
        let p = gtk::CssProvider::new();
        if let Some(d) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &d,
                &p,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 10,
            );
        }
        p
    };
    static SWIPE_RULES: RefCell<std::collections::HashMap<String, String>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Set the translateX (and optional transition) for the swipe class `cls` on the
/// shared provider. Rows at rest hold no rule, so the rule set stays tiny —
/// typically just the one row being dragged.
fn swipe_apply(cls: &str, px: f64, transition: &str) {
    SWIPE_RULES.with(|rules| {
        let mut m = rules.borrow_mut();
        let t = if transition.is_empty() {
            String::new()
        } else {
            format!("transition:{transition};")
        };
        m.insert(
            cls.to_string(),
            format!(".{cls}{{transform:translateX({px:.0}px);{t}}}"),
        );
        let css: String = m
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        SWIPE_PROVIDER.with(|p| p.load_from_string(&css));
    });
}

/// Drop the swipe rule for `cls` (row removed or settled back to centre), so the
/// shared rule set doesn't accumulate stale entries over a session.
fn swipe_clear(cls: &str) {
    SWIPE_RULES.with(|rules| {
        if rules.borrow_mut().remove(cls).is_some() {
            let css: String = rules
                .borrow()
                .values()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            SWIPE_PROVIDER.with(|p| p.load_from_string(&css));
        }
    });
}

/// Pixels of travel per unit of accumulated touchpad/wheel scroll — tuned so a
/// swipe moves the row at roughly the speed of the fingers.
const SWIPE_SCALE: f64 = 8.0;
/// How far a row must travel before a release/scroll commits the delete.
const SWIPE_COMMIT_PX: f64 = 140.0;
/// Past this the row has left the popover, so a drag commits without waiting
/// for release.
const SWIPE_OFFSCREEN_PX: f64 = 260.0;
/// How far a swiped-away row is parked. Must clear the popover's 360 px
/// width for ANY row width; the width watcher below refines it to the
/// measured width + 2 once the row has been allocated.
const SWIPE_PARK_PX: f64 = 420.0;
/// Final slide target used once a swipe commits.
const SWIPE_SETTLE_PX: f64 = 500.0;
/// A single large side-scroll delta counts as a fling and commits immediately.
const SWIPE_SCROLL_FLING_DX: f64 = 4.0;
/// Spring-back transition used when a swipe is released short of the threshold.
const SWIPE_SPRING: &str = "transform 300ms cubic-bezier(0.34,1.56,0.64,1)";

/// Whether the touchpad uses natural scroll (fingers move content in the same
/// direction). When OFF, GTK EventControllerScroll reports dx opposite to finger
/// movement, so we negate it to keep swipes feeling 1:1.
fn touchpad_natural_scroll() -> bool {
    std::process::Command::new("gsettings")
        .args([
            "get",
            "org.gnome.desktop.peripherals.touchpad",
            "natural-scroll",
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "true")
        .unwrap_or(true)
}

fn scroll_fling_offset(dx: f64) -> Option<f64> {
    if dx.abs() >= SWIPE_SCROLL_FLING_DX {
        Some(dx.signum() * SWIPE_COMMIT_PX.max(dx.abs() * SWIPE_SCALE))
    } else {
        None
    }
}

fn pending_row_tx(row_rest: f64, dismiss_dir: f64, offset: f64) -> f64 {
    if offset * dismiss_dir < 0.0 {
        let progress = (offset.abs() / SWIPE_COMMIT_PX).clamp(0.0, 1.0);
        row_rest * (1.0 - progress)
    } else {
        row_rest + offset
    }
}

/// Where the undo bar sits while a dismissed row is dragged back in.
/// The row comes back from the side it was dismissed to; the bar slides out the
/// opposite side, so the two are complementary and never overlap (a "sliding
/// door"): with `R = row_rest` the row spans `[R(1-p), R(1-p)+R]` and the bar
/// `[-Rp, -Rp+R]`, so the bar's trailing edge always meets the row's leading
/// edge. Forward drags (re-dismissing) keep following the finger.
fn pending_bar_tx(row_rest: f64, dismiss_dir: f64, offset: f64) -> f64 {
    if offset * dismiss_dir >= 0.0 {
        offset
    } else {
        let progress = (offset.abs() / SWIPE_COMMIT_PX).clamp(0.0, 1.0);
        -dismiss_dir * row_rest * progress
    }
}

pub struct SearchWindow {
    pub(crate) window: gtk::Window,
    /// The window-level key controller — kept so `destroy` can detach it
    /// (its closures hold clones of the window: without detaching, every
    /// replaced window would leak).
    window_kc: gtk::EventControllerKey,
    /// Handler ids detached again in `destroy`: they close the reference
    /// cycles (the entry's changed closure captures the entry itself plus
    /// list/results/preview; the window's is-active closure clones the
    /// window) that would otherwise leak one ~1 MB widget graph per show.
    entry_changed_h: std::cell::Cell<Option<glib::SignalHandlerId>>,
    is_active_h: std::cell::Cell<Option<glib::SignalHandlerId>>,
    entry: gtk::Entry,
    mode_chip: gtk::Box,
    mode_icon: gtk::Image,
    mode_label: gtk::Label,
    active_mode: Rc<RefCell<Option<crate::config::CommandKeyword>>>,
    list: gtk::ListBox,
    revealer: gtk::Revealer,
    preview_box: gtk::Box,
    preview: Rc<PreviewPane>,
    results: Rc<RefCell<Vec<SearchResult>>>,
    clipboard_mode: Rc<Cell<bool>>,
    undo_stack: Rc<RefCell<Vec<String>>>,
    redo_stack: Rc<RefCell<Vec<String>>>,
    // ── Progress mode (shared Rc closures so new() closures can also call them) ─
    progress_active: Rc<Cell<bool>>,
    cancel_progress_fn: Rc<dyn Fn()>,
    // Rebuilds Operations popover content in place if it is currently open.
    refresh_ops: Rc<dyn Fn()>,
    // True when the current trigger mode was entered via keybinding shortcut
    // (not by typing the trigger word). Backspace on an empty entry won't exit
    // a keybinding-originated mode; it will exit a typed-trigger-word mode.
    mode_from_keybinding: Rc<Cell<bool>>,
    // True while the window is fully shown. Toggle's source of truth: GTK's
    // is_visible() stays true during the fade-out, so app.rs would treat a
    // mid-fade window as visible and never re-show it — the fade-out must
    // complete and only then does `shown` flip false.
    shown: Rc<Cell<bool>>,
    suppress_changed: Rc<Cell<bool>>,
    ops_ring: Rc<crate::ui::circular_progress::Ring>,
    busy_stack: gtk::Stack,
    busy_label: gtk::Label,
    busy_revealer: gtk::Revealer,
    /// Settings gear next to the orb slot — revealed purely on hover.
    gear_revealer: gtk::Revealer,
    /// Inline process pill right of the orb, toggled by clicking the orb.
    ops_pill_revealer: gtk::Revealer,
    ops_pill_label: gtk::Label,
    ops_pill_open: Rc<Cell<bool>>,
    toast_gen: Rc<Cell<u64>>,
    /// Update badge in the orb slot. Nothing expands on its own — the
    /// options appear once the user types "update".
    update_badge: gtk::Button,
    pre_show_reset: Rc<dyn Fn()>,
}

impl SearchWindow {
    pub fn new(
        app: &adw::Application,
        config: Rc<RefCell<Config>>,
        indexer: Rc<Indexer>,
        clipboard: Rc<RefCell<ClipboardHistory>>,
    ) -> Self {
        let window = gtk::Window::builder()
            .application(app)
            .title(gettext("Spotty"))
            .default_width(750)
            .decorated(false)
            .resizable(false)
            .css_classes(["spotty-window"])
            .build();
        window.set_startup_id("");

        let outer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["spotty-outer"])
            .build();
        window.set_child(Some(&outer));

        // ── Search bar ──
        let entry = gtk::Entry::builder()
            .placeholder_text(gettext("Search"))
            .hexpand(true)
            .has_frame(false)
            .css_classes(["spotty-entry", "title-3"])
            .build();

        // Raycast-style mode chip: an icon + label shown before the entry when a
        // trigger mode is active (e.g. "PDF", "Files").
        let mode_icon = gtk::Image::builder()
            .icon_name("folder-symbolic")
            .pixel_size(16)
            .build();
        let mode_label = gtk::Label::builder().build();
        let mode_chip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .css_classes(["spotty-mode-chip"])
            .visible(false)
            .build();
        mode_chip.append(&mode_icon);
        mode_chip.append(&mode_label);

        let bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(16)
            .margin_end(12)
            .margin_top(10)
            .margin_bottom(10)
            .build();
        bar.append(&mode_chip);
        bar.append(&entry);

        // ── Busy indicator: orb + BT status label, wrapped in a
        //    Revealer (crossfade in/out) containing a Stack (crossfade
        //    orb ↔ label) so the orb transitions smoothly to a short
        //    status toast after an action completes, then fades away. ──
        let ops_ring = Rc::new(crate::ui::circular_progress::Ring::new(22));
        let busy_label = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let busy_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(250)
            .build();
        busy_stack.add_named(ops_ring.area(), Some("orb"));
        busy_stack.add_named(&busy_label, Some("status"));
        let busy_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(250)
            .reveal_child(false)
            .child(&busy_stack)
            .build();
        // A fully-closed GtkRevealer still counts as a visible child, so its
        // Box spacing kept the idle gear away from the bar's right edge.
        // Keep closed revealers out of the layout entirely: the notify hook
        // hides the widget once the transition has actually finished (and
        // never when an interrupted hide is followed by a re-show).
        busy_revealer.connect_child_revealed_notify(|r| {
            if !r.is_child_revealed() && !r.reveals_child() {
                r.set_visible(false);
            }
        });
        busy_revealer.set_visible(false);

        // Settings gear: sits LEFT of the orb slot and is purely
        // hover-driven — it slides in when the pointer is over the search
        // bar and out when it leaves, orb or no orb.
        let gear = gtk::Button::builder()
            .icon_name("emblem-system-symbolic")
            .css_classes(["flat", "circular", "spotty-gear"])
            .tooltip_text(gettext("Settings"))
            .build();
        let rev_gear = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .transition_duration(150)
            .reveal_child(false)
            .child(&gear)
            .build();
        bar.append(&rev_gear);

        // Inline process pill: clicking the orb expands the running
        // operation's title + live status to the ORB's left, so the orb
        // stays the rightmost element of the bar in every state — the pill
        // takes its width out of the expanding entry, not out of the orb's
        // spot. SlideLeft makes it emerge from the orb's side.
        let ops_pill_label = gtk::Label::builder()
            .css_classes(["spotty-ops-pill", "caption"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .single_line_mode(true)
            .xalign(0.0)
            .build();
        let ops_pill_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .transition_duration(150)
            .reveal_child(false)
            .child(&ops_pill_label)
            .build();
        // Same closed-revealer spacing rule as the orb above.
        ops_pill_revealer.connect_child_revealed_notify(|r| {
            if !r.is_child_revealed() && !r.reveals_child() {
                r.set_visible(false);
            }
        });
        ops_pill_revealer.set_visible(false);
        bar.append(&ops_pill_revealer);
        bar.append(&busy_revealer);

        let ops_pill_open = Rc::new(Cell::new(false));
        {
            let mc = gtk::EventControllerMotion::new();
            {
                let rg = rev_gear.clone();
                mc.connect_enter(move |_, _, _| rg.set_reveal_child(true));
            }
            {
                let rg = rev_gear.clone();
                mc.connect_leave(move |_| rg.set_reveal_child(false));
            }
            bar.add_controller(mc);
        }
        // Clicking the orb toggles the inline process pill.
        {
            let open = ops_pill_open.clone();
            let revealer = ops_pill_revealer.clone();
            let label = ops_pill_label.clone();
            let br = busy_revealer.clone();
            let st = busy_stack.clone();
            let area = ops_ring.area().clone();
            let click = gtk::GestureClick::new();
            {
                let area_c = area.clone();
                click.connect_pressed(move |_, _, _, _| {
                    open.set(!open.get());
                    sync_ops_pill(&open, &revealer, &label, &br, &st, &area_c);
                });
            }
            area.add_controller(click);
            area.set_cursor_from_name(Some("pointer"));
        }
        let aw = app.downgrade();
        gear.connect_clicked(move |_| {
            if let Some(a) = aw.upgrade() {
                crate::app::open_settings(&a);
            }
        });
        outer.append(&bar);

        // ── Results revealer ──
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(60)
            .reveal_child(false)
            .build();

        let results_inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        results_inner.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build();

        // Left: list inside scroll
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["spotty-list"])
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .propagate_natural_height(true)
            .min_content_height(0)
            .max_content_height(0)
            .css_classes(["spotty-results-scroll"])
            .child(&list)
            .build();
        body.append(&scroll);

        // Right: preview - shown only for previewable results
        let preview_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .visible(false)
            .vexpand(true)
            .valign(gtk::Align::Fill)
            .build();
        preview_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        let preview = Rc::new(PreviewPane::new());
        preview_box.append(preview.widget());
        body.append(&preview_box);

        results_inner.append(&body);

        // ── Footer bar: Operations (left) + Hints (right) ───────────────────
        // Each carries a native GNOME key indicator (gtk::ShortcutLabel) so the
        // shortcut to open it is shown right on the button. Occupies the same
        // narrow space the old pin/unpin hint label used.
        let footer_button = |icon: &str, accel: &str, tooltip: &str| {
            let inner = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(4)
                .build();
            inner.append(&gtk::Image::from_icon_name(icon));
            if !accel.is_empty() {
                inner.append(&gtk::ShortcutLabel::new(accel));
            }
            gtk::Button::builder()
                .child(&inner)
                .css_classes(["flat", "spotty-footer-btn"])
                .tooltip_text(tooltip)
                .can_focus(false)
                .build()
        };

        let ops_accel = {
            let s = config.borrow().operations_shortcut.clone();
            if s.is_empty() {
                String::new()
            } else {
                to_gtk_accel(&s)
            }
        };
        let hints_accel = {
            let s = config.borrow().hints_shortcut.clone();
            if s.is_empty() {
                String::new()
            } else {
                to_gtk_accel(&s)
            }
        };
        let ops_btn = footer_button("view-list-symbolic", &ops_accel, "Operations");
        let hints_btn = footer_button("dialog-question-symbolic", &hints_accel, "Hints");
        let footer_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .margin_start(8)
            .margin_end(8)
            .margin_top(2)
            .margin_bottom(4)
            .css_classes(["spotty-footer"])
            .build();
        let footer_spacer = gtk::Box::builder().hexpand(true).build();
        footer_row.append(&ops_btn);
        footer_row.append(&footer_spacer);
        footer_row.append(&hints_btn);
        results_inner.append(&footer_row);

        // ── Progress pane (hidden by default, shown when a command is running) ──
        let progress_pane = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(14)
            .margin_end(14)
            .margin_top(10)
            .margin_bottom(12)
            .visible(false)
            .build();

        let prog_header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .build();
        let progress_ring = std::rc::Rc::new(crate::ui::circular_progress::Ring::new(22));
        progress_ring.area().set_visible(false);
        let progress_title_lbl = gtk::Label::builder()
            .hexpand(true)
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let progress_status_lbl = gtk::Label::builder()
            .label(gettext("Running…"))
            .css_classes(["dim-label", "caption"])
            .valign(gtk::Align::Center)
            .build();
        prog_header.append(progress_ring.area());
        prog_header.append(&progress_title_lbl);
        prog_header.append(&progress_status_lbl);
        progress_pane.append(&prog_header);

        let prog_frame = gtk::Frame::new(None);
        let prog_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(140)
            .max_content_height(260)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        let progress_buf = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        let log_view = gtk::TextView::builder()
            .buffer(&progress_buf)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .build();
        prog_scroll.set_child(Some(&log_view));
        prog_frame.set_child(Some(&prog_scroll));
        progress_pane.append(&prog_frame);

        let prog_btn_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .build();
        let prog_cancel = gtk::Button::builder()
            .label(gettext("Cancel"))
            .css_classes(["destructive-action"])
            .build();
        let prog_done = gtk::Button::builder()
            .label(gettext("Done"))
            .visible(false)
            .css_classes(["suggested-action"])
            .build();
        prog_btn_row.append(&prog_cancel);
        prog_btn_row.append(&prog_done);
        progress_pane.append(&prog_btn_row);

        results_inner.append(&progress_pane);

        // Clipboard-only mode flag (set when opened via clipboard shortcut) and
        // the active trigger keyword (None = default universal search). Declared
        // here so the Hints popover can tailor its shortcut list to the mode.
        let clipboard_mode: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let active_mode: Rc<RefCell<Option<crate::config::CommandKeyword>>> =
            Rc::new(RefCell::new(None));
        // The current result list, declared here so the Hints popover can read
        // the selected result and tailor itself to what the user is on top of.
        let results: Rc<RefCell<Vec<SearchResult>>> = Rc::new(RefCell::new(Vec::new()));

        // True while either footer popover is open. An autohide popover grabs
        // input, which drops the toplevel's :active state — without this guard
        // the "hide on focus loss" handler would dismiss the whole window the
        // instant a popover opens.
        let popover_open: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let shown: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Operations popover: rebuilt from the live + past operations each time
        // it opens, so it always reflects current state.
        let ops_popover = gtk::Popover::builder()
            .autohide(false)
            .can_focus(false)
            .position(gtk::PositionType::Top)
            .build();
        ops_popover.set_parent(&ops_btn);
        {
            let f = popover_open.clone();
            let w = window.clone();
            let shown = shown.clone();
            ops_popover.connect_closed(move |_| {
                f.set(false);
                // A close cancels any in-flight swipe without its end event, so
                // release the refresh lock here instead of waiting it out.
                ops_gesture_end();
                // If the popover closed because the user clicked outside the
                // whole window, finish the job and hide the window too.
                if !w.is_active() {
                    dismiss(&w, &shown, true);
                }
            });
        }
        // ops_state: the Operations popover's persistent widget tree, shared with
        // the open/close paths below.
        let ops_state: Rc<RefCell<Option<OpsPopover>>> = Rc::new(RefCell::new(None));

        // build_ops_content: refresh the Operations popover. The tree is built
        // once and then updated in place; only a structural change (item added,
        // removed, or its state/dismissal flipped) rebuilds the rows. Rebuilding
        // on every refresh recreated the pending undo bars ~8x/sec while an
        // operation ran — replaying their slide-down, resetting the scroll, and
        // destroying whatever row a swipe gesture was attached to.
        // Does NOT popup/popdown — callers do that.
        let build_ops_content: Rc<dyn Fn()> = {
            let popover = ops_popover.clone();
            let entry = entry.clone();
            let state = ops_state.clone();
            Rc::new(move || {
                {
                    let mut slot = state.borrow_mut();
                    if slot.is_none() {
                        *slot = Some(OpsPopover::new());
                        let fresh = slot.as_ref().expect("tree just inserted");
                        popover.set_child(Some(&fresh.list));
                    }
                }
                let items = crate::operations::popover_items();
                let sig = ops_signature(&items);
                let mut slot = state.borrow_mut();
                let st = slot.as_mut().expect("ops popover tree built above");
                st.empty.set_visible(items.is_empty());
                st.scroll.set_visible(!items.is_empty());
                if st.sig == sig && st.handles.len() == items.len() {
                    // Nothing structural changed: refresh only the live parts, so
                    // no widget — and no in-flight gesture — is disturbed.
                    for (h, it) in st.handles.iter_mut().zip(items.iter()) {
                        ops_row_update(h, it);
                    }
                } else {
                    for h in st.handles.drain(..) {
                        if let Some(c) = h.cleanup {
                            c();
                        }
                    }
                    while let Some(child) = st.rows.first_child() {
                        st.rows.remove(&child);
                    }
                    // When natural scroll is OFF, GTK reports dx opposite to the
                    // finger direction (traditional: finger right → scroll left →
                    // dx negative). Negate so the row always follows the finger.
                    let scroll_sign: f64 = if touchpad_natural_scroll() { -1.0 } else { 1.0 };
                    for it in &items {
                        let w = match it.dismissed_dir {
                            Some(_) => build_ops_pending_bar(it),
                            None => build_ops_row(it, scroll_sign),
                        };
                        st.rows.append(&w.widget);
                        st.handles.push(w);
                    }
                    st.sig = sig;
                }
                drop(slot);
                // Restore focus to the search entry so shortcuts keep working.
                entry.grab_focus();
            })
        };

        // show_ops_popover: toggle — opens the popover (building content) or
        // closes it if already visible.
        let show_ops_popover: Rc<dyn Fn()> = {
            let popover = ops_popover.clone();
            let popover_open = popover_open.clone();
            let build = build_ops_content.clone();
            let state = ops_state.clone();
            Rc::new(move || {
                if popover.is_visible() {
                    popover.popdown();
                    popover_open.set(false);
                    return;
                }
                popover_open.set(true);
                // A fresh open re-renders from scratch so the list starts at the top; the
                // in-place path only serves refreshes while the popover is open.
                if let Some(st) = state.borrow_mut().as_mut() {
                    st.sig.clear();
                }
                build();
                popover.popup();
            })
        };

        // refresh_ops: if the popover is already open, rebuild its content
        // in place so live changes (music start/stop, undo) are reflected
        // immediately without the user having to close and reopen it.
        let refresh_ops: Rc<dyn Fn()> = {
            let popover = ops_popover.clone();
            let build = build_ops_content.clone();
            Rc::new(move || {
                if !popover.is_visible() {
                    return;
                }
                // Never rebuild the rows out from under a live swipe — that is
                // what made the undo bar flicker and the swipe-back impossible.
                if ops_gesture_active() {
                    ops_refresh_deferred();
                    return;
                }
                build();
            })
        };

        {
            let show = show_ops_popover.clone();
            ops_btn.connect_clicked(move |_| show());
        }

        // Hints popover: keyboard shortcuts (with native key indicators) plus
        // the pin/unpin shortcut moved here from the inline hint bar.
        let hints_popover = gtk::Popover::builder()
            .autohide(false)
            .can_focus(false)
            .position(gtk::PositionType::Top)
            .build();
        hints_popover.set_parent(&hints_btn);
        {
            let f = popover_open.clone();
            let w = window.clone();
            let shown = shown.clone();
            hints_popover.connect_closed(move |_| {
                f.set(false);
                if !w.is_active() {
                    dismiss(&w, &shown, true);
                }
            });
        }
        let show_hints_popover: Rc<dyn Fn()> = {
            let popover = hints_popover.clone();
            let config = config.clone();
            let popover_open = popover_open.clone();
            let active_mode = active_mode.clone();
            let res_store = results.clone();
            let entry = entry.clone();
            let list = list.clone();
            let clipboard_mode = clipboard_mode.clone();
            Rc::new(move || {
                // Pressing the shortcut/button again toggles the popover closed.
                if popover.is_visible() {
                    popover.popdown();
                    popover_open.set(false);
                    return;
                }
                popover_open.set(true);

                let content = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(6)
                    .width_request(320)
                    .build();
                let header = |text: &str| {
                    gtk::Label::builder()
                        .label(text)
                        .xalign(0.0)
                        .css_classes(["heading"])
                        .margin_bottom(2)
                        .build()
                };
                let hint_row = |label: &str, accel: &str| {
                    let row = gtk::Box::builder()
                        .orientation(gtk::Orientation::Horizontal)
                        .spacing(12)
                        .build();
                    row.append(
                        &gtk::Label::builder()
                            .label(label)
                            .xalign(0.0)
                            .hexpand(true)
                            .build(),
                    );
                    row.append(&gtk::ShortcutLabel::new(accel));
                    row
                };

                let mode_id = active_mode
                    .borrow()
                    .as_ref()
                    .map(|kw| kw.id.clone())
                    .unwrap_or_default();
                let has_results = !res_store.borrow().is_empty();

                // Default launch with nothing showing: list the keyword triggers
                // so the user sees what they can type to switch modes, then fall
                // through to the always-visible shortcut list below.
                if mode_id.is_empty() && !has_results {
                    content.append(&header(&gettext("Type a keyword")));
                    let cfg = config.borrow();
                    for kw in &cfg.command_keywords {
                        let row = gtk::Box::builder()
                            .orientation(gtk::Orientation::Horizontal)
                            .spacing(12)
                            .build();
                        let textbox = gtk::Box::builder()
                            .orientation(gtk::Orientation::Vertical)
                            .hexpand(true)
                            .build();
                        textbox.append(
                            &gtk::Label::builder()
                                .label(crate::search::capitalize(&kw.word))
                                .xalign(0.0)
                                .build(),
                        );
                        textbox.append(
                            &gtk::Label::builder()
                                .label(&kw.description)
                                .xalign(0.0)
                                .css_classes(["dim-label", "caption"])
                                .build(),
                        );
                        row.append(&textbox);
                        if !kw.shortcut.trim().is_empty() {
                            row.append(&gtk::ShortcutLabel::new(&to_gtk_accel(&kw.shortcut)));
                        }
                        content.append(&row);
                    }
                }

                // Build the shortcut list conditionally: only show each
                // shortcut when the currently selected result makes it
                // doable.
                let selected = list.selected_row().and_then(|row| {
                    res_store.borrow().get(row.index() as usize).cloned()
                });
                let mode_is_none = active_mode.borrow().is_none();
                let mut entries: Vec<(String, String)> = Vec::new();

                // Pin / Unpin — eligible if clip-mode + pinable action,
                // or universal_pin_eligible.
                if let Some(res) = &selected {
                    let pin_eligible = if clipboard_mode.get() {
                        pin_info_for(&res.action).is_some()
                    } else {
                        universal_pin_eligible(res)
                    };
                    if pin_eligible {
                        let s = config.borrow().clipboard_pin_shortcut.clone();
                        let accel = if s.is_empty() {
                            "<Control>p".to_string()
                        } else {
                            s
                        };
                        if !accel.is_empty() {
                            entries.push((gettext("Pin / Unpin selected"), accel));
                        }
                    }
                }

                // Uninstall / Kill — universal mode + selected App.
                if mode_is_none {
                    if let Some(res) = &selected {
                        if res.kind == crate::search::ResultKind::App {
                            if let Action::LaunchDesktopFile(path) = &res.action {
                                let uaccel = accel_or(
                                    &config,
                                    |c| c.uninstall_shortcut.as_str(),
                                    "<Control>u",
                                );
                                if !uaccel.is_empty() {
                                    entries.push((gettext("Uninstall app"), uaccel));
                                }
                                if crate::search::uninstall::is_app_running(path, &res.title) {
                                    let kaccel = accel_or(
                                        &config,
                                        |c| c.kill_shortcut.as_str(),
                                        "<Control>k",
                                    );
                                    if !kaccel.is_empty() {
                                        entries.push((gettext("Kill app"), kaccel));
                                    }
                                }
                            } else if matches!(res.action, Action::LaunchAppImage(_)) {
                                // Same shortcut as uninstall: it trashes the file.
                                let uaccel = accel_or(
                                    &config,
                                    |c| c.uninstall_shortcut.as_str(),
                                    "<Control>u",
                                );
                                if !uaccel.is_empty() {
                                    entries.push((gettext("Remove AppImage"), uaccel));
                                }
                            }
                        }
                    }
                }

                // Find mode contextual shortcuts: terminal (folder), delete +
                // open location (file/folder).
                let in_find = active_mode
                    .borrow()
                    .as_ref()
                    .is_some_and(|kw| kw.all_files);
                if in_find {
                    let query = entry.text().to_string();
                    let trimmed = query.trim();
                    let browsing_folder = trimmed.starts_with('/')
                        || trimmed.starts_with("~/")
                        || trimmed == "~";
                    let selected_target = selected.as_ref().and_then(|res| match &res.action {
                        Action::BrowseInto(p) | Action::OpenInFileManager(p) => {
                            Some((p.clone(), true))
                        }
                        Action::OpenPath(p) => Some((
                            p.clone(),
                            matches!(res.kind, crate::search::ResultKind::Folder),
                        )),
                        _ => None,
                    });
                    let selected_is_folder =
                        selected_target.as_ref().is_some_and(|(_, is_dir)| *is_dir);
                    if browsing_folder || selected_is_folder {
                        let taccel = accel_or(
                            &config,
                            |c| c.terminal_shortcut.as_str(),
                            "<Control><Shift>Return",
                        );
                        if !taccel.is_empty() {
                            entries.insert(0, (gettext("Open folder in terminal"), taccel));
                        }
                    }
                    if selected_target.is_some() {
                        let daccel = accel_or(
                            &config,
                            |c| c.delete_file_shortcut.as_str(),
                            "<Control>d",
                        );
                        if !daccel.is_empty() {
                            let label = if selected_is_folder {
                                gettext("Delete folder")
                            } else {
                                gettext("Delete file")
                            };
                            entries.push((label, daccel));
                        }
                    }
                    if selected_target.is_some() || browsing_folder {
                        let laccel = accel_or(
                            &config,
                            |c| c.open_location_shortcut.as_str(),
                            "<Control>Return",
                        );
                        if !laccel.is_empty() {
                            entries.push((gettext("Open location in file manager"), laccel));
                        }
                    }
                }

                if !entries.is_empty() {
                    content.append(&header(&gettext("Keyboard Shortcuts")));
                    for (label, accel) in &entries {
                        content.append(&hint_row(label, &to_gtk_accel(accel)));
                    }
                }
                popover.set_child(Some(&content));
                popover.popup();
                entry.grab_focus();
            })
        };
        {
            let show = show_hints_popover.clone();
            hints_btn.connect_clicked(move |_| show());
        }

        // Shared progress state
        let progress_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let mode_from_keybinding: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let child_pid: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        let source_holder: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

        // ── Cancel progress closure (used by Cancel button + Ctrl+C) ────────
        let cancel_progress: Rc<dyn Fn()> = {
            let body = body.clone();
            let pane = progress_pane.clone();
            let active = progress_active.clone();
            let pid = child_pid.clone();
            let holder = source_holder.clone();
            let entry = entry.clone();
            let spinner = progress_ring.clone();
            let prog_done = prog_done.clone();
            let prog_status = progress_status_lbl.clone();
            Rc::new(move || {
                // Nothing is running (e.g. a fresh run calling us to clear a
                // previous one that never existed): don't fabricate a
                // "Cancelled" state or schedule a stray collapse timer.
                if !active.get() && holder.borrow().is_none() && pid.get() == 0 {
                    return;
                }
                // Remove the glib channel source first
                if let Some(sid) = holder.borrow_mut().take() {
                    sid.remove();
                }
                // Kill child process
                let p = pid.get();
                if p > 0 {
                    let _ = std::process::Command::new("kill")
                        .arg(p.to_string())
                        .spawn();
                    pid.set(0);
                }
                // Restore UI
                spinner.set(None, crate::ui::circular_progress::RingState::Done);
                prog_status.set_text("Cancelled");
                prog_done.set_visible(true);
                active.set(false);
                entry.set_sensitive(true);
                // Collapse progress after a moment
                let body_d = body.clone();
                let pane_d = pane.clone();
                let entry_d = entry.clone();
                let done_d = prog_done.clone();
                glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                    body_d.set_visible(true);
                    pane_d.set_visible(false);
                    done_d.set_visible(false);
                    entry_d.grab_focus();
                });
            })
        };

        {
            let cp = cancel_progress.clone();
            prog_cancel.connect_clicked(move |_| cp());
        }
        {
            let cp = cancel_progress.clone();
            prog_done.connect_clicked(move |_| cp());
        }

        // ── start_progress_fn ─────────────────────────────────────────────────
        // Defined once here so both the row-activated / key-handler closures
        // (which can't call methods on `self`) and the public method can use it.
        let start_progress_fn: Rc<dyn Fn(String, Vec<String>)> = {
            let cancel_fn = cancel_progress.clone();
            let body = body.clone();
            let pane = progress_pane.clone();
            let buf = progress_buf.clone();
            let title_lbl = progress_title_lbl.clone();
            let status_lbl = progress_status_lbl.clone();
            let spinner = progress_ring.clone();
            let active = progress_active.clone();
            let pid = child_pid.clone();
            let holder = source_holder.clone();
            let entry = entry.clone();
            let revealer = revealer.clone();
            let prog_scroll_widget = prog_scroll.clone();
            Rc::new(move |title: String, mut args: Vec<String>| {
                cancel_fn(); // stop any previous run

                buf.set_text("");
                title_lbl.set_text(&title);
                status_lbl.set_text("Running…");
                spinner.set(None, crate::ui::circular_progress::RingState::Running);
                spinner.area().set_visible(true);

                body.set_visible(false);
                pane.set_visible(true);
                active.set(true);
                entry.set_sensitive(false);
                revealer.set_reveal_child(true);

                let program = args.remove(0);
                let child_res = std::process::Command::new(&program)
                    .args(&args)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();

                let mut child = match child_res {
                    Err(e) => {
                        let mut end = buf.end_iter();
                        buf.insert(&mut end, &format!("Error launching '{}': {}\n", program, e));
                        spinner.set(None, crate::ui::circular_progress::RingState::Failed);
                        spinner.area().set_visible(false);
                        status_lbl.set_text("Failed");
                        active.set(false);
                        pid.set(0);
                        body.set_visible(true);
                        pane.set_visible(false);
                        entry.set_sensitive(true);
                        return;
                    }
                    Ok(c) => c,
                };

                pid.set(child.id());

                let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
                let rx = std::rc::Rc::new(std::cell::RefCell::new(rx));
                // Set by the stderr thread once the child exits, read by the
                // idle handler when it sees the end-of-output marker (None).
                let exit_ok = std::sync::Arc::new(std::sync::Mutex::new(true));

                let tx_out = tx.clone();
                if let Some(stdout) = child.stdout.take() {
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(stdout).lines() {
                            if tx_out.send(Some(line.unwrap_or_default())).is_err() {
                                return;
                            }
                        }
                    });
                }
                let tx_err = tx;
                let exit_ok_thread = exit_ok.clone();
                if let Some(stderr) = child.stderr.take() {
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(stderr).lines() {
                            let _ = tx_err.send(Some(line.unwrap_or_default()));
                        }
                        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
                        *exit_ok_thread.lock().unwrap() = ok;
                        let _ = tx_err.send(None);
                    });
                }

                let buf_cb = buf.clone();
                let body_cb = body.clone();
                let pane_cb = pane.clone();
                let active_cb = active.clone();
                let pid_cb = pid.clone();
                let entry_cb = entry.clone();
                let spinner_cb = progress_ring.clone();
                let status_cb = status_lbl.clone();
                let scroll_cb = prog_scroll_widget.clone();
                let exit_ok_cb = exit_ok.clone();
                let title_cb = title.clone();
                let holder_cb = holder.clone();

                let sid = glib::idle_add_local(move || {
                    loop {
                        match rx.borrow().try_recv() {
                            Ok(Some(line)) => {
                                let mut end = buf_cb.end_iter();
                                buf_cb.insert(&mut end, &format!("{}\n", line));
                                let adj = scroll_cb.vadjustment();
                                adj.set_value(adj.upper() - adj.page_size());
                                // Parse percentage from output lines for the progress ring.
                                if let Some(pct) = crate::operations::parse_percent(&line) {
                                    if pct > 0.0 {
                                        spinner_cb.set(
                                            Some(pct),
                                            crate::ui::circular_progress::RingState::Running,
                                        );
                                    }
                                }
                            }
                            Ok(None) => {
                                let ok = *exit_ok_cb.lock().unwrap();
                                let final_state = if ok {
                                    crate::ui::circular_progress::RingState::Done
                                } else {
                                    crate::ui::circular_progress::RingState::Failed
                                };
                                spinner_cb.set(None, final_state);
                                spinner_cb.area().set_visible(false);
                                status_cb.set_text(if ok { "Done ✓" } else { "Failed" });
                                active_cb.set(false);
                                pid_cb.set(0);
                                entry_cb.set_sensitive(true);
                                // This source is about to be auto-removed by
                                // returning Break; forget its id so a later
                                // cancel can't remove a recycled source id.
                                holder_cb.borrow_mut().take();
                                // Remember the command and log it in Operations
                                // history; notify if the window is hidden.
                                crate::search::run::record(&title_cb);
                                crate::operations::record_command(&title_cb, ok);
                                if crate::app::is_search_window_hidden() {
                                    let verb = if ok { "finished" } else { "failed" };
                                    crate::app::send_desktop_notification(
                                        "Spotty",
                                        &format!("Command \"{}\" {}", title_cb, verb),
                                    );
                                }
                                // Bluetooth actions change device state: refresh
                                // the device list once the run finishes.
                                if title_cb.starts_with("Bluetooth:") {
                                    crate::search::bluetooth::invalidate_cache();
                                    entry_cb.emit_by_name::<()>("changed", &[]);
                                }
                                let body_d = body_cb.clone();
                                let pane_d = pane_cb.clone();
                                let entry_d = entry_cb.clone();
                                glib::timeout_add_local_once(Duration::from_secs(2), move || {
                                    body_d.set_visible(true);
                                    pane_d.set_visible(false);
                                    entry_d.grab_focus();
                                });
                                return glib::ControlFlow::Break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                return glib::ControlFlow::Continue;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                // Senders gone without an explicit end marker:
                                // tear down so progress state can't get stuck
                                // (which would block the window from hiding).
                                spinner_cb.set(None, crate::ui::circular_progress::RingState::Failed);
                                spinner_cb.area().set_visible(false);
                                active_cb.set(false);
                                pid_cb.set(0);
                                entry_cb.set_sensitive(true);
                                holder_cb.borrow_mut().take();
                                let body_d = body_cb.clone();
                                let pane_d = pane_cb.clone();
                                glib::timeout_add_local_once(Duration::from_secs(2), move || {
                                    body_d.set_visible(true);
                                    pane_d.set_visible(false);
                                });
                                return glib::ControlFlow::Break;
                            }
                        }
                    }
                });
                *holder.borrow_mut() = Some(sid);
            })
        };

        revealer.set_child(Some(&results_inner));
        // ── Update badge ───────────────────────────────────────────────
        // A quiet indicator in the orb slot (idle only — it never replaces
        // a running orb). Nothing expands on its own: clicking it types
        // "update", and the options appear with the search results.
        let update_badge = gtk::Button::builder()
            .icon_name("software-update-available-symbolic")
            .css_classes(["flat", "circular", "spotty-update-badge"])
            .visible(false)
            .build();
        bar.append(&update_badge);
        {
            let entry_c = entry.clone();
            update_badge.connect_clicked(move |_| {
                entry_c.set_text("update ");
                entry_c.set_position(entry_c.text().chars().count() as i32);
            });
        }
        outer.append(&revealer);

        // ── Undo bar: shown briefly after deleting a clipboard entry, like
        // Nautilus's "Moved to Trash" toast with an Undo button. Ctrl+Z within
        // the next 2 seconds also undoes the deletion.
        let undo_label = gtk::Label::builder()
            .label(gettext("Item deleted"))
            .hexpand(true)
            .halign(gtk::Align::Start)
            .css_classes(["caption"])
            .build();
        let undo_button = gtk::Button::builder()
            .label(gettext("Undo"))
            .css_classes(["flat"])
            .build();
        let undo_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(14)
            .margin_end(14)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        undo_box.append(&undo_label);
        undo_box.append(&undo_button);
        let undo_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(150)
            .reveal_child(false)
            .child(&undo_box)
            .build();
        outer.append(&undo_revealer);

        // Set when a clipboard entry was just deleted; Ctrl+Z restores it as
        // long as this remains the user's last action (no time limit — only
        // cleared once the user types something else or the undo is used).
        let last_action_was_delete: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // Bumped on every deletion so a stale 2s toast-hide timeout doesn't
        // hide the bar for a more recent deletion.
        let undo_generation: Rc<Cell<u64>> = Rc::new(Cell::new(0));

        // Track unpinned clipboard items for undo (5-second window)
        let last_action_was_unpin: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let unpin_generation: Rc<Cell<u64>> = Rc::new(Cell::new(0));

        // Create a separate undo bar for unpins
        let unpin_undo_label = gtk::Label::builder()
            .label(gettext("Item unpinned — Press Ctrl+Z to undo"))
            .hexpand(true)
            .halign(gtk::Align::Start)
            .css_classes(["caption"])
            .build();
        let unpin_undo_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .transition_duration(150)
            .reveal_child(false)
            .child(&unpin_undo_label)
            .build();
        outer.append(&unpin_undo_revealer);

        // Returns true if a deletion was actually undone.
        let do_undo: Rc<dyn Fn() -> bool> = {
            let clip = clipboard.clone();
            let entry_d = entry.clone();
            let undo_revealer_d = undo_revealer.clone();
            let last_action_d = last_action_was_delete.clone();
            Rc::new(move || {
                let restored = clip.borrow_mut().undo_remove();
                if restored {
                    last_action_d.set(false);
                    undo_revealer_d.set_reveal_child(false);
                    entry_d.emit_by_name::<()>("changed", &[]);
                }
                restored
            })
        };
        {
            let do_undo = do_undo.clone();
            undo_button.connect_clicked(move |_| {
                do_undo();
            });
        }

        // Track how much of the entry text was "typed by user" vs "auto-completed ghost".
        // The ghost is the SELECTED portion at the end; when typing more, GTK replaces it.
        let typed_len: Rc<Cell<usize>> = Rc::new(Cell::new(0));
        // Track previous text length to detect backspace (length shrinks)
        let prev_text_len: Rc<Cell<usize>> = Rc::new(Cell::new(0));
        // Suppress connect_changed re-entry while we're updating text programmatically
        let suppress_changed: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // After backspace, skip applying ghost for one search cycle
        let skip_ghost: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // While skip_ghost is active, remembers the query length at the time
        // it was first observed, so later "changed" events fired by async
        // search refreshes (same query, no new typing) keep suppressing the
        // ghost instead of re-arming it.
        let skip_ghost_anchor: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let ghost_active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // Text undo/redo stacks for Ctrl+Z / Ctrl+Shift+Z.
        let undo_stack: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let redo_stack: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let prev_typed_for_undo: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let is_undo_redo: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // True while the handler is rebuilding the result list (remove_all + append + select).
        // Prevents the row_selected(None) signal from clearing the preview mid-rebuild.
        let rebuilding: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // Last query rendered (for preserving selection + scroll on content-scan refreshes).
        let last_rendered_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

        // ── start_bt_scan_ui ────────────────────────────────────────────────
        // Clears the entry, starts the Bluetooth scan session, and kicks off
        // the animated placeholder ("Searching for Bluetooth device...").
        let start_bt_scan_ui: Rc<dyn Fn()> = {
            let entry = entry.clone();
            let typed_len = typed_len.clone();
            let suppress = suppress_changed.clone();
            let skip_ghost = skip_ghost.clone();
            let ghost_active = ghost_active.clone();
            Rc::new(move || {
                crate::search::bluetooth::start_scan();
                skip_ghost.set(true);
                ghost_active.set(false);
                typed_len.set(0);
                suppress.set(true);
                entry.set_text("");
                suppress.set(false);
                entry.grab_focus();
                // Animated placeholder while scanning.
                let saved = entry.placeholder_text().map(|g| g.to_string());
                let step = Rc::new(std::cell::Cell::new(0u32));
                let entry2 = entry.clone();
                let step2 = step.clone();
                glib::timeout_add_local(Duration::from_millis(400), move || {
                    if !crate::search::bluetooth::is_scanning() {
                        entry2.set_placeholder_text(saved.as_deref());
                        return glib::ControlFlow::Break;
                    }
                    let s = step2.get();
                    let dots = match s % 4 {
                        0 => ".",
                        1 => "..",
                        2 => "...",
                        _ => "",
                    };
                    let text = gettext("Searching for Bluetooth device{dots}").replace("{dots}", &dots);
                    entry2.set_placeholder_text(Some(text.as_str()));
                    step2.set(s + 1);
                    glib::ControlFlow::Continue
                });
            })
        };

        // ── pre_show_reset ──────────────────────────────────────────────────
        // Canonical reset of all mutable widgets before every map.  Ensures
        // the window maps at the same size each time so the compositor places
        // it at a consistent position.
        let pre_show_reset: Rc<dyn Fn()> = {
            let list = list.clone();
            let results = results.clone();
            let preview_box = preview_box.clone();
            let preview = preview.clone();
            let revealer = revealer.clone();
            let undo_revealer = undo_revealer.clone();
            let unpin_undo_revealer = unpin_undo_revealer.clone();
            let scroll = scroll.clone();
            let progress_active = progress_active.clone();
            let undo_stack = undo_stack.clone();
            let redo_stack = redo_stack.clone();
            Rc::new(move || {
                // Clear results list + data.
                while let Some(c) = list.first_child() {
                    list.remove(&c);
                }
                *results.borrow_mut() = vec![];
                // Hide preview + undo bars.
                preview_box.set_visible(false);
                preview.clear();
                undo_revealer.set_reveal_child(false);
                unpin_undo_revealer.set_reveal_child(false);
                // Normalize progress pane.
                progress_active.set(false);
                // Clear undo/redo stacks.
                undo_stack.borrow_mut().clear();
                redo_stack.borrow_mut().clear();
                // Collapse the results area and reset its sizing so the window
                // maps at its compact size (bar only) every show — no blank
                // reserved space, and a consistent compositor placement.
                revealer.set_reveal_child(false);
                scroll.set_min_content_height(0);
                scroll.set_max_content_height(0);
            })
        };

        // ── Typing handler ──
        // Debounce state: trailing 120 ms.
        let search_dispatch_now: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let search_debounce_id: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        // Filled with the changed handler's id below; `destroy` detaches it
        // to break the entry's self-cycle.
        #[allow(unused_assignments)] // the block below always fills it
        let mut entry_changed_h: Option<glib::SignalHandlerId> = None;
        {
            let list = list.clone();
            let results = results.clone();
            let config = config.clone();
            let indexer = indexer.clone();
            let clipboard = clipboard.clone();
            let preview = preview.clone();
            let revealer = revealer.clone();
            let preview_box = preview_box.clone();
            let typed_len = typed_len.clone();
            let suppress = suppress_changed.clone();
            let entry_h = entry.clone();
            let prev_text_len = prev_text_len.clone();
            let skip_ghost_typing = skip_ghost.clone();
            let skip_ghost_anchor_typing = skip_ghost_anchor.clone();
            let ghost_active_typing = ghost_active.clone();
            let clipboard_mode_cb = clipboard_mode.clone();
            let active_mode_cb = active_mode.clone();
            let undo_stack_cb = undo_stack.clone();
            let redo_stack_cb = redo_stack.clone();
            let prev_typed_cb = prev_typed_for_undo.clone();
            let is_undo_redo_cb = is_undo_redo.clone();
            let undo_revealer_cb = undo_revealer.clone();
            let last_action_was_delete_cb = last_action_was_delete.clone();
            let undo_generation_cb = undo_generation.clone();
            let scroll_cb_results = scroll.clone();
            let search_dispatch_now_cb = search_dispatch_now.clone();
            let search_debounce_cb = search_debounce_id.clone();
            let rebuild_flag = rebuilding.clone();
            let last_q = last_rendered_query.clone();
            let ops_ring_c = ops_ring.clone();

            entry_changed_h = Some(entry.connect_changed(move |e| {
                if suppress.get() {
                    return;
                }
                let full = e.text().to_string();
                // The orb's colour follows the text it sits next to.
                ops_ring_c.set_tone(if full.is_empty() {
                    crate::ui::circular_progress::Tone::Accent
                } else {
                    crate::ui::circular_progress::Tone::Foreground
                });
                let typed_now = visible_typed_len(e, typed_len.get());
                // Text undo/redo bookkeeping (word-step checkpoints).
                if is_undo_redo_cb.get() {
                    is_undo_redo_cb.set(false);
                } else {
                    let prev = prev_typed_cb.borrow().clone();
                    let current_typed = full.chars().take(typed_now).collect::<String>();
                    if prev != current_typed {
                        // Real user typing invalidates the "undo last deletion" action.
                        last_action_was_delete_cb.set(false);
                        let prev_wc = word_count(&prev);
                        let full_wc = word_count(&current_typed);
                        let prev_last = prev.chars().last();
                        let full_last = current_typed.chars().last();
                        let class_changed = char_class(prev_last) != char_class(full_last);
                        let big_edit = char_count(&prev).abs_diff(char_count(&current_typed)) > 1;
                        let boundary = prev_wc != full_wc
                            || prev.is_empty()
                            || current_typed.is_empty()
                            || class_changed
                            || big_edit;
                        if boundary {
                            let mut us = undo_stack_cb.borrow_mut();
                            if us.last().map(|s| s != &prev).unwrap_or(true) {
                                us.push(prev);
                                if us.len() > 200 {
                                    us.remove(0);
                                }
                            }
                        }
                        // Any user-typed change invalidates redo chain.
                        redo_stack_cb.borrow_mut().clear();
                    }
                }
                *prev_typed_cb.borrow_mut() = full.chars().take(typed_now).collect();
                // Detect backspace / character deletion: if text length shrank,
                // skip applying a new ghost on the next selection event so the
                // user can keep deleting.
                prev_text_len.set(char_count(&full));
                // Keep the logical typed portion separate from any selected tail
                // ghost text so backspace/search operate on what the user really typed.
                typed_len.set(typed_now);
                ghost_active_typing.set(false);

                // ── Compute query + mode before debounce so we can check pending. ──
                let active_kw = active_mode_cb.borrow().clone();
                let mut q_owned = full.chars().take(typed_now).collect::<String>();
                if active_kw.as_ref().is_some_and(|kw| kw.all_files) && q_owned.trim().is_empty() {
                    let visible = full.trim();
                    if visible.starts_with('/') || visible.starts_with("~/") || visible == "~" {
                        q_owned = visible.to_string();
                    }
                }
                let q = q_owned.trim();
                let clip_mode = clipboard_mode_cb.get();
                let mode_word: Option<String> = active_kw.as_ref().map(|kw| kw.word.clone());
                let is_dict = active_kw.as_ref().is_some_and(|kw| kw.id == "dictionary");
                let jkey = crate::search::jobs::key(mode_word.as_deref(), q);

                // ── Debounce: trailing 120 ms. No search runs while typing. ──
                let pending_ready = !clip_mode && crate::search::jobs::has_pending(&jkey);
                if search_dispatch_now_cb.get() {
                    // Re-entry from the debounce timeout: run the search.
                    search_dispatch_now_cb.set(false);
                } else if !pending_ready {
                    // Async results not ready yet — debounce (skip scheduling if delivery).
                    if let Some(id) = search_debounce_cb.take() {
                        id.remove();
                    }
                    let entry_emit = entry_h.clone();
                    let db = search_debounce_cb.clone();
                    let fire = search_dispatch_now_cb.clone();
                    let id = glib::timeout_add_local_once(Duration::from_millis(120), move || {
                        db.set(None);
                        fire.set(true);
                        entry_emit.emit_by_name::<()>("changed", &[]);
                    });
                    search_debounce_cb.set(Some(id));
                    return;
                }
                // pending_ready or dispatch_now → proceed to render/dispatch below.

                // ── Compute results ──
                let new = if clip_mode {
                    let cfg = config.borrow();
                    search::clipboard::all_or_filtered(
                        q,
                        &clipboard.borrow(),
                        &cfg.pinned_clipboard,
                        &cfg.pinned_clipboard_images,
                        &cfg.pinned_clipboard_files,
                    )
                } else if let Some(pending) = crate::search::jobs::take_pending(&jkey) {
                    pending
                } else if is_dict {
                    // Dictionary uses thread_local fetch throttle → must run on main thread.
                    let cfg_snap = indexer.snapshot();
                    let _cfg_snap_guard = cfg_snap.read().unwrap();
                    let cfg_guard = config.borrow();
                    crate::search::search_mode("dict", q, &cfg_guard, &cfg_snap)
                } else if !crate::search::jobs::job_inflight(&jkey) {
                    let cfg_clone = config.borrow().clone();
                    let snap_clone = indexer.snapshot();
                    indexer.ensure_files_indexed();
                    crate::search::jobs::spawn(
                        jkey,
                        q.to_string(),
                        mode_word.clone(),
                        cfg_clone,
                        snap_clone,
                    );
                    return; // Keep previous results visible.
                } else {
                    return; // Job in flight, keep previous results.
                };

                // Skip widget rebuild when results haven't changed.
                // Op rows carry a volatile fraction in the action sentinel
                // that changes every tick — ignore it so progress-only
                // updates don't trigger a full list rebuild (which would
                // restart the row's fade-in animation and flash).
                let mut results_changed = true;
                {
                    let prev = results.borrow();
                    if new.len() == prev.len() {
                        results_changed = false;
                        for (a, b) in new.iter().zip(prev.iter()) {
                            if a.title != b.title || !op_actions_equal(&a.action, &b.action) {
                                results_changed = true;
                                break;
                            }
                        }
                    }
                }
                *results.borrow_mut() = new.clone();
                if new.is_empty() {
                    // No results: always collapse the results area (even when
                    // the set is unchanged) so nothing can leave blank space.
                    if results_changed {
                        list.remove_all();
                        preview_box.set_visible(false);
                        preview.clear();
                    }
                    revealer.set_reveal_child(false);
                    scroll_cb_results.set_min_content_height(0);
                    return;
                }
                if !results_changed {
                    return;
                }

                // ── Same-query refresh detection (content-scan updates) ──
                // When the query is unchanged, preserve the user's selection
                // and scroll position so scanning doesn't jump the list.
                let same_query = {
                    let prev_q = last_q.borrow();
                    !prev_q.is_empty() && *prev_q == q
                };
                // Save scroll position before the rebuild.
                let saved_scroll_val = if same_query {
                    scroll_cb_results.vadjustment().value()
                } else {
                    0.0
                };

                // ── Compute ghost + select_idx BEFORE the list rebuild ──
                // so the ghost text paints immediately when results land,
                // without waiting for row widgets to be created.
                let cur_q_len = char_count(q);
                let should_apply = if skip_ghost_typing.get() {
                    match skip_ghost_anchor_typing.get() {
                        None => {
                            skip_ghost_anchor_typing.set(Some(cur_q_len));
                            false
                        }
                        Some(anchor) if cur_q_len > anchor => {
                            skip_ghost_typing.set(false);
                            skip_ghost_anchor_typing.set(None);
                            true
                        }
                        Some(anchor) => {
                            skip_ghost_anchor_typing.set(Some(cur_q_len.min(anchor)));
                            false
                        }
                    }
                } else {
                    skip_ghost_anchor_typing.set(None);
                    true
                };
                let user_text = q;
                let in_path = user_text.starts_with('/') || user_text.starts_with('~');
                let in_emoji_mode = active_kw.as_ref().is_some_and(|kw| kw.id == "emoji");
                let allow_ghost = if in_emoji_mode {
                    false
                } else if in_path {
                    !user_text.ends_with('/')
                } else {
                    true
                };
                let exact_top = allow_ghost
                    && new
                        .first()
                        .and_then(|res| candidate_for(res, user_text))
                        .map(|c| c.eq_ignore_ascii_case(user_text))
                        .unwrap_or(false);
                let mut best_idx: Option<usize> = None;
                if should_apply && allow_ghost && !exact_top {
                    for (idx, res) in new.iter().enumerate() {
                        if let Some(cand) = candidate_for(res, user_text) {
                            let cl = cand.to_lowercase();
                            let ul = user_text.to_lowercase();
                            if cl == ul {
                                continue;
                            }
                            if cl.starts_with(&ul) && char_count(&cand) > char_count(user_text) {
                                best_idx = Some(idx);
                                break;
                            }
                        }
                    }
                }
                let path_select_idx = if user_text.starts_with('/') || user_text.starts_with('~') {
                    let ul = user_text.to_lowercase();
                    new.iter().position(|res| {
                        candidate_for(res, user_text)
                            .map(|cand| {
                                let cl = cand.to_lowercase();
                                cl == ul || cl.starts_with(&ul)
                            })
                            .unwrap_or(false)
                            && !matches!(res.action, Action::OpenInFileManager(_))
                    })
                } else {
                    None
                };
                let fallback_idx = new.iter().position(|res| {
                    if res.icon.as_deref() == Some("op-progress") {
                        let t = res.title.to_lowercase();
                        let q = user_text.to_lowercase();
                        !q.is_empty() && t.contains(&q)
                    } else {
                        true
                    }
                }).unwrap_or(0);
                let select_idx = if let Some(idx) = path_select_idx {
                    idx
                } else if user_text.ends_with('/') && user_text.starts_with('/') {
                    0
                } else {
                    best_idx.unwrap_or(fallback_idx)
                };

                // Apply ghost text NOW (before the row rebuild) so the
                // user sees the completion immediately on the entry.
                if should_apply {
                    if let Some(idx) = best_idx {
                        apply_ghost(
                            &entry_h,
                            &new[idx],
                            user_text,
                            &typed_len,
                            &suppress,
                            &prev_text_len,
                            &ghost_active_typing,
                        );
                    }
                }

                // ── Rebuild the row list ──
                // Save the current selection for preservation across rebuilds.
                // For BT scans: always preserve (live updates shouldn't jump selection).
                // For same-query content-scan refreshes: also preserve.
                let prev_selected_action = if same_query || crate::search::bluetooth::is_scanning() {
                    list.selected_row()
                        .and_then(|row| {
                            let prev = results.borrow();
                            prev.get(row.index() as usize)
                                .map(|r| r.action.clone())
                        })
                } else {
                    None
                };
                rebuild_flag.set(true);
                list.remove_all();
                revealer.set_reveal_child(true);
                let recent_file_mode = active_kw
                    .as_ref()
                    .is_some_and(|kw| kw.all_files && q.is_empty());
                preview_box.set_visible(new.iter().any(previewable));
                for r in &new {
                    let is_clip = matches!(r.kind, crate::search::ResultKind::Clipboard);
                    let is_recent_file = recent_file_mode
                        && matches!(
                            &r.action,
                            Action::OpenPath(_)
                                | Action::BrowseInto(_)
                                | Action::OpenInFileManager(_)
                        );
                    // Clipboard entries (text, image, or file) get a pin-toggle
                    // button so the user can pin/unpin them from the manager.
                    // Everything else eligible (apps, files, web, etc.) gets a
                    // universal pin-toggle that works in any search mode.
                    // The "App" trigger (manage/uninstall apps) shows system
                    // actions like "Kill: ..." that aren't meaningful to pin —
                    // no pin/unpin button there at all.
                    let in_app_trigger = active_kw.as_ref().is_some_and(|kw| kw.id == "cmd");
                    let pin_mode = if is_clip && clip_mode {
                        pin_info_for(&r.action).map(|(k, key)| PinMode::Clipboard(k, key))
                    } else if !in_app_trigger && universal_pin_eligible(r) {
                        Some(PinMode::Universal)
                    } else {
                        None
                    };
                    let row = if let Some(pm) = &pin_mode {
                        let pinned = match pm {
                            PinMode::Clipboard(kind, key) => {
                                is_pinned(&config.borrow(), *kind, key)
                            }
                            PinMode::Universal => config
                                .borrow()
                                .pinned_results
                                .iter()
                                .any(|p| p.action == r.action),
                        };
                        let label = pin_shortcut_label(&config.borrow().clipboard_pin_shortcut);
                        ResultRow::with_trash_and_pin(r, is_clip && clip_mode, pinned, &label)
                    } else {
                        ResultRow::with_trash(r, (is_clip && clip_mode) || is_recent_file)
                    };
                    if let Some(btn) = &row.pin_button {
                        let cfg = config.clone();
                        let clip = clipboard.clone();
                        let entry_ref = e.clone();
                        let action = r.action.clone();
                        let result_clone = r.clone();
                        let pm = pin_mode.clone();
                        btn.connect_clicked(move |btn| {
                            match &pm {
                                Some(PinMode::Clipboard(..)) => {
                                    toggle_clipboard_pin(&cfg, &action, &clip)
                                }
                                Some(PinMode::Universal) => {
                                    toggle_universal_pin(&cfg, &result_clone)
                                }
                                None => {}
                            }
                            pulse(btn);
                            let entry_ref2 = entry_ref.clone();
                            glib::idle_add_local_once(move || {
                                entry_ref2.emit_by_name::<()>("changed", &[]);
                            });
                        });
                    }
                    // Wire trash button to remove the clipboard entry (text or image)
                    if let Some(btn) = &row.trash_button {
                        let clip = clipboard.clone();
                        let cfg = config.clone();
                        let entry_ref = e.clone();
                        let action = r.action.clone();
                        let is_clip_entry = is_clip && clip_mode;
                        let undo_revealer = undo_revealer_cb.clone();
                        let last_action_was_delete = last_action_was_delete_cb.clone();
                        let undo_generation = undo_generation_cb.clone();
                        btn.connect_clicked(move |btn| {
                            // Prevent deletion of pinned clipboard items
                            if is_clip_entry && is_clipboard_item_pinned(&cfg.borrow(), &action) {
                                log::info!("clipboard: skipping deletion of pinned item");
                                return;
                            }

                            let mut undoable = false;
                            {
                                let mut c = clip.borrow_mut();
                                match &action {
                                    Action::CopyToClipboard(text) => {
                                        c.remove_text(text);
                                        undoable = true;
                                    }
                                    Action::CopyImageToClipboard(path) => {
                                        c.remove_image(path);
                                        undoable = true;
                                    }
                                    Action::CopyFileToClipboard(path) => {
                                        c.remove_file(path);
                                        undoable = true;
                                    }
                                    Action::OpenPath(path)
                                    | Action::BrowseInto(path)
                                    | Action::OpenInFileManager(path) => {
                                        crate::recent_paths::remove(path);
                                    }
                                    _ => {}
                                }
                            }
                            pulse(btn);
                            // Show a Nautilus-style "Undo" toast for clipboard
                            // deletions; Ctrl+Z within 2 seconds also restores it.
                            if undoable && is_clip_entry {
                                last_action_was_delete.set(true);
                                undo_revealer.set_reveal_child(true);
                                let gen = undo_generation.get() + 1;
                                undo_generation.set(gen);
                                let undo_revealer2 = undo_revealer.clone();
                                let undo_generation2 = undo_generation.clone();
                                glib::timeout_add_local_once(Duration::from_secs(2), move || {
                                    if undo_generation2.get() == gen {
                                        undo_revealer2.set_reveal_child(false);
                                    }
                                });
                            }
                            // Defer the refresh: emitting "changed" synchronously would
                            // rebuild the list and destroy this very row mid-callback.
                            let entry_ref2 = entry_ref.clone();
                            glib::idle_add_local_once(move || {
                                entry_ref2.emit_by_name::<()>("changed", &[]);
                            });
                        });
                    }
                    list.append(&row.container);
                }
                // Size the results list so at most 5 rows are visible by
                // default; with fewer results the list (and window) shrink
                // to fit exactly that many rows, with the rest scrollable.
                {
                    let visible_rows = new.len().min(5).max(1);
                    if let Some(first) = list.row_at_index(0) {
                        let (_, natural, _, _) = first.measure(gtk::Orientation::Vertical, -1);
                        if natural > 0 {
                            scroll_cb_results.set_max_content_height(natural * visible_rows as i32);
                        }
                    }
                    // Release the canonical min set before map so the window
                    // can shrink to fit the actual content.
                    scroll_cb_results.set_min_content_height(0);
                }
                // Restore selection across rebuilds (BT scan or same-query refresh).
                rebuild_flag.set(false);
                if let Some(sel_action) = &prev_selected_action {
                    if let Some(idx) = new.iter().position(|r| &r.action == sel_action) {
                        if let Some(f) = list.row_at_index(idx as i32) {
                            list.select_row(Some(&f));
                        }
                    }
                } else {
                    // Normal auto-select + ghost (new query or no prior selection).
                    if let Some(f) = list.row_at_index(select_idx as i32) {
                        list.select_row(Some(&f));
                    } else {
                        preview.clear();
                    }
                }
                // Restore scroll position for same-query rebuilds so the list
                // doesn't snap to the top while the user is scrolling.
                if same_query {
                    scroll_cb_results.vadjustment().set_value(saved_scroll_val);
                }
                // Track the last rendered query for the next rebuild.
                *last_q.borrow_mut() = q.to_string();
            }));
        }

        // ── Row activated (click) ──
        {
            let e = entry.clone();
            let r = results.clone();
            let w = window.clone();
            let typed_len_c = typed_len.clone();
            let sp_fn = start_progress_fn.clone();
            let bt_scan = start_bt_scan_ui.clone();
            let popover_open_click = popover_open.clone();
            let shown_click = shown.clone();
            list.connect_row_activated(move |_, row| {
                let rs = r.borrow();
                if let Some(res) = rs.get(row.index() as usize) {
                    let query = real_typed_text(&e, &typed_len_c);
                    if let Action::RunWithProgress { title, args } = &res.action {
                        sp_fn(title.clone(), args.clone());
                    } else if let Action::Bluetooth { op, mac } = &res.action {
                        if op == "scan" {
                            bt_scan();
                        } else {
                            crate::search::bluetooth::stop_scan(crate::search::bluetooth::ScanStop::DeviceChosen);
                            crate::search::bluetooth::run_action(op, mac, &res.title);
                        }
                    } else if let Action::StartOperation {
                        title,
                        source,
                        icon,
                        args,
                    } = &res.action
                    {
                        let plan = crate::search::uninstall::Plan {
                            title: title.clone(),
                            source: source.clone(),
                            icon: icon.clone(),
                            args: args.clone(),
                        };
                        let question = match title.strip_prefix("Update: ") {
                            Some(name) => gettext("Do you want to update {name}?").replace("{name}", &name),
                            None if title.starts_with("Installing ") => {
                                let name = title.strip_prefix("Installing ").unwrap();
                                gettext("Are you sure you want to install {name}?").replace("{name}", &name)
                            }
                            None if title.starts_with("Uninstalling ") => {
                                let name = title.strip_prefix("Uninstalling ").unwrap();
                                gettext("Are you sure you want to uninstall {name}?").replace("{name}", &name)
                            }
                            None => gettext("Do you want to {action}?").replace("{action}", &title.to_lowercase()),
                        };
                        show_confirm_dialog(&w, &popover_open_click, &e, question, Some(plan), "", query.clone());
                        drop(rs);
                        return;
                    } else if let Action::ConfirmRunCommand(cmd) = &res.action {
                        popover_open_click.set(true);
                        let dialog = adw::MessageDialog::builder()
                            .transient_for(&w)
                            .heading(gettext("Are you sure?"))
                            .build();
                        dialog.add_response("cancel", &gettext("Cancel"));
                        dialog.add_response("confirm", &gettext("Confirm"));
                        dialog.set_default_response(Some("confirm"));
                        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
                        let w2 = w.clone();
                        let s2 = shown_click.clone();
                        let po = popover_open_click.clone();
                        let c = cmd.clone();
                        dialog.connect_response(None, move |_, resp| {
                            po.set(false);
                            if resp == "confirm" {
                                let _ = crate::app::spawn_host_shell_command(&c);
                                dismiss(&w2, &s2, false);
                            }
                        });
                        dialog.present();
                        drop(rs);
                        return;
                    } else if let Action::RemoveAppImage(path) = &res.action {
                        // Local file delete: the same "moved to the Trash"
                        // dialog Find mode uses for files.
                        let name = crate::search::appimage::display_name(path);
                        confirm_delete_path(&w, &popover_open_click, &e, path, &name);
                        crate::history::record(&query, &res.title);
                        drop(rs);
                        return;
                    } else {
                        activate(res, &rs, &e, &w, &shown_click);
                    }
                    crate::history::record(&query, &res.title);
                }
            });
        }

        // ── Selection -> preview + update ghost ──
        {
            let r = results.clone();
            let p = preview.clone();
            let rebuild_flag = rebuilding.clone();
            list.connect_row_selected(move |_, row| {
                // During a list rebuild, ignore the transient None selection
                // (caused by remove_all) to avoid clearing the preview.
                if rebuild_flag.get() {
                    return;
                }
                if let Some(row) = row {
                    let rs = r.borrow();
                    upd_preview(row, &rs, &p);
                } else {
                    p.clear();
                }
            });
        }

        // ── Key handling on entry ──
        // Capture phase so we see keys before Entry's default behavior
        let kc = gtk::EventControllerKey::builder()
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        {
            let l = list.clone();
            let scroll_kc = scroll.clone();
            let w = window.clone();
            let shown_kc = shown.clone();
            let e = entry.clone();
            let r = results.clone();
            let typed_len_c = typed_len.clone();
            let suppress = suppress_changed.clone();
            let skip_ghost_clone = skip_ghost.clone();
            let ghost_active_kc = ghost_active.clone();
            let cfg_kc = config.clone();
            let mode_kc = active_mode.clone();
            let clip_mode_kc = clipboard_mode.clone();
            let chip_kc = mode_chip.clone();
            let chip_icon_kc = mode_icon.clone();
            let chip_label_kc = mode_label.clone();
            let typed_len_mode = typed_len.clone();
            let clip_hist_kc = clipboard.clone();
            let undo_stack_kc = undo_stack.clone();
            let redo_stack_kc = redo_stack.clone();
            let is_undo_redo_kc = is_undo_redo.clone();
            let do_undo_kc = do_undo.clone();
            let last_action_was_delete_kc = last_action_was_delete.clone();
            let undo_revealer_kc = undo_revealer.clone();
            let undo_generation_kc = undo_generation.clone();
            let last_action_was_unpin_kc = last_action_was_unpin.clone();
            let unpin_undo_revealer_kc = unpin_undo_revealer.clone();
            let _unpin_generation_kc = unpin_generation.clone();
            let cfg_for_unpin = config.clone();
            let sp_fn_kc = start_progress_fn.clone();
            let bt_scan_kc = start_bt_scan_ui.clone();
            let show_ops_kc = show_ops_popover.clone();
            let show_hints_kc = show_hints_popover.clone();
            let popover_open_kc = popover_open.clone();
            let ops_popover_kc = ops_popover.clone();
            let hints_popover_kc = hints_popover.clone();
            let mode_from_keybinding_kc = mode_from_keybinding.clone();
            let preview_kc = preview.clone();
            kc.connect_key_pressed(move |_, key, _, state| {
                use gtk::gdk::Key;

                // ── File operations: Ctrl+C / Ctrl+X / Ctrl+V plus text undo/redo ──
                // These act on the selected result / current browse location.
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);

                // Resolve a configurable in-window shortcut (from config.rs)
                // against the pressed key/state. Empty config value = disabled.
                // `case_insensitive` tolerates keysym case shifts (Shift+z
                // delivers `Z` on most layouts).
                let hit =
                    |field: fn(&Config) -> &str,
                     key: gtk::gdk::Key,
                     state: gtk::gdk::ModifierType,
                     case_insensitive: bool| -> bool {
                        let s = {
                            let cfg = cfg_kc.borrow();
                            field(&cfg).to_string()
                        };
                        if s.is_empty() {
                            return false;
                        }
                        if let Some((accel_key, accel_mods)) = gtk::accelerator_parse(&s) {
                            let key_matches = if case_insensitive {
                                key.to_upper() == accel_key.to_upper()
                            } else {
                                key == accel_key
                            };
                            key_matches && state == accel_mods
                        } else {
                            false
                        }
                    };
                let copy_hit = hit(|c| c.copy_shortcut.as_str(), key, state, false);
                let cut_hit = hit(|c| c.cut_shortcut.as_str(), key, state, false);
                let paste_hit = hit(|c| c.paste_shortcut.as_str(), key, state, false);
                let undo_hit = hit(|c| c.undo_shortcut.as_str(), key, state, false);
                let redo_hit = hit(|c| c.redo_shortcut.as_str(), key, state, true);
                let select_all_hit = hit(|c| c.select_all_shortcut.as_str(), key, state, false);

                // Escape closes an open Operations/Hints popover first, leaving
                // the search window itself open and other shortcuts usable.
                if key == Key::Escape && popover_open_kc.get() {
                    if ops_popover_kc.is_visible() {
                        ops_popover_kc.popdown();
                    }
                    if hints_popover_kc.is_visible() {
                        hints_popover_kc.popdown();
                    }
                    popover_open_kc.set(false);
                    return glib::Propagation::Stop;
                }

                if ctrl {
                    log::info!(
                        "key_pressed: ctrl=1 shift={} key={:?} name={:?} unicode={:?}",
                        shift as u8,
                        key,
                        key.name(),
                        key.to_unicode()
                    );
                }

                // A web result: the configured key opens the search in a private
                // window of the default browser (default Ctrl+Enter). Checked
                // before everything else because plain Enter would otherwise
                // open the same search normally.
                if hit(|c| c.private_search_shortcut.as_str(), key, state, false) {
                    let web_url = l.selected_row().and_then(|row| {
                        r.borrow()
                            .get(row.index() as usize)
                            .filter(|res| res.kind == crate::search::ResultKind::Web)
                            .and_then(|res| match &res.action {
                                Action::OpenUrl(u) => Some(u.clone()),
                                _ => None,
                            })
                    });
                    if let Some(url) = web_url {
                        if let Some(argv) =
                            crate::search::browser_launch::private_window_argv(&url)
                        {
                            crate::search::browser_launch::spawn_private_window(&argv);
                            dismiss(&w, &shown_kc, false);
                        } else {
                            log::info!(
                                "private search: the default browser has no known private mode"
                            );
                        }
                        // Either way the key is ours: falling through would open
                        // the very window the shortcut promises to keep private.
                        return glib::Propagation::Stop;
                    }
                }

                // Select all text in the search entry, regardless of trigger mode.
                if select_all_hit {
                    let len = char_count(&e.text()) as i32;
                    e.select_region(0, len);
                    return glib::Propagation::Stop;
                }

                // Open the Operations popover (ongoing + past ops).
                if hit(|c| c.operations_shortcut.as_str(), key, state, false) {
                    show_ops_kc();
                    return glib::Propagation::Stop;
                }
                // Open the Hints popover (keyboard shortcuts).
                if hit(|c| c.hints_shortcut.as_str(), key, state, false) {
                    show_hints_kc();
                    return glib::Propagation::Stop;
                }
                // Uninstall the selected application (default universal mode).
                if hit(|c| c.uninstall_shortcut.as_str(), key, state, false) && mode_kc.borrow().is_none() {
                    let res = l
                        .selected_row()
                        .and_then(|row| r.borrow().get(row.index() as usize).cloned());
                    if let Some(res) = res {
                        if res.kind == crate::search::ResultKind::App {
                            if let Action::LaunchDesktopFile(path) = &res.action {
                                let icon = res.icon.clone().unwrap_or_default();
                                let name = res.title.clone();
                                let plan =
                                    crate::search::uninstall::plan_for_app(path, &name, &icon);
                                show_confirm_dialog(
                                    &w,
                                    &popover_open_kc,
                                    &e,
                                    gettext("Uninstall {name}?").replace("{name}", &name),
                                    plan,
                                    "Couldn't determine how this app was installed.",
                                    String::new(),
                                );
                                return glib::Propagation::Stop;
                            } else if let Action::LaunchAppImage(path) = &res.action {
                                // AppImage launch row: the file itself goes to
                                // the Trash — no package manager involved.
                                let name = res.title.clone();
                                confirm_delete_path(&w, &popover_open_kc, &e, path, &name);
                                return glib::Propagation::Stop;
                            }
                        }
                    }
                }
                // Kill a running instance of the selected application (default universal mode).
                if hit(|c| c.kill_shortcut.as_str(), key, state, false) && mode_kc.borrow().is_none() {
                    let res = l
                        .selected_row()
                        .and_then(|row| r.borrow().get(row.index() as usize).cloned());
                    if let Some(res) = res {
                        if res.kind == crate::search::ResultKind::App {
                            if let Action::LaunchDesktopFile(path) = &res.action {
                                if !crate::search::uninstall::is_app_running(path, &res.title) {
                                    return glib::Propagation::Stop;
                                }
                                let icon = res.icon.clone().unwrap_or_default();
                                let name = res.title.clone();
                                let plan =
                                    crate::search::uninstall::kill_plan_for_app(path, &name, &icon);
                                show_confirm_dialog(
                                    &w,
                                    &popover_open_kc,
                                    &e,
                                    gettext("Kill {name}?").replace("{name}", &name),
                                    Some(plan),
                                    "",
                                    String::new(),
                                );
                                return glib::Propagation::Stop;
                            }
                        }
                    }
                }

                // Find mode: open the location of the selected file/folder in
                // the default file manager (default Ctrl+Enter). A folder
                // opens itself, a file opens its containing folder; with
                // nothing selected the path being browsed is used. Deliberately
                // FM-agnostic (gio/xdg-open based) so any file manager works.
                {
                    let in_find = mode_kc
                        .borrow()
                        .as_ref()
                        .is_some_and(|kw| kw.all_files);
                    let s = cfg_kc.borrow().open_location_shortcut.clone();
                    if in_find && !s.is_empty() {
                        if let Some((accel_key, accel_mods)) = gtk::accelerator_parse(&s) {
                            let enter_family = |k: gtk::gdk::Key| {
                                matches!(
                                    k,
                                    gtk::gdk::Key::Return
                                        | gtk::gdk::Key::KP_Enter
                                        | gtk::gdk::Key::Linefeed
                                )
                            };
                            let key_matches =
                                key == accel_key || (enter_family(accel_key) && enter_family(key));
                            if key_matches && state == accel_mods {
                                let target = l
                                    .selected_row()
                                    .and_then(|row| r.borrow().get(row.index() as usize).cloned())
                                    .and_then(|res| match &res.action {
                                        Action::OpenPath(p) => Some(
                                            crate::fileops::location_target(
                                                p,
                                                res.kind == crate::search::ResultKind::Folder,
                                            ),
                                        ),
                                        Action::BrowseInto(p)
                                        | Action::OpenInFileManager(p) => Some(p.clone()),
                                        _ => None,
                                    })
                                    .or_else(|| {
                                        let typed = current_typed(&e, &typed_len_c);
                                        let t = typed.trim();
                                        if t.starts_with('/') || t.starts_with("~/") || t == "~" {
                                            expand_path(t)
                                        } else {
                                            None
                                        }
                                    });
                                if let Some(dir) = target {
                                    log::info!(
                                        "open-location shortcut: opening {} in file manager",
                                        dir.display()
                                    );
                                    crate::app::open_in_file_manager(&dir);
                                    dismiss(&w, &shown_kc, false);
                                    return glib::Propagation::Stop;
                                }
                                log::info!("open-location shortcut pressed but no location resolved");
                            }
                        }
                    }
                }

                // Find mode: delete the selected file/folder (default Ctrl+D).
                // Always behind a confirmation dialog whose default is No.
                if hit(|c| c.delete_file_shortcut.as_str(), key, state, false) {
                    let in_find = mode_kc
                        .borrow()
                        .as_ref()
                        .is_some_and(|kw| kw.all_files);
                    if in_find {
                        let target = l
                            .selected_row()
                            .and_then(|row| r.borrow().get(row.index() as usize).cloned())
                            .and_then(|res| match &res.action {
                                Action::OpenPath(p)
                                | Action::BrowseInto(p)
                                | Action::OpenInFileManager(p) => {
                                    Some((p.clone(), res.title.clone()))
                                }
                                _ => None,
                            });
                        if let Some((path, name)) = target {
                            confirm_delete_path(&w, &popover_open_kc, &e, &path, &name);
                            return glib::Propagation::Stop;
                        }
                    }
                }

                // Open folder in a terminal (default Ctrl+Shift+Enter). Only available in the
                // Find trigger when browsing a folder (query starts with ~/ or /) or when
                // a folder result is selected.
                {
                    let in_find = mode_kc
                        .borrow()
                        .as_ref()
                        .is_some_and(|kw| kw.all_files);
                    let typed = current_typed(&e, &typed_len_c);
                    let query = typed.trim();
                    let browsing_folder = query.starts_with('/') || query.starts_with("~/") || query == "~";
                    let selected_is_folder = l.selected_row().and_then(|row| {
                        r.borrow().get(row.index() as usize).map(|res| {
                            matches!(&res.action, Action::BrowseInto(_) | Action::OpenInFileManager(_))
                                || matches!(&res.action, Action::OpenPath(_) if matches!(res.kind, crate::search::ResultKind::Folder))
                        })
                    }).unwrap_or(false);
                    if in_find && (browsing_folder || selected_is_folder) {
                        let s = cfg_kc.borrow().terminal_shortcut.clone();
                        if !s.is_empty() {
                            if let Some((accel_key, accel_mods)) = gtk::accelerator_parse(&s) {
                                let enter_family = |k: gtk::gdk::Key| {
                                    matches!(
                                        k,
                                        gtk::gdk::Key::Return
                                            | gtk::gdk::Key::KP_Enter
                                            | gtk::gdk::Key::Linefeed
                                    )
                                };
                                let key_matches =
                                    key == accel_key || (enter_family(accel_key) && enter_family(key));
                                if key_matches && state == accel_mods {
                                    let folder_path = l.selected_row().and_then(|row| {
                                        r.borrow()
                                            .get(row.index() as usize)
                                            .and_then(|res| match &res.action {
                                                Action::BrowseInto(p) => Some(p.clone()),
                                                Action::OpenInFileManager(p) => Some(p.clone()),
                                                Action::OpenPath(p)
                                                    if matches!(res.kind, crate::search::ResultKind::Folder) =>
                                                {
                                                    Some(p.clone())
                                                }
                                                _ => None,
                                            })
                                    });
                                    if let Some(p) = folder_path {
                                        log::info!("terminal shortcut: opening terminal at {}", p.display());
                                        crate::app::open_terminal_at(&p);
                                        dismiss(&w, &shown_kc, false);
                                        return glib::Propagation::Stop;
                                    } else {
                                        log::info!("terminal shortcut pressed but no folder result selected");
                                    }
                                }
                            }
                        }
                    }
                }

                // Undo — a pending Operations dismissal wins over text/file undo: it is the
                // most recent thing the user did, and it only lives for the
                // dismiss grace window (a committed dismissal is gone). When the
                // bar is on screen the row slides back in from the side it left.
                if undo_hit && ops_undo_last_dismiss() {
                    return glib::Propagation::Stop;
                }

                // Undo — if deleting a clipboard entry was the user's last
                // action, restore it (Nautilus-style "undo trash") instead of
                // undoing typed text. No time limit: stays valid until the
                // user types or otherwise acts again.
                if undo_hit
                    && last_action_was_delete_kc.get()
                    && do_undo_kc()
                {
                    return glib::Propagation::Stop;
                }

                // Undo — if unpinning a clipboard entry was the user's last action,
                // restore the pin (5-second window).
                if undo_hit && last_action_was_unpin_kc.get() {
                    // Restore the last unpinned item by re-pinning it
                    if let Some((kind, key_val)) =
                        clip_hist_kc.borrow_mut().get_and_clear_last_unpinned()
                    {
                        let mut c = cfg_for_unpin.borrow_mut();
                        let list = match kind {
                            crate::clipboard::ClipboardEntryKind::Text => &mut c.pinned_clipboard,
                            crate::clipboard::ClipboardEntryKind::Image => {
                                &mut c.pinned_clipboard_images
                            }
                            crate::clipboard::ClipboardEntryKind::File => {
                                &mut c.pinned_clipboard_files
                            }
                        };
                        // Re-add to pinned list if not already there
                        if !list.iter().any(|p| p == &key_val) {
                            list.insert(0, key_val);
                        }
                        c.save();
                        last_action_was_unpin_kc.set(false);
                        unpin_undo_revealer_kc.set_reveal_child(false);
                        e.emit_by_name::<()>("changed", &[]);
                        return glib::Propagation::Stop;
                    }
                }

                // Undo — undo typed text (search box only).
                if undo_hit {
                    let cur = e.text().to_string();
                    let mut prev_text: Option<String> = None;
                    {
                        let mut us = undo_stack_kc.borrow_mut();
                        while let Some(s) = us.pop() {
                            if s != cur {
                                prev_text = Some(s);
                                break;
                            }
                        }
                    }
                    if let Some(prev_text) = prev_text {
                        let cur = e.text().to_string();
                        redo_stack_kc.borrow_mut().push(cur);
                        is_undo_redo_kc.set(true);
                        skip_ghost_clone.set(true);
                        ghost_active_kc.set(false);
                        typed_len_c.set(char_count(&prev_text));
                        e.set_text(&prev_text);
                        e.set_position(char_count(&prev_text) as i32);
                    }
                    return glib::Propagation::Stop;
                }
                // Redo — redo typed text (search box only).
                if redo_hit {
                    let next_text = { redo_stack_kc.borrow_mut().pop() };
                    if let Some(next_text) = next_text {
                        let cur = e.text().to_string();
                        undo_stack_kc.borrow_mut().push(cur);
                        is_undo_redo_kc.set(true);
                        skip_ghost_clone.set(true);
                        ghost_active_kc.set(false);
                        typed_len_c.set(char_count(&next_text));
                        e.set_text(&next_text);
                        e.set_position(char_count(&next_text) as i32);
                    }
                    return glib::Propagation::Stop;
                }
                // Pin/unpin the selected result (default Ctrl+P, configurable in
                // Settings > Shortcuts). In clip mode this toggles the
                // clipboard-specific pin; elsewhere it toggles the universal pin
                // for any eligible result (apps, files, web, etc.).
                {
                    let shortcut = cfg_kc.borrow().clipboard_pin_shortcut.clone();
                    if let Some((accel_key, accel_mods)) = gtk::accelerator_parse(&shortcut) {
                        if key == accel_key && state == accel_mods {
                            let res = l
                                .selected_row()
                                .and_then(|row| r.borrow().get(row.index() as usize).cloned());
                            if let Some(res) = res {
                                if clip_mode_kc.get() {
                                    toggle_clipboard_pin(&cfg_kc, &res.action, &clip_hist_kc);
                                } else if universal_pin_eligible(&res) {
                                    toggle_universal_pin(&cfg_kc, &res);
                                }
                                let entry_ref = e.clone();
                                glib::idle_add_local_once(move || {
                                    entry_ref.emit_by_name::<()>("changed", &[]);
                                });
                            }
                            return glib::Propagation::Stop;
                        }
                    }
                }
                // Delete the selected clipboard-manager entry (default Delete,
                // configurable in Settings > Clipboard).
                if clip_mode_kc.get() {
                    let shortcut = cfg_kc.borrow().clipboard_delete_shortcut.clone();
                    if let Some((accel_key, accel_mods)) = gtk::accelerator_parse(&shortcut) {
                        if key == accel_key && state == accel_mods {
                            let action = l.selected_row().and_then(|row| {
                                r.borrow()
                                    .get(row.index() as usize)
                                    .map(|res| res.action.clone())
                            });
                            if let Some(action) = action {
                                let mut undoable = false;
                                {
                                    let mut c = clip_hist_kc.borrow_mut();
                                    match &action {
                                        Action::CopyToClipboard(text) => {
                                            c.remove_text(text);
                                            undoable = true;
                                        }
                                        Action::CopyImageToClipboard(path) => {
                                            c.remove_image(path);
                                            undoable = true;
                                        }
                                        Action::CopyFileToClipboard(path) => {
                                            c.remove_file(path);
                                            undoable = true;
                                        }
                                        _ => {}
                                    }
                                }
                                // Same Nautilus-style "Undo" toast + no-time-limit
                                // Ctrl+Z as the row trash button.
                                if undoable {
                                    last_action_was_delete_kc.set(true);
                                    undo_revealer_kc.set_reveal_child(true);
                                    let gen = undo_generation_kc.get() + 1;
                                    undo_generation_kc.set(gen);
                                    let undo_revealer2 = undo_revealer_kc.clone();
                                    let undo_generation2 = undo_generation_kc.clone();
                                    glib::timeout_add_local_once(
                                        Duration::from_secs(2),
                                        move || {
                                            if undo_generation2.get() == gen {
                                                undo_revealer2.set_reveal_child(false);
                                            }
                                        },
                                    );
                                }
                                let entry_ref = e.clone();
                                glib::idle_add_local_once(move || {
                                    entry_ref.emit_by_name::<()>("changed", &[]);
                                });
                            }
                            return glib::Propagation::Stop;
                        }
                    }
                }
                // Copy — if the user has text selected in the search entry,
                // that always takes priority: copy the selected text, no
                // matter which trigger mode is active or what's selected in
                // the results list.
                if copy_hit {
                    if let Some((start, end)) = e.selection_bounds() {
                        let full = e.text();
                        let chars: Vec<char> = full.chars().collect();
                        let lo = start.min(end).max(0) as usize;
                        let hi = end.max(start).max(0) as usize;
                        let selected: String = chars
                            .get(lo..hi.min(chars.len()))
                            .map(|s| s.iter().collect())
                            .unwrap_or_default();
                        if !selected.is_empty() {
                            e.clipboard().set_text(&selected);
                            return glib::Propagation::Stop;
                        }
                    }
                }
                // Copy on a clipboard-manager entry: copy the SELECTED entry's
                // content back onto the system clipboard (text, image, or file).
                if copy_hit {
                    let action = l.selected_row().and_then(|row| {
                        r.borrow()
                            .get(row.index() as usize)
                            .map(|res| res.action.clone())
                    });
                    match action {
                        Some(Action::CopyToClipboard(text)) => {
                            crate::clipboard::set_text(&text);
                            return glib::Propagation::Stop;
                        }
                        Some(Action::CopyImageToClipboard(path)) => {
                            crate::clipboard::set_image(&path);
                            return glib::Propagation::Stop;
                        }
                        Some(Action::CopyFileToClipboard(path)) => {
                            crate::clipboard::set_file(&path);
                            crate::fileops::copy(&path);
                            // The clipboard watcher skips our own content
                            // (is_local): move the entry to the front
                            // explicitly to preserve re-copy recency.
                            crate::app::with_state(|st| {
                                st.clipboard.borrow_mut().push_file(path.clone())
                            });
                            return glib::Propagation::Stop;
                        }
                        _ => {}
                    }
                }
                // Copy — COPY the selected file/folder/etc. (only if a result is
                // selected; otherwise let the entry do normal text copy).
                if copy_hit {
                    if let Some(path) = selected_path(&l, &r) {
                        crate::fileops::copy(&path);
                        crate::clipboard::set_file(&path);
                        // Record it in the clipboard manager. Images go in as
                        // Image entries (so they show a picture thumbnail);
                        // everything else as a File entry (type/thumbnail preview).
                        record_clip_item(&clip_hist_kc, &path);
                        return glib::Propagation::Stop;
                    }
                }
                // Cut — cut the selected file/folder.
                if cut_hit {
                    if let Some(path) = selected_path(&l, &r) {
                        crate::fileops::cut(&path);
                        crate::clipboard::set_file(&path);
                        record_clip_item(&clip_hist_kc, &path);
                        return glib::Propagation::Stop;
                    }
                }
                // Paste — PASTE into the current destination directory. Only
                // intercepted when the search entry is empty and there's a
                // pending file/folder to paste; otherwise paste always falls
                // through to the entry's normal text-paste behavior.
                if paste_hit && e.text().is_empty() {
                    if crate::fileops::has_pending() {
                        if let Some(dest) = paste_destination(&e, &l, &r) {
                            crate::fileops::paste(&dest);
                            return glib::Propagation::Stop;
                        }
                    }
                }

                // Delete previous word (word-by-word deletion). The configurable combo
                // covers Ctrl+Space; Ctrl+Backspace is kept as a fixed alias.
                let is_backspace = key == gtk::gdk::Key::BackSpace
                    || key
                        .name()
                        .map(|name| name.as_str().eq_ignore_ascii_case("backspace"))
                        .unwrap_or(false);
                let word_del = hit(|c| c.delete_word_shortcut.as_str(), key, state, false)
                    || (ctrl && is_backspace);
                if word_del {
                    push_undo_snapshot(&undo_stack_kc, &e.text());
                    redo_stack_kc.borrow_mut().clear();
                    if !ghost_active_kc.get() {
                        typed_len_c.set(char_count(&e.text()));
                    }
                    ghost_active_kc.set(false);
                    delete_prev_word(&e, &typed_len_c, &skip_ghost_clone);
                    return glib::Propagation::Stop;
                }

                match key {
                    // Space while a ghost suggestion is active: drop the
                    // suggestion and insert a literal space after what the
                    // user actually typed, rather than letting GTK's default
                    // "replace selection" behavior interact unpredictably
                    // with the ghost-selected tail.
                    Key::space if ghost_active_kc.get() => {
                        push_undo_snapshot(&undo_stack_kc, &e.text());
                        redo_stack_kc.borrow_mut().clear();
                        let typed = current_typed(&e, &typed_len_c);
                        let new_text = format!("{} ", typed);
                        ghost_active_kc.set(false);
                        skip_ghost_clone.set(false);
                        e.set_text(&new_text);
                        let end = char_count(&new_text) as i32;
                        e.select_region(end, end);
                        e.set_position(end);
                        glib::Propagation::Stop
                    }
                    Key::Escape => {
                        dismiss(&w, &shown_kc, true);
                        glib::Propagation::Stop
                    }
                    Key::Down => {
                        move_sel(&l, 1, &scroll_kc);
                        glib::Propagation::Stop
                    }
                    Key::Up => {
                        move_sel(&l, -1, &scroll_kc);
                        glib::Propagation::Stop
                    }
                    // While the Operations popover is open, Left/Right scrub the
                    // playing track ±5s; otherwise they flip through a
                    // multi-page preview (slide deck / PDF / sheet tabs)
                    // while one is on screen — same preconditions as the
                    // wheel stepper. Only bare keys are hijacked: Ctrl/Alt/
                    // Shift combos and popover time keep their meaning, and
                    // when no multi-page preview shows the keys fall through
                    // to the entry's normal cursor movement.
                    Key::Left
                        if !ctrl
                            && !shift
                            && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                            && !popover_open_kc.get()
                            && preview_kc.key_step_page(-1) =>
                    {
                        glib::Propagation::Stop
                    }
                    Key::Right
                        if !ctrl
                            && !shift
                            && !state.contains(gtk::gdk::ModifierType::ALT_MASK)
                            && !popover_open_kc.get()
                            && preview_kc.key_step_page(1) =>
                    {
                        glib::Propagation::Stop
                    }
                    Key::Left => {
                        glib::Propagation::Proceed
                    }
                    Key::Right => {
                        glib::Propagation::Proceed
                    }
                    Key::Tab | Key::ISO_Left_Tab => {
                        let action = l.selected_row().and_then(|row| {
                            let rs = r.borrow();
                            rs.get(row.index() as usize).map(|res| res.action.clone())
                        });
                        // Prevent the post-Tab search from immediately adding ANOTHER
                        // ghost (e.g. a sub-folder of the just-accepted folder).
                        // Tab should land exactly at what the ghost showed.
                        skip_ghost_clone.set(true);
                        ghost_active_kc.set(false);
                        if let Some(
                            Action::OpenPath(path)
                            | Action::BrowseInto(path)
                            | Action::OpenInFileManager(path),
                        ) = action.as_ref()
                        {
                            crate::recent_paths::record(path);
                        }
                        accept_ghost(&e, &typed_len_c, &suppress, action.as_ref());
                        glib::Propagation::Stop
                    }
                    Key::BackSpace => {
                        let current = e.text().to_string();
                        if current.is_empty() {
                            // Backspace on an empty triggered search exits the mode,
                            // but only if the mode was entered by typing the trigger
                            // word. Keybinding-launched modes stay active (the user
                            // explicitly chose this mode).
                            if mode_kc.borrow().is_some() {
                                if mode_from_keybinding_kc.get() {
                                    // Keybinding mode: swallow the backspace, stay in mode.
                                    return glib::Propagation::Stop;
                                }
                                *mode_kc.borrow_mut() = None;
                                clip_mode_kc.set(false);
                                chip_kc.set_visible(false);
                                skip_ghost_clone.set(true);
                                ghost_active_kc.set(false);
                                e.set_text("");
                                typed_len_mode.set(0);
                                e.emit_by_name::<()>("changed", &[]);
                                return glib::Propagation::Stop;
                            }
                            return glib::Propagation::Proceed;
                        }

                        // A ghost suggestion is showing (selected tail text, any
                        // mode/trigger). Backspace always discards the ghost
                        // completely AND removes the last real typed character in
                        // the same press, so the suggestion never lingers while
                        // deleting. Driven by `ghost_active`/`typed_len` rather
                        // than the raw selection so it's consistent across
                        // trigger modes too.
                        if ghost_active_kc.get() {
                            push_undo_snapshot(&undo_stack_kc, &current);
                            redo_stack_kc.borrow_mut().clear();
                            skip_ghost_clone.set(true);
                            ghost_active_kc.set(false);
                            let typed_chars = logical_typed_chars(&e, &typed_len_c);
                            let new_len = typed_chars.saturating_sub(1);
                            let typed: String = current.chars().take(new_len).collect();
                            typed_len_c.set(char_count(&typed));
                            // set_text() can emit "changed" more than once (a
                            // delete-all + insert pair), which would let the
                            // second event slip past the skip_ghost suppression
                            // and re-apply the ghost immediately. Suppress those
                            // built-in emissions and fire exactly one "changed"
                            // ourselves afterwards.
                            suppress.set(true);
                            e.set_text(&typed);
                            e.set_position(char_count(&typed) as i32);
                            suppress.set(false);
                            let entry = e.clone();
                            glib::idle_add_local_once(move || {
                                entry.emit_by_name::<()>("changed", &[]);
                            });
                            return glib::Propagation::Stop;
                        }
                        push_undo_snapshot(&undo_stack_kc, &current);
                        redo_stack_kc.borrow_mut().clear();
                        skip_ghost_clone.set(true);
                        ghost_active_kc.set(false);
                        glib::Propagation::Proceed
                    }
                    Key::Return | Key::KP_Enter => {
                        // Helper: activate a trigger mode by its keyword.
                        let enter_mode = |kw: crate::config::CommandKeyword| {
                            // The clipboard keyword switches into clipboard-only mode.
                            clip_mode_kc.set(kw.id == "clipboard");
                            mode_from_keybinding_kc.set(false);
                            chip_icon_kc.set_icon_name(Some(if kw.icon.is_empty() {
                                "folder-symbolic"
                            } else {
                                kw.icon.as_str()
                            }));
                            chip_label_kc.set_text(&crate::search::capitalize(&kw.word));
                            chip_kc.set_visible(true);
                            *mode_kc.borrow_mut() = Some(kw);
                            skip_ghost_clone.set(true);
                            ghost_active_kc.set(false);
                            typed_len_mode.set(0);
                            e.set_text("");
                            e.grab_focus();
                            crate::app::refresh_search_window();
                        };

                        // 1. If the typed text is exactly a trigger word, enter
                        //    that mode — but only when the user hasn't navigated
                        //    away from the top (recommended) row, otherwise Enter
                        //    should act on whatever row they selected.
                        let typed = current_typed(&e, &typed_len_c);
                        let entry_text = e.text().to_string();
                        let typed_trim = if typed.trim().is_empty() {
                            entry_text.trim()
                        } else {
                            typed.trim()
                        };
                        let at_top_selection =
                            l.selected_row().map(|row| row.index() == 0).unwrap_or(true);
                        if mode_kc.borrow().is_none() && !typed_trim.is_empty() && at_top_selection
                        {
                            let kw = cfg_kc.borrow().keyword_for_word(typed_trim);
                            if let Some(kw) = kw {
                                enter_mode(kw);
                                return glib::Propagation::Stop;
                            }
                        }

                        // 2. If the SELECTED result is a trigger suggestion
                        //    (EnterMode), enter that mode.
                        if mode_kc.borrow().is_none() {
                            if let Some(row) = l.selected_row() {
                                let kw_opt = {
                                    let rs = r.borrow();
                                    rs.get(row.index() as usize).and_then(|res| {
                                        if let Action::EnterMode(word) = &res.action {
                                            cfg_kc.borrow().keyword_for_word(word)
                                        } else {
                                            None
                                        }
                                    })
                                };
                                if let Some(kw) = kw_opt {
                                    enter_mode(kw);
                                    return glib::Propagation::Stop;
                                }
                            }
                        }

                        // 3. Otherwise: act on the selected row as usual.
                        if let Some(row) = l.selected_row() {
                            let rs = r.borrow();
                            if let Some(res) = rs.get(row.index() as usize) {
                                let query = current_typed(&e, &typed_len_c);
                                // A live install/uninstall progress row: Enter
                                // cancels the running operation, or — if it was
                                // already cancelled — restarts it from scratch.
                                if res.icon.as_deref() == Some("op-progress") {
                                    let typed = current_typed(&e, &typed_len_c);
                                    if typed.trim().is_empty() {
                                        return glib::Propagation::Stop;
                                    }
                                    if let Action::EnterMode(s) = &res.action {
                                        let parts: Vec<&str> = s.split('\u{1f}').collect();
                                        let state = parts.get(2).copied().unwrap_or("");
                                        if let Some(id) =
                                            parts.get(4).and_then(|p| p.parse::<u64>().ok())
                                        {
                                            if state == "cancelled" {
                                                crate::operations::restart(id);
                                            } else {
                                                crate::operations::cancel(id);
                                            }
                                        }
                                    }
                                    return glib::Propagation::Stop;
                                }
                                // In "app" mode, the Kill/Install/Uninstall
                                // templates aren't directly actionable — Enter
                                // autofills their action word into the entry so
                                // the user can type the app name next.
                                if matches!(res.action, Action::EnterMode(ref m) if m == "cmd")
                                    && matches!(
                                        res.title.as_str(),
                                        "Kill" | "Install" | "Uninstall"
                                    )
                                {
                                    let word = format!("{} ", res.title.to_lowercase());
                                    drop(rs);
                                    skip_ghost_clone.set(true);
                                    ghost_active_kc.set(false);
                                    let end = char_count(&word) as i32;
                                    suppress.set(true);
                                    e.set_text(&word);
                                    typed_len_c.set(char_count(&word));
                                    e.select_region(end, end);
                                    e.set_position(end);
                                    suppress.set(false);
                                    e.emit_by_name::<()>("changed", &[]);
                                    return glib::Propagation::Stop;
                                }
                                if let Action::StartOperation {
                                    title,
                                    source,
                                    icon,
                                    args,
                                } = &res.action
                                {
                                    let plan = crate::search::uninstall::Plan {
                                        title: title.clone(),
                                        source: source.clone(),
                                        icon: icon.clone(),
                                        args: args.clone(),
                                    };
                                    let question = match title.strip_prefix("Update: ") {
                                        Some(name) => {
                                            gettext("Do you want to update {name}?").replace("{name}", &name)
                                        }
                                        None if title.starts_with("Installing ") => {
                                            let name = title.strip_prefix("Installing ").unwrap();
                                            gettext("Are you sure you want to install {name}?").replace("{name}", &name)
                                        }
                                        None if title.starts_with("Uninstalling ") => {
                                            let name = title.strip_prefix("Uninstalling ").unwrap();
                                            gettext("Are you sure you want to uninstall {name}?").replace("{name}", &name)
                                        }
                                        None => format!(
                                            "Do you want to {}?",
                                            title.to_lowercase()
                                        ),
                                    };
                                    show_confirm_dialog(
                                        &w,
                                        &popover_open_kc,
                                        &e,
                                        question,
                                        Some(plan),
                                        "",
                                        query.clone(),
                                    );
                                    drop(rs);
                                    return glib::Propagation::Stop;
                                }
                                // Free-form "cmd" run: stream the command's
                                // output in-window (same as clicking the row).
                                if let Action::RunWithProgress { title, args } = &res.action {
                                    sp_fn_kc(title.clone(), args.clone());
                                    crate::history::record(&query, &res.title);
                                    return glib::Propagation::Stop;
                                }
                                if let Action::Bluetooth { op, mac } = &res.action {
                                    if op == "scan" {
                                        bt_scan_kc();
                                    } else {
                                        crate::search::bluetooth::stop_scan(crate::search::bluetooth::ScanStop::DeviceChosen);
                                        crate::search::bluetooth::run_action(op, mac, &res.title);
                                    }
                                    crate::history::record(&query, &res.title);
                                    return glib::Propagation::Stop;
                                }
                                if let Action::ConfirmRunCommand(cmd) = &res.action {
                                    popover_open_kc.set(true);
                                    let dialog = adw::MessageDialog::builder()
                                        .transient_for(&w)
                                        .heading(gettext("Are you sure?"))
                                        .build();
                                    dialog.add_response("cancel", &gettext("Cancel"));
                                    dialog.add_response("confirm", &gettext("Confirm"));
                                    dialog.set_default_response(Some("confirm"));
                                    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
                                    let w2 = w.clone();
                                    let s2 = shown_kc.clone();
                                    let po = popover_open_kc.clone();
                                    let c = cmd.clone();
                                    dialog.connect_response(None, move |_, resp| {
                                        po.set(false);
                            if resp == "confirm" {
                                            let _ = crate::app::spawn_host_shell_command(&c);
                                            dismiss(&w2, &s2, false);
                                        }
                                    });
                                    dialog.present();
                                    crate::history::record(&query, &res.title);
                                    return glib::Propagation::Stop;
                                }
                                if let Action::RemoveAppImage(path) = &res.action {
                                    // Local file delete: the same "moved to
                                    // the Trash" dialog Find mode uses for files.
                                    let name = crate::search::appimage::display_name(path);
                                    confirm_delete_path(&w, &popover_open_kc, &e, path, &name);
                                    crate::history::record(&query, &res.title);
                                    return glib::Propagation::Stop;
                                }
                                activate(res, &rs, &e, &w, &shown_kc);
                                crate::history::record(&query, &res.title);
                            }
                        }
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }
        window.add_controller(kc.clone());

        // ── Hide on focus loss ──
        // But not while a footer popover is open (it grabs input, dropping
        // :active) or while a command is running in the progress pane — in
        // those cases the focus change is internal, not the user dismissing
        // the window.
        #[allow(unused_assignments)] // the block below always fills it
        let mut is_active_h: Option<glib::SignalHandlerId> = None;
        {
            let w = window.clone();
            let popover_open = popover_open.clone();
            let progress_active = progress_active.clone();
            let shown = shown.clone();
            is_active_h = Some(window.connect_is_active_notify(move |win| {
                if shown.get() && !win.is_active() && !popover_open.get() && !progress_active.get()
                {
                    dismiss(&w, &shown, true);
                }
            }));
        }

        Self {
            window,
            window_kc: kc,
            entry_changed_h: std::cell::Cell::new(entry_changed_h),
            is_active_h: std::cell::Cell::new(is_active_h),
            entry,
            mode_chip,
            mode_icon,
            mode_label,
            active_mode,
            list,
            revealer,
            preview_box,
            preview,
            results,
            clipboard_mode,
            undo_stack,
            redo_stack,
            progress_active,
            cancel_progress_fn: cancel_progress,
            refresh_ops,
            mode_from_keybinding,
            shown,
            suppress_changed,
            ops_ring,
            busy_stack,
            busy_label,
            busy_revealer,
            gear_revealer: rev_gear,
            ops_pill_revealer,
            ops_pill_label,
            ops_pill_open,
            toast_gen: Rc::new(Cell::new(0)),
            update_badge,
            pre_show_reset,
        }
    }

    // ── Progress mode methods (delegate to shared Rc closures) ───────────────

    pub fn cancel_progress(&self) {
        (self.cancel_progress_fn)();
    }

    /// Show the window and grab entry focus. Fresh window → guaranteed focus
    /// on Wayland (re-showing a previously hidden window leaves it
    /// input-dead, so app.rs only ever calls this on a brand-new instance).
    /// Fades in from opacity 0 to make the appearance less abrupt.
    pub fn present_and_focus(&self) {
        if self.progress_active.get() {
            self.cancel_progress();
        }
        self.shown.set(true);
        // Canonical reset: clear results, normalize progress pane, set the
        // revealer with a fixed min-height so the compositor always places
        // the window at the same position.
        (self.pre_show_reset)();
        self.window.set_startup_id("");
        self.window.set_visible(true);
        // Paint-first: move focus to the entry before the frame paints so the
        // window is focused the instant it appears.  Heavy work (search, ops
        // indicator) is deferred to the first after_paint callback so the
        // compositor frame clock isn't stalled — critical for the Super+Space
        // video-pause race: if the window focuses before the Space key-release
        // is delivered, the release lands on Spotty (ignored) instead of the
        // focused browser (toggling video play/pause).
        self.entry.grab_focus();
        self.window.present();
        // The badge/banner must be right on the first open, before any
        // refresh_search_window has ever run (the update check may still be
        // in flight when the window first appears).
        self.sync_update_notification();
        // ponytail: one-shot frame clock probe — measures present-to-paint
        // latency AND runs the deferred post-present work on the first frame.
        if let Some(clock) = self.window.frame_clock() {
            let t0 = Instant::now();
            let signal_mark = crate::app::take_signal_mark();
            let fired = Rc::new(Cell::new(false));
            let fired_c = fired.clone();
            let entry = self.entry.clone();
            let ops_ring = self.ops_ring.clone();
            let suppress = self.suppress_changed.clone();
            let busy_stack = self.busy_stack.clone();
            let busy_revealer = self.busy_revealer.clone();
            clock.connect_after_paint(move |_clk| {
                if !fired_c.get() {
                    fired_c.set(true);
                    let paint_ms = t0.elapsed().as_millis();
                    if let Some((t_sig, majflt_before)) = signal_mark {
                        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
                        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru); }
                        let majflt = ru.ru_majflt;
                        log::info!("perf: sig→paint {}ms (majflt Δ{}), first frame {}ms after present",
                            t_sig.elapsed().as_millis(),
                            majflt - majflt_before,
                            paint_ms);
                    } else {
                        log::info!("perf: first frame after_paint in {}ms", paint_ms);
                    }
                    // Deferred: trigger the search (empty query → apps) and
                    // refresh the ops indicator.  These are cheap per-call but
                    // stall the frame clock when run before present().
                    suppress.set(true);
                    entry.set_text("");
                    suppress.set(false);
                    // Refresh ops indicator (updates ring state).
                    if !apply_ops_ring(&ops_ring, &busy_stack, &busy_revealer) {
                        set_revealed(&busy_revealer, false);
                    }
                    entry.emit_by_name::<()>("changed", &[]);
                }
            });
        }
        self.window.set_opacity(1.0);
        self.clipboard_mode.set(false);
        self.mode_from_keybinding.set(false);
        *self.active_mode.borrow_mut() = None;
        self.mode_chip.set_visible(false);
        self.mode_icon.set_icon_name(Some("folder-symbolic"));
        self.mode_label.set_text("");
        self.entry.set_placeholder_text(Some("Search"));
    }

    /// Open the window in clipboard-only search mode.
    pub fn present_clipboard_mode(&self) {
        self.present_keyword_mode(crate::config::CommandKeyword {
            id: "clipboard".into(),
            word: "clip".into(),
            description: gettext("Search clipboard history").into(),
            extensions: Vec::new(),
            icon: "edit-paste-symbolic".into(),
            all_files: false,
            shortcut: String::new(),
            enabled: true,
        });
    }

    /// Show the window in a trigger-keyword mode (mode chip + description in
    /// the entry placeholder). Same show/fade behavior as `present_and_focus`.
    pub fn present_keyword_mode(&self, keyword: crate::config::CommandKeyword) {
        self.shown.set(true);
        (self.pre_show_reset)();
        self.window.set_startup_id("");
        self.window.set_visible(true);
        // Paint-first: focus + present first, then heavy work in after_paint.
        self.entry.grab_focus();
        self.window.present();
        self.window.set_opacity(1.0);
        // Apply the same global appearance settings as regular search. This
        // path also opens every built-in and installed trigger mode.
        self.sync_update_notification();
        self.sync_footer();
        self.clipboard_mode.set(keyword.id == "clipboard");
        self.mode_from_keybinding.set(true);
        *self.active_mode.borrow_mut() = Some(keyword.clone());
        self.mode_icon
            .set_icon_name(Some(if keyword.icon.is_empty() {
                "folder-symbolic"
            } else {
                keyword.icon.as_str()
            }));
        self.mode_label
            .set_text(&crate::search::capitalize(&keyword.word));
        self.mode_chip.set_visible(true);
        self.entry.set_placeholder_text(Some(&keyword.description));
        // Deferred: run the keyword search after the first frame paints.
        if let Some(clock) = self.window.frame_clock() {
            let entry = self.entry.clone();
            let ops_ring = self.ops_ring.clone();
            let busy_stack = self.busy_stack.clone();
            let busy_revealer = self.busy_revealer.clone();
            let fired = Rc::new(Cell::new(false));
            let fired_c = fired.clone();
            clock.connect_after_paint(move |_clk| {
                if !fired_c.get() {
                    fired_c.set(true);
                    if !apply_ops_ring(&ops_ring, &busy_stack, &busy_revealer) {
                        set_revealed(&busy_revealer, false);
                    }
                    entry.emit_by_name::<()>("changed", &[]);
                }
            });
        }
    }

    /// Hide the window with the shared fade-out + unmap path. The unmapped
    /// window is dropped on next show (replaced by a fresh one); this instance
    /// is left for the caller's replacement to destroy.
    pub fn hide(&self) {
        dismiss(&self.window, &self.shown, true);
    }

    /// Tear this instance down for good — it's being replaced by a fresh
    /// window. The cycles have to be cut by hand: the key controller and
    /// in-tree closures hold clones of this window (and the window holds
    /// them), so `destroy` alone frees the surface but leaks the whole
    /// ~1 MB widget tree per show. Re-arming `shown` first tells an
    /// in-flight hide fade to stop touching the window on its next tick.
    pub fn destroy(&self) {
        self.shown.set(true);
        // Cut the cycles first: the changed closure holds the entry (and
        // through it list/results/preview — the whole ~1 MB graph), the
        // is-active closure holds the window itself.
        if let Some(h) = self.entry_changed_h.take() {
            self.entry.disconnect(h);
        }
        if let Some(h) = self.is_active_h.take() {
            self.window.disconnect(h);
        }
        // The preview pane's payload/staleness timers are infinite — stop
        // them or they'd outlive this window and pin its graph forever.
        self.preview.stop_polling();
        self.window.remove_controller(&self.window_kc);
        self.window.set_child(None::<&gtk::Box>);
        self.window.destroy();
    }
    /// Toggle source of truth: `shown` flag, NOT GTK visibility — GTK stays
    /// "visible" during the fade-out (see [`dismiss`]).
    pub fn is_visible(&self) -> bool {
        self.shown.get()
    }
    pub fn refresh_results(&self) {
        if self.shown.get() {
            let entry = self.entry.clone();
            glib::idle_add_local_once(move || entry.emit_by_name::<()>("changed", &[]));
        }
        // If the Operations popover is open, rebuild it so music card
        // changes (play/stop/undo) appear immediately.
        (self.refresh_ops)();
    }

    /// Let a preview showing a since-modified file re-render (live edit
    /// tracking — the pane also runs its own 1 s staleness check).
    pub fn refresh_preview_if_stale(&self) {
        self.preview.refresh_if_stale();
    }

    /// Keep the inline orb pill in sync with the orb state (text + collapse).
    fn update_ops_pill(&self) {
        sync_ops_pill(
            &self.ops_pill_open,
            &self.ops_pill_revealer,
            &self.ops_pill_label,
            &self.busy_revealer,
            &self.busy_stack,
            self.ops_ring.area(),
        );
    }

    /// Recompute the busy indicator (ops ring / BT status / content search /
    /// hidden), then re-sync the inline pill around it. The gear is purely
    // hover-driven and doesn't depend on the orb.
    pub fn refresh_ops_indicator(&self) {
        self.sync_orb_indicator();
        self.update_ops_pill();
        self.sync_update_notification();
    }

    /// Refresh the update badge (orb slot) and the notification banner.
    /// Runs from `refresh_ops_indicator`, i.e. on every window refresh.
    /// Refresh the update badge in the orb slot. Runs from
    /// `refresh_ops_indicator` (every window refresh) and on first show.
    /// No expansion under the search bar — typing "update" reveals the
    /// options, per design.
    fn sync_update_notification(&self) {
        let notice =
            crate::app::with_state(|st| crate::search::cmd::update_notice(&st.config.borrow()));
        let reboot = crate::search::cmd::reboot_pending();

        // The badge shares the orb slot — only when the orb itself is idle.
        let orb_busy = self.busy_revealer.reveals_child();
        self.update_badge
            .set_visible(!orb_busy && (notice.is_some() || reboot));
        if reboot {
            let (title, _) = crate::search::cmd::reboot_notice();
            self.update_badge.set_tooltip_text(Some(&title));
        } else if let Some((n, _)) = &notice {
            let tip = if *n == 1 {
                gettext("One update available")
            } else {
                gettext("{n} updates available").replace("{n}", &n.to_string())
            };
            self.update_badge.set_tooltip_text(Some(&tip));
        } else {
            self.update_badge.set_tooltip_text(None);
        }
    }

    fn sync_orb_indicator(&self) {
        // While the orb sits next to typed text it is drawn in the text
        // colour (black on a light theme, white on a dark one); otherwise
        // it keeps the accent palette.
        self.ops_ring.set_tone(if self.entry.text().is_empty() {
            crate::ui::circular_progress::Tone::Accent
        } else {
            crate::ui::circular_progress::Tone::Foreground
        });
        // Priority: ops > finished-op flash > bt action > bt scan > find-mode > hidden.
        if apply_ops_ring(&self.ops_ring, &self.busy_stack, &self.busy_revealer) {
            return;
        }
        if crate::search::bluetooth::is_busy() {
            self.ops_ring.set(
                None,
                crate::ui::circular_progress::RingState::Running,
            );
            self.ops_ring
                .area()
                .set_tooltip_text(Some(crate::search::bluetooth::busy_label().as_str()));
            self.reveal_orb();
            return;
        }
        if crate::search::bluetooth::is_scanning() {
            self.ops_ring.set(
                None,
                crate::ui::circular_progress::RingState::Running,
            );
            self.ops_ring
                .area()
                .set_tooltip_text(Some("Scanning for Bluetooth devices…"));
            self.reveal_orb();
            return;
        }
        let is_find_busy = self
            .active_mode
            .borrow()
            .as_ref()
            .is_some_and(|kw| kw.all_files)
            && self.entry.text().chars().count() >= 3
            && crate::search::files::is_content_search_inflight(
                &self.entry.text().to_string(),
            );
        if is_find_busy {
            self.ops_ring.set(
                None,
                crate::ui::circular_progress::RingState::Running,
            );
            self.ops_ring
                .area()
                .set_tooltip_text(Some("Searching file contents…"));
            self.reveal_orb();
            return;
        }
        // Idle: hide the orb (but not if a toast is still fading out).
        if self.toast_gen.get() == 0 {
            self.hide_orb();
        }
    }

    /// Show the busy stack with the orb child and reveal it.
    fn reveal_orb(&self) {
        self.busy_stack.set_visible_child_name("orb");
        set_revealed(&self.busy_revealer, true);
    }

    /// Hide the busy revealer (the stack reverts to "orb" state next time
    /// it's revealed). The process pill goes with it; the gear is hover-
    /// driven and stays wherever the pointer left it.
    fn hide_orb(&self) {
        set_revealed(&self.busy_revealer, false);
        self.ops_pill_open.set(false);
        set_revealed(&self.ops_pill_revealer, false);
    }

    /// If a Bluetooth action just completed, show its result as a toast
    /// replacing the orb for ~4 s (like Android/Windows toasts).
    pub fn refresh_bt_toast(&self) {
        let Some((text, ok)) = crate::search::bluetooth::take_result() else {
            return;
        };
        self.busy_label.set_text(&text);
        self.busy_stack.set_visible_child_name("status");
        // The ring is gone for the toast — the pill (clicked open against
        // the ring) collapses with it.
        self.ops_pill_open.set(false);
        set_revealed(&self.ops_pill_revealer, false);
        self.reveal_orb();
        let gen = self.toast_gen.get().wrapping_add(1);
        self.toast_gen.set(gen);
        let gen_c = self.toast_gen.clone();
        let revealer = self.busy_revealer.clone();
        glib::timeout_add_local(Duration::from_millis(4000), move || {
            if gen_c.get() == gen {
                set_revealed(&revealer, false);
            }
            glib::ControlFlow::Break
        });
        // If the user types or interacts, the next refresh_ops_indicator will
        // hide the toast (gen check ensures stale timers don't clobber).
    }
}

/// Toggle a revealer while keeping a fully-closed one out of the layout: a
/// closed GtkRevealer still counts as a visible child, so its Box spacing
/// would push the idle gear away from the bar's right edge. Opening brings
/// the widget back before the transition; closing hides it via the
/// child-revealed notify hook once the transition has finished.
fn set_revealed(revealer: &gtk::Revealer, on: bool) {
    if on {
        revealer.set_visible(true);
    }
    revealer.set_reveal_child(on);
}

/// Show/hide the inline process pill next to the orb. The open flag is a
/// toggle from clicking the orb; the text is the running operation's title
/// and live status, falling back to the orb's own state text (Bluetooth /
/// content search). Collapses whenever the ring isn't the visible state.
fn sync_ops_pill(
    open: &Rc<Cell<bool>>,
    revealer: &gtk::Revealer,
    label: &gtk::Label,
    busy_revealer: &gtk::Revealer,
    busy_stack: &gtk::Stack,
    ring_area: &gtk::DrawingArea,
) {
    if !open.get() {
        set_revealed(revealer, false);
        return;
    }
    let ring_visible = busy_revealer.reveals_child()
        && busy_stack.visible_child_name().is_some_and(|n| n == "orb");
    if !ring_visible {
        open.set(false);
        set_revealed(revealer, false);
        return;
    }
    let text = match crate::operations::active_op_detail() {
        Some((title, detail)) if detail.is_empty() => title,
        Some((title, detail)) => gettext("{title} — {detail}").replace("{title}", &title).replace("{detail}", &detail),
        None => ring_area
            .tooltip_text()
            .map(|t| t.to_string())
            .unwrap_or_default(),
    };
    label.set_text(&text);
    set_revealed(revealer, true);
}

/// Shared "operations → search-bar orb" update. Returns `true` when the orb
/// was driven (an operation is running, or one just finished and is still in
/// its `DONE_GRACE` linger) and `false` when the caller should fall back to
/// the Bluetooth / content-search / idle states.
///
/// Used by `refresh_ops_indicator` and by the deferred after-paint handlers,
/// which clone the widgets instead of borrowing the window and therefore
/// can't call into `self`.
fn apply_ops_ring(
    ops_ring: &crate::ui::circular_progress::Ring,
    busy_stack: &gtk::Stack,
    busy_revealer: &gtk::Revealer,
) -> bool {
    let (title, fraction, state) = match crate::operations::active_op_progress() {
        Some((title, fraction)) => (
            title,
            fraction,
            crate::ui::circular_progress::RingState::Running,
        ),
        // Just finished: flash a full ring (green/red) through the same
        // DONE_GRACE window the popover uses, so "100% completed" is visible
        // in the bar before the orb disappears.
        None => match crate::operations::just_finished_op() {
            Some((title, failed)) => (
                title,
                Some(1.0),
                if failed {
                    crate::ui::circular_progress::RingState::Failed
                } else {
                    crate::ui::circular_progress::RingState::Done
                },
            ),
            None => return false,
        },
    };
    ops_ring.set(fraction, state);
    ops_ring.area().set_tooltip_text(Some(&title));
    busy_stack.set_visible_child_name("orb");
    set_revealed(busy_revealer, true);
    true
}

/// Shared dismiss path: fade out (~120ms) then unmap, or unmap instantly for
/// action launches. The `shown` flag makes a spammed second dismiss a no-op
/// and lets a concurrent show cancel the fade mid-way.
fn dismiss(window: &gtk::Window, shown: &Rc<Cell<bool>>, animate: bool) {
    // Stop Bluetooth scan on hide (cancel = restore power if it was off).
    crate::search::bluetooth::stop_scan(crate::search::bluetooth::ScanStop::Cancelled);
    if !shown.get() {
        return;
    }
    if !animate {
        shown.set(false);
        window.set_visible(false);
        return;
    }
    shown.set(false);
    let win = window.clone();
    let shown = shown.clone();
    glib::timeout_add_local(
        std::time::Duration::from_millis(16),
        move || {
            if shown.get() {
                return glib::ControlFlow::Break;
            }
            let next = win.opacity() - 0.125;
            if next <= 0.0 {
                win.set_opacity(0.0);
                win.set_visible(false);
                glib::ControlFlow::Break
            } else {
                win.set_opacity(next);
                glib::ControlFlow::Continue
            }
        },
    );
}

// ──────────────────────────────────────────────────────────────────────
// Ghost-text completion
// ──────────────────────────────────────────────────────────────────────
//
// The entry's text contains:  <user-typed> <ghost-suggestion>
// where <ghost-suggestion> is highlighted via Entry's text selection.
// When the user types more, GTK replaces the selection automatically.
// When the user presses Tab, we collapse the selection (cursor to end),
// effectively "accepting" the ghost.

#[derive(Clone, Copy)]
enum PinKind {
    Text,
    Image,
    File,
}

// Map a clipboard-result action to its pin identity (which config list it
// belongs to, and the key used to find it in that list).
fn pin_info_for(action: &Action) -> Option<(PinKind, String)> {
    match action {
        Action::CopyToClipboard(t) => Some((PinKind::Text, t.clone())),
        Action::CopyImageToClipboard(p) => Some((PinKind::Image, p.display().to_string())),
        Action::CopyFileToClipboard(p) => Some((PinKind::File, p.display().to_string())),
        _ => None,
    }
}

fn is_pinned(cfg: &Config, kind: PinKind, key: &str) -> bool {
    match kind {
        PinKind::Text => cfg.pinned_clipboard.iter().any(|p| p == key),
        PinKind::Image => cfg.pinned_clipboard_images.iter().any(|p| p == key),
        PinKind::File => cfg.pinned_clipboard_files.iter().any(|p| p == key),
    }
}

// Human-readable label for an accelerator string, e.g. "<Control>p" -> "Ctrl+P".
fn pin_shortcut_label(accel: &str) -> String {
    match gtk::accelerator_parse(accel) {
        Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
        None => String::new(),
    }
}

// Toggle the pinned state of a clipboard entry, saving the config.
fn is_clipboard_item_pinned(cfg: &Config, action: &Action) -> bool {
    if let Some((kind, key)) = pin_info_for(action) {
        let list = match kind {
            PinKind::Text => &cfg.pinned_clipboard,
            PinKind::Image => &cfg.pinned_clipboard_images,
            PinKind::File => &cfg.pinned_clipboard_files,
        };
        list.iter().any(|p| p == &key)
    } else {
        false
    }
}

fn toggle_clipboard_pin(
    cfg: &Rc<RefCell<Config>>,
    action: &Action,
    clipboard: &Rc<RefCell<crate::clipboard::ClipboardHistory>>,
) {
    if let Some((kind, key)) = pin_info_for(action) {
        let _was_pinned = {
            let c = cfg.borrow();
            let list = match kind {
                PinKind::Text => &c.pinned_clipboard,
                PinKind::Image => &c.pinned_clipboard_images,
                PinKind::File => &c.pinned_clipboard_files,
            };
            list.iter().any(|p| p == &key)
        };

        let mut c = cfg.borrow_mut();
        let list = match kind {
            PinKind::Text => &mut c.pinned_clipboard,
            PinKind::Image => &mut c.pinned_clipboard_images,
            PinKind::File => &mut c.pinned_clipboard_files,
        };

        if let Some(pos) = list.iter().position(|p| p == &key) {
            list.remove(pos);
            // Record the unpin for undo
            let kind_enum = match kind {
                PinKind::Text => crate::clipboard::ClipboardEntryKind::Text,
                PinKind::Image => crate::clipboard::ClipboardEntryKind::Image,
                PinKind::File => crate::clipboard::ClipboardEntryKind::File,
            };
            clipboard
                .borrow_mut()
                .record_unpinned(kind_enum, key.clone());
        } else {
            list.insert(0, key);
        }
        c.save();
    }
}

// Which pinning system a row's pin button should use.
#[derive(Clone)]
enum PinMode {
    /// Clipboard-manager entries use the dedicated clipboard pin lists.
    Clipboard(PinKind, String),
    /// Any other result type uses the universal `pinned_results` list.
    Universal,
}

// A small centered "<Title>?" confirmation dialog with Yes/No buttons,
// styled to match the main Spotty window (rounded corners, libadwaita
// theming). On "Yes", starts `plan` as a background operation; if `plan` is
// `None`, shows `fail_message` as a desktop notification instead.
fn show_confirm_dialog(
    window: &gtk::Window,
    popover_open: &Rc<Cell<bool>>,
    entry: &gtk::Entry,
    title: String,
    plan: Option<crate::search::uninstall::Plan>,
    fail_message: &'static str,
    history_query: String,
) {
    // Block the focus-loss auto-hide while the confirmation dialog is up.
    popover_open.set(true);

    let dialog = gtk::Window::builder()
        .transient_for(window)
        .modal(true)
        .resizable(false)
        .decorated(false)
        .default_width(260)
        .build();
    dialog.add_css_class("spotty-window");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(24)
        .margin_end(24)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(title)
            .css_classes(["title-3"])
            .halign(gtk::Align::Center)
            .build(),
    );
    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    let no_btn = gtk::Button::builder()
        .label(gettext("No"))
        .css_classes(["spotty-confirm-btn"])
        .build();
    let yes_btn = gtk::Button::builder()
        .label(gettext("Yes"))
        .css_classes(["suggested-action", "spotty-confirm-btn"])
        .build();
    btn_row.append(&yes_btn);
    btn_row.append(&no_btn);
    content.append(&btn_row);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&yes_btn));

    let popover_open2 = popover_open.clone();
    let finish = {
        let dialog = dialog.clone();
        let entry = entry.clone();
        let query = history_query;
        move |confirmed: bool, plan: Option<crate::search::uninstall::Plan>| {
            popover_open2.set(false);
            dialog.close();
            if confirmed {
                if let Some(plan) = plan {
                    crate::operations::start(plan.title.clone(), plan.source, plan.icon, plan.args);
                    if !query.is_empty() {
                        crate::history::record(&query, &plan.title);
                    }
                    entry.emit_by_name::<()>("changed", &[]);
                } else {
                    crate::app::send_desktop_notification("Spotty", fail_message);
                }
            }
            entry.grab_focus();
        }
    };
    {
        let finish = finish.clone();
        let plan = plan.clone();
        yes_btn.connect_clicked(move |_| finish(true, plan.clone()));
    }
    {
        let finish = finish.clone();
        no_btn.connect_clicked(move |_| finish(false, None));
    }
    {
        let finish = finish.clone();
        let kc = gtk::EventControllerKey::new();
        kc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                finish(false, None);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(kc);
    }
    dialog.present();
    yes_btn.grab_focus();
}

/// Find-mode delete (Ctrl+D): confirmation popup asking
/// `Do you want to delete "name"?` whose default answer is No (focused
/// button, default widget, and Escape all cancel). On confirm the item goes
/// to the Trash — never a permanent delete — and the search re-runs; failures
/// surface as a desktop notification, like the app's other confirm dialogs.
/// Drop a trashed (or already gone) path — and everything indexed under
/// it — from the search index, so its row disappears immediately. The
/// change watcher only re-indexes the priority folders, so locations like
/// ~/.config would keep a stale row (and fail on every further delete).
fn forget_indexed(path: &std::path::Path) {
    crate::app::with_state(|st| {
        let snap = st.indexer.snapshot();
        {
            let mut s = snap.write().unwrap();
            crate::index::prune_indexed_path(&mut s, path);
        }
        crate::index::persist_files_async(&snap);
    });
}

fn confirm_delete_path(
    window: &gtk::Window,
    popover_open: &Rc<Cell<bool>>,
    entry: &gtk::Entry,
    path: &std::path::Path,
    name: &str,
) {
    // Block the focus-loss auto-hide while the dialog is up.
    popover_open.set(true);

    let dialog = gtk::Window::builder()
        .transient_for(window)
        .modal(true)
        .resizable(false)
        .decorated(false)
        .default_width(340)
        .build();
    dialog.add_css_class("spotty-window");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(24)
        .margin_end(24)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(gettext("Do you want to delete \"{name}\"?").replace("{name}", &name))
            .css_classes(["title-3"])
            .halign(gtk::Align::Center)
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(gettext("It will be moved to the Trash."))
            .css_classes(["dim-label", "caption"])
            .halign(gtk::Align::Center)
            .build(),
    );
    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .margin_top(8)
        .build();
    let delete_btn = gtk::Button::builder()
        .label(gettext("Delete"))
        .css_classes(["destructive-action", "spotty-confirm-btn"])
        .build();
    let no_btn = gtk::Button::builder()
        .label(gettext("No"))
        .css_classes(["spotty-confirm-btn"])
        .build();
    btn_row.append(&delete_btn);
    btn_row.append(&no_btn);
    content.append(&btn_row);
    dialog.set_child(Some(&content));
    // Default answer is No: it owns the default widget and the initial focus.
    dialog.set_default_widget(Some(&no_btn));

    let popover_open2 = popover_open.clone();
    let finish = {
        let dialog = dialog.clone();
        let entry = entry.clone();
        let path = path.to_path_buf();
        let name = name.to_string();
        move |confirmed: bool| {
            popover_open2.set(false);
            dialog.close();
            if confirmed {
                match crate::fileops::trash_path(&path) {
                    Ok(()) => {
                        log::info!("fileops: trashed {}", path.display());
                        forget_indexed(&path);
                        // Re-run the search so the row disappears right away.
                        entry.emit_by_name::<()>("changed", &[]);
                    }
                    // Already gone: a stale row an earlier delete left
                    // behind (the watcher never re-indexes non-priority
                    // folders). Drop it instead of failing again.
                    Err(e) if !path.exists() => {
                        log::info!(
                            "fileops: {} already gone ({e}), dropping the stale row",
                            path.display()
                        );
                        forget_indexed(&path);
                        entry.emit_by_name::<()>("changed", &[]);
                    }
                    Err(e) => {
                        log::warn!("fileops: trash failed for {}: {e}", path.display());
                        crate::app::send_desktop_notification(
                            "Spotty",
                            &gettext("Couldn't delete \"{name}\": {e}").replace("{name}", &name).replace("{e}", &e),
                        );
                    }
                }
            }
            entry.grab_focus();
        }
    };
    {
        let finish = finish.clone();
        delete_btn.connect_clicked(move |_| finish(true));
    }
    {
        let finish = finish.clone();
        no_btn.connect_clicked(move |_| finish(false));
    }
    {
        let finish = finish.clone();
        let kc = gtk::EventControllerKey::new();
        kc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                finish(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(kc);
    }
    dialog.present();
    no_btn.grab_focus();
}

// Whether a result is eligible for universal pinning: real, static results
// (apps, files, folders, web searches, system actions) — not trigger
// suggestions, calculator results, or running-operation rows.
/// Read a configurable in-window shortcut for display, falling back to the
/// given default (GTK accelerator string) when the field is empty.
fn accel_or(config: &Rc<RefCell<Config>>, field: fn(&Config) -> &str, default: &str) -> String {
    let s = {
        let cfg = config.borrow();
        field(&cfg).to_string()
    };
    if s.is_empty() {
        default.to_string()
    } else {
        s
    }
}

fn to_gtk_accel(s: &str) -> String {
    let mut out = String::new();
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" | "primary" => out.push_str("<Control>"),
            "super" | "meta" | "win" | "logo" => out.push_str("<Super>"),
            "shift" => out.push_str("<Shift>"),
            "alt" => out.push_str("<Alt>"),
            _ => out.push_str(part),
        }
    }
    out
}

fn universal_pin_eligible(r: &SearchResult) -> bool {
    use crate::search::ResultKind::*;
    if matches!(
        r.icon.as_deref(),
        Some("op-progress") | Some("emblem-synchronizing-symbolic")
    ) {
        return false;
    }
    if !matches!(r.kind, App | File | Folder | Web | System) {
        return false;
    }
    !matches!(
        r.action,
        Action::EnterMode(_)
            | Action::StartOperation { .. }
            | Action::RunWithProgress { .. }
            | Action::InsertCalculatorResult(_)
            | Action::Bluetooth { .. }
            | Action::ConfirmRunCommand(_)
            | Action::ToggleUpdates
            | Action::SnoozeUpdates
            | Action::DismissUpdates(_)
            | Action::CheckUpdates
            | Action::Noop
    )
}

// Toggle a result's pinned state in the universal `pinned_results` list.
fn toggle_universal_pin(cfg: &Rc<RefCell<Config>>, r: &SearchResult) {
    let mut c = cfg.borrow_mut();
    if let Some(pos) = c.pinned_results.iter().position(|p| p.action == r.action) {
        c.pinned_results.remove(pos);
    } else {
        let mut pinned = r.clone();
        pinned.score = 0;
        c.pinned_results.insert(0, pinned);
        c.pinned_results.truncate(50);
    }
    c.save();
}

// Briefly flash a button to give visual feedback for a click (pin/unpin/delete).
fn pulse(btn: &gtk::Button) {
    btn.add_css_class("action-pulse");
    let btn = btn.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
        btn.remove_css_class("action-pulse");
    });
}

fn current_typed(entry: &gtk::Entry, typed_len: &Cell<usize>) -> String {
    let text = entry.text().to_string();
    text.chars()
        .take(logical_typed_chars(entry, typed_len))
        .collect()
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

fn visible_typed_len(entry: &gtk::Entry, fallback: usize) -> usize {
    let text = entry.text().to_string();
    let text_len = char_count(&text);
    if let Some((sel_start, sel_end)) = entry.selection_bounds() {
        let sel_start = sel_start as usize;
        let sel_end = sel_end as usize;
        let tail_selection = sel_end == text_len && sel_start <= sel_end;
        if tail_selection && sel_start >= fallback.min(text_len) {
            return sel_start;
        }
    }
    let pos = entry.position().max(0) as usize;
    if pos < text_len && pos >= fallback.min(text_len) {
        return pos;
    }
    text_len
}

fn logical_typed_chars(entry: &gtk::Entry, typed_len: &Cell<usize>) -> usize {
    let full = entry.text().to_string();
    let full_chars = char_count(&full);
    if let Some((sel_start, sel_end)) = entry.selection_bounds() {
        if sel_end > sel_start && sel_end as usize == full_chars {
            return sel_start as usize;
        }
    }
    typed_len.get().min(full_chars)
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn push_undo_snapshot(stack: &Rc<RefCell<Vec<String>>>, text: &str) {
    let mut s = stack.borrow_mut();
    if s.last().map(|v| v.as_str()) != Some(text) {
        s.push(text.to_string());
        if s.len() > 200 {
            s.remove(0);
        }
    }
}

fn char_class(c: Option<char>) -> u8 {
    match c {
        None => 0,
        Some(ch) if ch.is_whitespace() => 1,
        Some(ch) if ch.is_alphanumeric() || ch == '_' => 2,
        Some(_) => 3,
    }
}

fn real_typed_text(entry: &gtk::Entry, typed_len: &Cell<usize>) -> String {
    current_typed(entry, typed_len)
}

// Cmd-mode template subtitles look like "kill <app-name>  —  ..." or
// "Type: install <app-name>  —  ...". For these, ghost-complete only the
// leading verb (e.g. "kill", "install") so the user types the app name
// themselves, instead of completing to the whole result title.
fn ghost_word_from_subtitle(subtitle: &str) -> Option<String> {
    let tokens: Vec<&str> = subtitle.split_whitespace().collect();
    tokens
        .windows(2)
        .find(|w| w[1].starts_with('<') && w[0].chars().all(|c| c.is_ascii_lowercase()))
        .map(|w| w[0].to_string())
}

fn candidate_for(res: &SearchResult, user_text: &str) -> Option<String> {
    let in_path_mode = user_text.starts_with('/') || user_text.starts_with('~');
    // Cmd-mode verb words: while the user is still typing "kill"/"install"/
    // "uninstall" itself (no space typed yet), always ghost-complete to the
    // full verb plus a trailing space — regardless of which result happens to
    // be selected (e.g. typing "k" already matches the "kill" alias and shows
    // running-app rows, but Tab should land on "kill " ready to pick one).
    if !in_path_mode && !user_text.contains(' ') {
        let ul = user_text.to_lowercase();
        for verb in ["kill", "install", "uninstall"] {
            if verb.starts_with(&ul) && ul != verb {
                return Some(format!("{} ", verb));
            }
        }
    }
    if in_path_mode {
        match &res.action {
            Action::BrowseInto(p) => {
                let mut s = p.display().to_string();
                if !s.ends_with('/') {
                    s.push('/');
                }
                Some(s)
            }
            Action::OpenPath(p) | Action::OpenInFileManager(p) => Some(p.display().to_string()),
            _ => None,
        }
    } else if res.kind == crate::search::ResultKind::System {
        // Update rows complete to the exact query they run ("update all",
        // "update flatpak", "update firefox") so the ghost recommends the
        // next token; while the verb itself is still incomplete ("updat")
        // they complete it to "update " first.
        let update_row = crate::search::cmd::query_for(&res.title).is_some()
            || matches!(res.action, Action::CheckUpdates | Action::ToggleUpdates);
        if update_row {
            if let Some(v) = crate::search::cmd::verb_completion(user_text) {
                return Some(v);
            }
            if let Some(q) = crate::search::cmd::query_for(&res.title) {
                return Some(q);
            }
        }
        if let Some(word) = res.subtitle.as_deref().and_then(ghost_word_from_subtitle) {
            return Some(word);
        }
        // "Install: Firefox" / "Uninstall: Firefox" / "Kill: Firefox" results —
        // once the verb is placed (user typed "install "), ghost-complete
        // "<verb> <app name>" so only the app name needs typing. If the verb
        // hasn't been placed yet, don't ghost-complete to the raw title at all
        // (falling through to `res.title.clone()` below would splice
        // "Kill: Firefox" onto whatever verb the user is still typing).
        for verb in ["Install", "Uninstall", "Kill"] {
            if let Some(name) = res.title.strip_prefix(&format!("{}: ", verb)) {
                let prefix = format!("{} ", verb.to_lowercase());
                if user_text.to_lowercase().starts_with(&prefix) {
                    return Some(format!("{}{}", prefix, name));
                }
                return None;
            }
        }
        Some(res.title.clone())
    } else {
        Some(res.title.clone())
    }
}

fn apply_ghost(
    entry: &gtk::Entry,
    res: &SearchResult,
    user_text: &str,
    typed_len: &Cell<usize>,
    suppress: &Cell<bool>,
    prev_text_len: &Cell<usize>,
    ghost_active: &Cell<bool>,
) {
    if user_text.is_empty() {
        return;
    }
    // Guard: only apply ghost when the entry text still matches what was
    // searched for. If the user typed more while a search was in flight,
    // applying would clobber those characters.
    {
        let current: String = entry.text().chars().take(typed_len.get()).collect();
        if current != user_text {
            return;
        }
    }

    // Use the same candidate logic as selection/Tab so the ghost, the selected
    // row, and Tab-acceptance all agree (paths, app names, and cmd verb words).
    let candidate = match candidate_for(res, user_text) {
        Some(c) => c,
        None => return,
    };

    let user_lower = user_text.to_lowercase();
    let cand_lower = candidate.to_lowercase();

    // Only prefix-style completion
    if !cand_lower.starts_with(&user_lower) {
        return;
    }
    if char_count(&candidate) <= char_count(user_text) {
        return;
    }

    // Build full text = exactly what user typed (preserving case)
    // + remainder from candidate (in candidate's original case)
    let remainder = &candidate[user_text.len()..];
    let full = format!("{}{}", user_text, remainder);
    let remainder_start = char_count(user_text);

    suppress.set(true);
    entry.set_text(&full);
    entry.select_region(remainder_start as i32, char_count(&full) as i32);
    typed_len.set(remainder_start);
    // Record the actual on-screen text length so subsequent backspace detection works
    prev_text_len.set(char_count(&full));
    ghost_active.set(true);
    suppress.set(false);
}

fn accept_ghost(
    entry: &gtk::Entry,
    typed_len: &Cell<usize>,
    suppress: &Cell<bool>,
    selected_action: Option<&Action>,
) {
    let current = entry.text().to_string();
    if current.is_empty() {
        return;
    }
    // For folder results: replace title with the FULL PATH + trailing slash
    // so the next search shows folder contents (path-browsing mode kicks in).
    let new_text = if let Some(Action::BrowseInto(p)) = selected_action {
        let mut s = p.display().to_string();
        if !s.ends_with('/') {
            s.push('/');
        }
        s
    } else if let Some(Action::OpenPath(p)) | Some(Action::OpenInFileManager(p)) = selected_action {
        p.display().to_string()
    } else {
        current.clone()
    };

    // Cmd-mode verb words ("kill", "install", "uninstall") complete on their
    // own — immediately append a space so the result list switches straight
    // to suggestions (running apps / install candidates) without requiring
    // the user to press space themselves.
    let new_text = if matches!(
        new_text.to_lowercase().as_str(),
        "kill" | "install" | "uninstall"
    ) && !new_text.ends_with(' ')
    {
        format!("{} ", new_text)
    } else {
        new_text
    };

    let end = char_count(&new_text) as i32;
    suppress.set(true);
    entry.set_text(&new_text);
    typed_len.set(char_count(&new_text));
    entry.select_region(end, end);
    entry.set_position(end);
    suppress.set(false);
    entry.emit_by_name::<()>("changed", &[]);
}

fn move_sel(l: &gtk::ListBox, delta: i32, scroll: &gtk::ScrolledWindow) {
    let mut n = 0i32;
    let mut c = l.first_child();
    while let Some(ch) = c {
        n += 1;
        c = ch.next_sibling();
    }
    if n == 0 {
        return;
    }
    let cur = l.selected_row().map(|r| r.index()).unwrap_or(-1);
    if let Some(row) = l.row_at_index((cur + delta).rem_euclid(n)) {
        l.select_row(Some(&row));
        scroll_row_into_view(&row, scroll);
    }
}

/// Scroll `scroll` just enough to bring `row` fully into view, since the
/// results list hides its scrollbar but is still scrollable (wheel, touch,
/// and arrow keys).
fn scroll_row_into_view(row: &gtk::ListBoxRow, scroll: &gtk::ScrolledWindow) {
    use gtk::prelude::WidgetExt;
    let Some(bounds) = row.compute_bounds(scroll) else {
        return;
    };
    let vadj = scroll.vadjustment();
    // `bounds` is relative to the visible viewport (0..page_size), not the
    // scrolled content, so adjust the current scroll position by the
    // overflow rather than treating these as absolute content coordinates.
    let row_top = bounds.y() as f64;
    let row_bottom = row_top + bounds.height() as f64;
    let page_size = vadj.page_size();
    if row_top < 0.0 {
        vadj.set_value(vadj.value() + row_top);
    } else if row_bottom > page_size {
        vadj.set_value(vadj.value() + (row_bottom - page_size));
    }
}

fn attach_two_way_swipe(
    area: &impl IsA<gtk::Widget>,
    feedback: &impl IsA<gtk::Widget>,
    dismiss_dir: f64,
    on_reverse: Rc<dyn Fn()>,
    on_forward: Rc<dyn Fn()>,
) {
    let cls = format!("swrow{}", SWIPE_ROW_CTR.fetch_add(1, Ordering::Relaxed));
    feedback.add_css_class(&cls);
    let set_css = move |tx: f64, transition: &str| swipe_apply(&cls, tx, transition);
    let committed = Rc::new(Cell::new(false));

    let commit: Rc<dyn Fn(f64)> = {
        let set_css = set_css.clone();
        let committed = committed.clone();
        let on_reverse = on_reverse.clone();
        let on_forward = on_forward.clone();
        Rc::new(move |offset: f64| {
            if committed.get() {
                return;
            }
            committed.set(true);
            let target = if offset >= 0.0 {
                SWIPE_SETTLE_PX
            } else {
                -SWIPE_SETTLE_PX
            };
            set_css(target, "transform 300ms ease-out");
            let reverse = offset * dismiss_dir < 0.0;
            let on_reverse = on_reverse.clone();
            let on_forward = on_forward.clone();
            glib::timeout_add_local_once(Duration::from_millis(310), move || {
                if reverse {
                    on_reverse();
                } else {
                    on_forward();
                }
            });
        })
    };

    let scroll_sign: f64 = if touchpad_natural_scroll() { -1.0 } else { 1.0 };

    let drag = gtk::GestureDrag::new();
    drag.set_touch_only(false);
    drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
    {
        let set_css = set_css.clone();
        let committed = committed.clone();
        drag.connect_drag_update(move |_, dx, _| {
            if committed.get() {
                return;
            }
            set_css(dx, "");
        });
    }
    {
        let set_css = set_css.clone();
        let commit = commit.clone();
        let committed = committed.clone();
        drag.connect_drag_end(move |_, dx, _| {
            if committed.get() {
                return;
            }
            if dx.abs() >= SWIPE_COMMIT_PX {
                commit(dx);
            } else {
                set_css(0.0, SWIPE_SPRING);
            }
        });
    }
    area.add_controller(drag);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let acc = Rc::new(Cell::new(0.0_f64));
    {
        let set_css = set_css.clone();
        let acc = acc.clone();
        let commit = commit.clone();
        let committed = committed.clone();
        scroll.connect_scroll(move |_, dx, _| {
            if committed.get() {
                return glib::Propagation::Stop;
            }
            if dx.abs() < 0.005 {
                return glib::Propagation::Proceed;
            }
            if let Some(fling) = scroll_fling_offset(dx * scroll_sign) {
                acc.set(0.0);
                commit(fling);
                return glib::Propagation::Stop;
            }
            let v = acc.get() + dx * scroll_sign;
            acc.set(v);
            set_css(v * SWIPE_SCALE, "");
            glib::Propagation::Stop
        });
    }
    {
        let acc = acc.clone();
        let commit = commit.clone();
        let committed = committed.clone();
        scroll.connect_scroll_end(move |_| {
            if committed.get() {
                return;
            }
            let px = acc.get() * SWIPE_SCALE;
            if px.abs() >= SWIPE_COMMIT_PX {
                commit(px);
            } else {
                acc.set(0.0);
                set_css(0.0, SWIPE_SPRING);
            }
        });
    }
    area.add_controller(scroll);
}

/// How far a swiped-away row parks offscreen: its own width + 2 px, so no
/// part of it peeks out beside the undo bar. (The popover is only 360 px
/// wide — the old fixed 260 px left ~100 px of the row visible on top of
/// "Undo", garbling the bar.) Before the first measurement we fall back to
/// the fixed offset.
fn swipe_hidden_offset(row_width: f64, dismiss_dir: f64) -> f64 {
    if row_width > 0.0 {
        dismiss_dir * (row_width + 2.0)
    } else {
        dismiss_dir * SWIPE_PARK_PX
    }
}

// ───────────────────────── Operations popover refresh ─────────────────────────
// The popover is refreshed roughly 8×/second while an operation runs: `nudge_ui`
// fires on the 120 ms progress tick in `operations::run_process`. Rebuilding the
// widget tree on every refresh recreated each pending undo bar — replaying its
// slide-down animation — and destroyed whatever row a swipe gesture was attached
// to, so a swipe-back never survived long enough to be seen. The popover now
// keeps ONE tree alive and refreshes it in place: `ops_signature` decides whether
// anything structural changed, and only that case rebuilds the rows.

thread_local! {
    /// A pointer is currently driving a row in the Operations popover.
    static OPS_GESTURE_ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// A refresh arrived while a gesture was live, so it had to wait.
    static OPS_REFRESH_DIRTY: Cell<bool> = const { Cell::new(false) };
    /// Bumped per gesture so the safety timeout can't clobber a newer one.
    static OPS_GESTURE_SEQ: Cell<u64> = const { Cell::new(0) };
}

fn ops_gesture_active() -> bool {
    OPS_GESTURE_ACTIVE.with(|c| c.get())
}

/// Block structural rebuilds while the user drags a row: replacing a row under a
/// live gesture is exactly what made the swipe-back glitch.
fn ops_gesture_begin() {
    // Only the event that takes the lock arms the safety timeout — a touchpad
    // swipe fires one event per delta, and each must not schedule its own.
    if OPS_GESTURE_ACTIVE.with(|c| c.replace(true)) {
        return;
    }
    let seq = OPS_GESTURE_SEQ.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    // Safety net: a gesture that never reports its end (popover closed mid-drag,
    // sequence cancelled elsewhere) must not freeze the popover's refreshes.
    glib::timeout_add_local_once(Duration::from_millis(5000), move || {
        if OPS_GESTURE_SEQ.with(|c| c.get()) == seq && ops_gesture_active() {
            ops_gesture_end();
        }
    });
}

/// Release the gesture lock and run any refresh that was waiting on it.
fn ops_gesture_end() {
    let dirty = OPS_REFRESH_DIRTY.with(|c| c.replace(false));
    OPS_GESTURE_ACTIVE.with(|c| c.set(false));
    if dirty {
        // Deferred to idle: we run inside a gesture callback, and rebuilding the
        // rows from there would re-enter the widgets currently emitting.
        glib::idle_add_once(crate::app::refresh_search_window);
    }
}

/// Note a refresh that could not run because a gesture is live.
fn ops_refresh_deferred() {
    OPS_REFRESH_DIRTY.with(|c| c.set(true));
}

thread_local! {
    /// Restore motions of the pending undo bars currently on screen, keyed by
    /// `(op_id, hist_id)`, so a keyboard undo (Ctrl+Z) can slide the row back
    /// from the side it left instead of just re-materialising it.
    static OPS_RESTORE_MOTIONS: RefCell<
        std::collections::HashMap<(Option<u64>, Option<u64>), Rc<dyn Fn()>>,
    > = RefCell::new(std::collections::HashMap::new());
}

fn ops_register_restore(key: (Option<u64>, Option<u64>), motion: Rc<dyn Fn()>) {
    OPS_RESTORE_MOTIONS.with(|m| m.borrow_mut().insert(key, motion));
}

fn ops_unregister_restore(key: (Option<u64>, Option<u64>)) {
    OPS_RESTORE_MOTIONS.with(|m| m.borrow_mut().remove(&key));
}

fn ops_take_restore(key: (Option<u64>, Option<u64>)) -> Option<Rc<dyn Fn()>> {
    OPS_RESTORE_MOTIONS.with(|m| m.borrow().get(&key).cloned())
}

/// Undo the newest Operations dismissal, animated the way it left: the pending
/// bar plays its row back in if it is on screen, otherwise the item is simply
/// restored. Returns whether a dismissal was undone.
fn ops_undo_last_dismiss() -> bool {
    let Some(key) = crate::operations::latest_dismissed() else {
        return false;
    };
    if let Some(motion) = ops_take_restore(key) {
        motion();
        return true;
    }
    crate::operations::restore_item(key.0, key.1)
}

/// Structural fingerprint of the Operations popover's rows. Progress and the live
/// status line are deliberately excluded: they change on every tick and are
/// refreshed in place, whereas including them would rebuild the tree 8×/sec.
fn ops_signature(items: &[crate::operations::OpItem]) -> String {
    let mut sig = String::with_capacity(items.len() * 32);
    for it in items {
        sig.push_str(&format!(
            "{:?}/{:?}/{}/{:?}/{};",
            it.op_id, it.hist_id, it.state, it.dismissed_dir, it.title
        ));
    }
    sig
}

fn ops_badge(state: &str) -> (&'static str, &'static str) {
    match state {
        "running" => ("● Running", "accent"),
        "failed" => ("Failed", "error"),
        "cancelled" => ("Cancelled", "warning"),
        _ => ("✓ Done", "success"),
    }
}

/// Widgets of one Operations row, kept so a refresh can update the live parts in
/// place instead of rebuilding — rebuilding a row destroys a swipe gesture on it.
struct OpsRowWidgets {
    /// What the rows box holds (a Revealer for both row kinds).
    widget: gtk::Revealer,
    /// The row box itself — owns the `op-pulse` class.
    row: gtk::Widget,
    title: gtk::Label,
    detail: gtk::Label,
    badge: gtk::Label,
    /// Running rows only: kept so progress ticks update it in place. A fresh
    /// ring every tick restarts its animation, which reads as a stutter.
    ring: Option<crate::ui::circular_progress::Ring>,
    /// Drop this row's swipe CSS rules when the row is destroyed.
    cleanup: Option<Rc<dyn Fn()>>,
    /// Whether this row kind ever pulses (live rows only).
    pulseable: bool,
    pulsing: bool,
}

/// Update the live parts of a row (status text, badge, progress, pulse). Anything
/// structural is the rebuild path's job.
fn ops_row_update(h: &mut OpsRowWidgets, it: &crate::operations::OpItem) {
    if h.title.text().as_str() != it.title {
        h.title.set_text(&it.title);
    }
    if h.detail.text().as_str() != it.detail {
        h.detail.set_text(&it.detail);
    }
    let (badge, _) = ops_badge(&it.state);
    if h.badge.text().as_str() != badge {
        h.badge.set_text(badge);
    }
    if let Some(ring) = &h.ring {
        ring.set(it.progress, crate::ui::circular_progress::RingState::Running);
    }
    let want_pulse = h.pulseable && it.state == "running" && it.progress.is_none();
    if want_pulse != h.pulsing {
        if want_pulse {
            h.row.add_css_class("op-pulse");
        } else {
            h.row.remove_css_class("op-pulse");
        }
        h.pulsing = want_pulse;
    }
}

/// The popover's persistent widget tree plus what the last refresh rendered.
struct OpsPopover {
    list: gtk::Box,
    scroll: gtk::ScrolledWindow,
    rows: gtk::Box,
    empty: gtk::Label,
    /// Structural fingerprint the current `handles` were built from.
    sig: String,
    handles: Vec<OpsRowWidgets>,
}

impl OpsPopover {
    /// Build the tree once. Later refreshes mutate it instead of handing the
    /// popover a brand-new child, which is what caused the flicker, the scroll
    /// reset and the lost gestures.
    fn new() -> Self {
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .width_request(360)
            .build();
        list.append(
            &gtk::Label::builder()
                .label(gettext("Operations"))
                .xalign(0.0)
                .css_classes(["heading"])
                .margin_bottom(4)
                .build(),
        );
        // top_slot: now-playing music card OR music undo bar.
        let top_slot = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        list.append(&top_slot);
        // undo_slot: brief toast after a history/running item is swiped away.
        let undo_slot = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        list.append(&undo_slot);
        let empty = gtk::Label::builder()
            .label(gettext("No operations yet"))
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        list.append(&empty);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(0)
            .max_content_height(360)
            .propagate_natural_height(true)
            .build();
        let rows = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        scroll.set_child(Some(&rows));
        list.append(&scroll);
        Self {
            list,
            scroll,
            rows,
            empty,
            sig: String::new(),
            handles: Vec::new(),
        }
    }
}

/// One live Operations row (icon, status, badge, cancel) with its dismiss
/// gesture. Returns the widgets so refreshes can update them in place.
/// Where the peek bar sits while a live row is dragged away.
///
/// The row travels `offset` under the finger while the bar enters from the
/// opposite side at a complementary rate — the swipe-back's sliding door,
/// mirrored — so the two never overlap: the row spans `[offset, offset+W]` and
/// the bar `[-(R-|offset|), …]`, leaving a `R - W` (2 px) gap throughout. At
/// rest the bar is parked off-screen, so it only ever appears because the
/// gesture revealed it.
fn ops_peek_bar_tx(offset: f64, row_rest: f64) -> f64 {
    let dir = if offset < 0.0 { -1.0 } else { 1.0 };
    -dir * (row_rest - offset.abs()).max(0.0)
}

/// The "Hidden — swipe back to restore" bar with its Undo button. The caller
/// decides what Undo does (and whether it can be pressed at all): the pending
/// bar's button plays the restore motion, the peek inside a live row is
/// pointer-transparent but styled identically, so the hand-off is invisible.
fn build_undo_bar() -> (gtk::Box, gtk::Button) {
    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(6)
        .margin_end(6)
        .margin_top(4)
        .margin_bottom(4)
        // Hug its natural height and centre in the (taller) row instead of
        // stretching — a stretched bar cramped its label and Undo button.
        .valign(gtk::Align::Center)
        .css_classes(["ops-undo-bar"])
        .build();
    bar.append(
        &gtk::Label::builder()
            .label(gettext("Hidden — swipe back to restore"))
            .hexpand(true)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption"])
            .build(),
    );
    let btn = gtk::Button::builder()
        .label(gettext("Undo"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    bar.append(&btn);
    (bar, btn)
}

/// One live Operations row with its dismiss gesture. The row slides under the
/// finger while the undo bar peeks in behind it, so a dismissal is a single
/// continuous motion and the pending state it hands off to is already in place.
fn build_ops_row(it: &crate::operations::OpItem, scroll_sign: f64) -> OpsRowWidgets {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .build();
    let img = gtk::Image::builder().pixel_size(20).build();
    crate::ui::result_row::set_op_row_icon(&img, &it.icon);
    // ponhytail: wrap icon in ring during install; drop the linear bar. The ring
    // is kept (not re-created per tick) so its animation doesn't restart.
    let mut ring: Option<crate::ui::circular_progress::Ring> = None;
    let icon_widget: gtk::Widget = if it.state == "running" {
        let r = crate::ui::circular_progress::Ring::new(32);
        r.snap(it.progress, crate::ui::circular_progress::RingState::Running);
        r.area().set_can_target(false);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&img));
        overlay.add_overlay(r.area());
        ring = Some(r);
        overlay.upcast()
    } else {
        img.upcast()
    };
    row.append(&icon_widget);
    let title = gtk::Label::builder()
        .label(&it.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let detail = gtk::Label::builder()
        .label(&it.detail)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption"])
        .build();
    let textbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    textbox.append(&title);
    textbox.append(&detail);
    row.append(&textbox);
    let (badge_text, badge_css) = ops_badge(&it.state);
    let badge = gtk::Label::builder()
        .label(badge_text)
        .css_classes(["caption", badge_css])
        .valign(gtk::Align::Center)
        .build();
    row.append(&badge);
    // ponhytail: pulse the whole row when progress is unknown.
    let pulsing = it.state == "running" && it.progress.is_none();
    if pulsing {
        row.add_css_class("op-pulse");
    }

    let op_id = it.op_id;
    let hist_id = it.hist_id;
    let (peek_bar, peek_btn) = build_undo_bar();
    // Pointer-transparent, not insensitive: a greyed-out Undo would give the
    // hand-off away a beat before the pending bar's real one replaces it.
    peek_btn.set_can_target(false);

    let row_cls = format!("swrow{}", SWIPE_ROW_CTR.fetch_add(1, Ordering::Relaxed));
    let bar_cls = format!("swrow{}", SWIPE_ROW_CTR.fetch_add(1, Ordering::Relaxed));
    row.add_css_class(&row_cls);
    peek_bar.add_css_class(&bar_cls);
    let set_css = {
        let rc = row_cls.clone();
        move |tx: f64, transition: &str| swipe_apply(&rc, tx, transition)
    };
    let set_bar_css = {
        let rc = bar_cls.clone();
        move |tx: f64, transition: &str| swipe_apply(&rc, tx, transition)
    };
    // Park distance for the peek: the bar has to travel the row's own width to
    // end up centred, and clear it by 2 px so the two never touch.
    let row_rest = Rc::new(Cell::new(swipe_hidden_offset(0.0, 1.0)));
    {
        let row_rest = row_rest.clone();
        row.connect_notify_local(Some("width"), move |w, _| {
            let rest = swipe_hidden_offset(w.width() as f64, 1.0);
            if row_rest.get() != rest {
                row_rest.set(rest);
            }
        });
    }
    // Which side the bar waits on; it flips (off-screen, so unseen) when the
    // gesture heads the other way.
    let park_side = Rc::new(Cell::new(-1.0_f64));
    set_bar_css(-swipe_hidden_offset(0.0, 1.0), "");

    let overlay = gtk::Overlay::new();
    // Clip: the peek bar enters from beyond the edge, so without this it would
    // paint over the row it is supposed to appear behind.
    overlay.set_overflow(gtk::Overflow::Hidden);
    overlay.set_child(Some(&row));
    overlay.add_overlay(&peek_bar);
    // Measure the bar too, so a live row's slot is exactly as tall as the
    // pending row's slot it is replaced by — otherwise the hand-off nudges the
    // whole list.
    overlay.set_measure_overlay(&peek_bar, true);

    let row_rev = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .transition_duration(240)
        .reveal_child(true)
        .child(&overlay)
        .build();
    let dismissed = Rc::new(Cell::new(false));

    // Park the bar on the side the row is leaving towards before mapping it.
    let park_bar_for: Rc<dyn Fn(f64)> = {
        let set_bar_css = set_bar_css.clone();
        let row_rest = row_rest.clone();
        let park_side = park_side.clone();
        Rc::new(move |dir: f64| {
            if park_side.get() != -dir {
                park_side.set(-dir);
                set_bar_css(-dir * row_rest.get(), "");
            }
        })
    };
    // Move the row and the bar together.
    let move_to: Rc<dyn Fn(f64, f64, &str)> = {
        let set_css = set_css.clone();
        let set_bar_css = set_bar_css.clone();
        Rc::new(move |row_tx: f64, bar_tx: f64, transition: &str| {
            set_css(row_tx, transition);
            set_bar_css(bar_tx, transition);
        })
    };

    let do_dismiss: Rc<dyn Fn(f64)> = {
        let park_bar_for = park_bar_for.clone();
        let move_to = move_to.clone();
        let dis = dismissed.clone();
        let row_cls = row_cls.clone();
        let bar_cls = bar_cls.clone();
        Rc::new(move |dx: f64| {
            if dis.get() {
                return;
            }
            dis.set(true);
            let dir = if dx >= 0.0 { 1.0 } else { -1.0 };
            park_bar_for(dir);
            // One motion: the row leaves as the bar arrives in its place, so
            // the pending bar built below is already sitting where this ended.
            move_to(dir * SWIPE_SETTLE_PX, 0.0, "transform 300ms ease-out");
            let row_cls = row_cls.clone();
            let bar_cls = bar_cls.clone();
            glib::timeout_add_local_once(Duration::from_millis(310), move || {
                crate::operations::dismiss_item(op_id, hist_id, dir);
                swipe_clear(&row_cls);
                swipe_clear(&bar_cls);
            });
        })
    };

    if it.state == "running" && op_id.is_some() {
        let cancel_btn = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Cancel"))
            .build();
        cancel_btn.connect_clicked(move |_| {
            if let Some(id) = op_id {
                crate::operations::cancel(id);
            }
        });
        row.append(&cancel_btn);
    }
    // Close button: the same dismissal a swipe performs, without a gesture.
    {
        let dismiss = do_dismiss.clone();
        let close_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Dismiss"))
            .build();
        close_btn.connect_clicked(move |_| {
            // Exit leftwards by default; the bar therefore enters from the right.
            dismiss(-1.0);
        });
        row.append(&close_btn);
    }

    let drag = gtk::GestureDrag::new();
    drag.set_touch_only(false);
    drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
    drag.connect_drag_begin(move |_, _, _| ops_gesture_begin());
    {
        let park_bar_for = park_bar_for.clone();
        let move_to = move_to.clone();
        let row_rest = row_rest.clone();
        let dismiss = do_dismiss.clone();
        let dis = dismissed.clone();
        drag.connect_drag_update(move |_, dx, _| {
            if dis.get() {
                return;
            }
            park_bar_for(if dx < 0.0 { -1.0 } else { 1.0 });
            move_to(dx, ops_peek_bar_tx(dx, row_rest.get()), "");
            if dx.abs() >= SWIPE_OFFSCREEN_PX {
                dismiss(dx);
            }
        });
    }
    {
        let move_to = move_to.clone();
        let row_rest = row_rest.clone();
        let park_side = park_side.clone();
        let dismiss = do_dismiss.clone();
        let dis = dismissed.clone();
        drag.connect_drag_end(move |_, dx, _| {
            ops_gesture_end();
            if dis.get() {
                return;
            }
            if dx.abs() >= SWIPE_COMMIT_PX {
                dismiss(dx);
            } else {
                move_to(0.0, park_side.get() * row_rest.get(), SWIPE_SPRING);
            }
        });
    }
    row_rev.add_controller(drag);

    let scroll_ctl =
        gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    scroll_ctl.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let scroll_acc = Rc::new(Cell::new(0.0_f64));
    {
        let park_bar_for = park_bar_for.clone();
        let move_to = move_to.clone();
        let row_rest = row_rest.clone();
        let dismiss = do_dismiss.clone();
        let acc = scroll_acc.clone();
        let dis = dismissed.clone();
        scroll_ctl.connect_scroll(move |_, dx, _dy| {
            if dis.get() {
                return glib::Propagation::Stop;
            }
            if dx.abs() < 0.005 {
                return glib::Propagation::Proceed;
            }
            ops_gesture_begin();
            let raw = dx * scroll_sign;
            if let Some(fling) = scroll_fling_offset(raw) {
                acc.set(0.0);
                dismiss(fling);
                return glib::Propagation::Stop;
            }
            let new_acc = acc.get() + raw;
            acc.set(new_acc);
            let px = new_acc * SWIPE_SCALE;
            park_bar_for(if px < 0.0 { -1.0 } else { 1.0 });
            move_to(px, ops_peek_bar_tx(px, row_rest.get()), "");
            if px.abs() >= SWIPE_COMMIT_PX {
                dismiss(px);
            }
            glib::Propagation::Stop
        });
    }
    {
        let move_to = move_to.clone();
        let row_rest = row_rest.clone();
        let park_side = park_side.clone();
        let acc = scroll_acc.clone();
        let dis = dismissed.clone();
        scroll_ctl.connect_scroll_end(move |_| {
            ops_gesture_end();
            if dis.get() {
                return;
            }
            acc.set(0.0);
            move_to(0.0, park_side.get() * row_rest.get(), SWIPE_SPRING);
        });
    }
    row_rev.add_controller(scroll_ctl);

    let cleanup: Rc<dyn Fn()> = {
        let row_cls = row_cls.clone();
        let bar_cls = bar_cls.clone();
        Rc::new(move || {
            swipe_clear(&row_cls);
            swipe_clear(&bar_cls);
        })
    };

    OpsRowWidgets {
        widget: row_rev,
        row: row.clone().upcast(),
        title,
        detail,
        badge,
        ring,
        cleanup: Some(cleanup),
        pulsing,
        pulseable: true,
    }
}


fn attach_ops_pending_swipe(
    area: &impl IsA<gtk::Widget>,
    undo_feedback: &impl IsA<gtk::Widget>,
    row_feedback: &impl IsA<gtk::Widget>,
    dismiss_dir: f64,
    on_reverse: Rc<dyn Fn()>,
    on_forward: Rc<dyn Fn()>,
) -> (Rc<dyn Fn()>, Rc<dyn Fn()>) {
    let undo_cls = format!("swrow{}", SWIPE_ROW_CTR.fetch_add(1, Ordering::Relaxed));
    let row_cls = format!("swrow{}", SWIPE_ROW_CTR.fetch_add(1, Ordering::Relaxed));
    undo_feedback.add_css_class(&undo_cls);
    row_feedback.add_css_class(&row_cls);
    let set_undo_css = {
        let cls = undo_cls.clone();
        move |tx: f64, transition: &str| swipe_apply(&cls, tx, transition)
    };
    let set_row_css = {
        let cls = row_cls.clone();
        move |tx: f64, transition: &str| swipe_apply(&cls, tx, transition)
    };
    let committed = Rc::new(Cell::new(false));
    // Owned handle: the row is fully transparent while parked (so nothing of
    // it can ever show through in the popover) and fades in while a
    // swipe-back drags it. Opacity, not visibility — hiding the row would
    // collapse the overlay (and with it the Undo bar), because GtkOverlay
    // sizes from its main child.
    let row_widget: gtk::Widget = row_feedback.upcast_ref::<gtk::Widget>().clone();
    // Park the row by its ACTUAL width and refresh it whenever the width
    // changes (a popover rebuild re-creates the widgets, and the first
    // measurement lands right after the row is mapped).
    let row_rest = Rc::new(Cell::new(dismiss_dir * SWIPE_PARK_PX));
    {
        let row_rest = row_rest.clone();
        let set_row_css = set_row_css.clone();
        row_feedback.connect_notify_local(Some("width"), move |w, _| {
            let rest = swipe_hidden_offset(w.width() as f64, dismiss_dir);
            if row_rest.get() != rest {
                row_rest.set(rest);
                set_row_css(rest, "");
            }
        });
    }

    set_row_css(row_rest.get(), "");
    row_widget.set_opacity(0.0);

    let commit: Rc<dyn Fn(f64)> = {
        let set_undo_css = set_undo_css.clone();
        let set_row_css = set_row_css.clone();
        let committed = committed.clone();
        let on_reverse = on_reverse.clone();
        let on_forward = on_forward.clone();
        let row_rest = row_rest.clone();
        let row_widget = row_widget.clone();
        Rc::new(move |offset: f64| {
            if committed.get() {
                return;
            }
            committed.set(true);
            let target = if offset >= 0.0 {
                SWIPE_SETTLE_PX
            } else {
                -SWIPE_SETTLE_PX
            };
            let reverse = offset * dismiss_dir < 0.0;
            // A reverse settle brings the row home — make sure it is visible
            // (a fling can commit without a single drag/scroll update).
            if reverse {
                row_widget.set_opacity(1.0);
            }
            set_undo_css(target, "transform 300ms ease-out");
            set_row_css(
                if reverse {
                    0.0
                } else {
                    pending_row_tx(row_rest.get(), dismiss_dir, target)
                },
                "transform 300ms ease-out",
            );
            let on_reverse = on_reverse.clone();
            let on_forward = on_forward.clone();
            glib::timeout_add_local_once(Duration::from_millis(310), move || {
                if reverse {
                    on_reverse();
                } else {
                    on_forward();
                }
            });
        })
    };

    let scroll_sign: f64 = if touchpad_natural_scroll() { -1.0 } else { 1.0 };

    let drag = gtk::GestureDrag::new();
    drag.set_touch_only(false);
    drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
    drag.connect_drag_begin(move |_, _, _| ops_gesture_begin());
    {
        let set_undo_css = set_undo_css.clone();
        let set_row_css = set_row_css.clone();
        let committed = committed.clone();
        let row_rest = row_rest.clone();
        let row_widget = row_widget.clone();
        drag.connect_drag_update(move |_, dx, _| {
            if committed.get() {
                return;
            }
            // Show the row only while it is actually coming back (reverse
            // drag); on a forward drag it is off-screen and stays hidden.
            row_widget.set_opacity(if dx * dismiss_dir < 0.0 { 1.0 } else { 0.0 });
            set_undo_css(pending_bar_tx(row_rest.get(), dismiss_dir, dx), "");
            set_row_css(pending_row_tx(row_rest.get(), dismiss_dir, dx), "");
        });
    }
    {
        let set_undo_css = set_undo_css.clone();
        let set_row_css = set_row_css.clone();
        let commit = commit.clone();
        let committed = committed.clone();
        let row_rest = row_rest.clone();
        let row_widget = row_widget.clone();
        drag.connect_drag_end(move |_, dx, _| {
            ops_gesture_end();
            if committed.get() {
                return;
            }
            if dx.abs() >= SWIPE_COMMIT_PX {
                commit(dx);
            } else {
                set_undo_css(0.0, SWIPE_SPRING);
                set_row_css(row_rest.get(), SWIPE_SPRING);
                // The row is visible while it springs back out — hide it again
                // once the spring has settled, so the rest state stays clean.
                let row_widget = row_widget.clone();
                let committed = committed.clone();
                glib::timeout_add_local_once(Duration::from_millis(320), move || {
                    if !committed.get() {
                        row_widget.set_opacity(0.0);
                    }
                });
            }
        });
    }
    area.add_controller(drag);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let acc = Rc::new(Cell::new(0.0_f64));
    {
        let set_undo_css = set_undo_css.clone();
        let set_row_css = set_row_css.clone();
        let acc = acc.clone();
        let commit = commit.clone();
        let committed = committed.clone();
        let row_rest = row_rest.clone();
        let row_widget = row_widget.clone();
        scroll.connect_scroll(move |_, dx, _| {
            if committed.get() {
                return glib::Propagation::Stop;
            }
            if dx.abs() < 0.005 {
                return glib::Propagation::Proceed;
            }
            ops_gesture_begin();
            if let Some(fling) = scroll_fling_offset(dx * scroll_sign) {
                acc.set(0.0);
                commit(fling);
                return glib::Propagation::Stop;
            }
            let v = acc.get() + dx * scroll_sign;
            acc.set(v);
            let px = v * SWIPE_SCALE;
            // Show the row only while it is actually coming back (reverse
            // scroll); on a forward scroll it is off-screen and stays hidden.
            row_widget.set_opacity(if px * dismiss_dir < 0.0 { 1.0 } else { 0.0 });
            set_undo_css(pending_bar_tx(row_rest.get(), dismiss_dir, px), "");
            set_row_css(pending_row_tx(row_rest.get(), dismiss_dir, px), "");
            glib::Propagation::Stop
        });
    }
    {
        let acc = acc.clone();
        let commit = commit.clone();
        let committed = committed.clone();
        let row_rest = row_rest.clone();
        let row_widget = row_widget.clone();
        scroll.connect_scroll_end(move |_| {
            ops_gesture_end();
            if committed.get() {
                return;
            }
            let px = acc.get() * SWIPE_SCALE;
            if px.abs() >= SWIPE_COMMIT_PX {
                commit(px);
            } else {
                acc.set(0.0);
                set_undo_css(0.0, SWIPE_SPRING);
                set_row_css(row_rest.get(), SWIPE_SPRING);
                // Visible while it springs back out — hide it again once the
                // spring has settled, so the rest state stays clean.
                let row_widget = row_widget.clone();
                let committed = committed.clone();
                glib::timeout_add_local_once(Duration::from_millis(320), move || {
                    if !committed.get() {
                        row_widget.set_opacity(0.0);
                    }
                });
            }
        });
    }
    area.add_controller(scroll);

    // Undo without a gesture must retrace the same path the row took on its way
    // out: it slides back in from the side it was dismissed to while the bar
    // leaves the way it came in. Used by the bar's Undo button and Ctrl+Z.
    let restore: Rc<dyn Fn()> = {
        let commit = commit.clone();
        Rc::new(move || commit(-dismiss_dir * SWIPE_COMMIT_PX))
    };

    // Drop this row's swipe CSS rules when the row is destroyed, so a popover
    // that rebuilds its rows on structural changes can't leave rules behind.
    let cleanup: Rc<dyn Fn()> = Rc::new(move || {
        swipe_clear(&undo_cls);
        swipe_clear(&row_cls);
    });
    (cleanup, restore)
}

/// The undo bar that replaces a dismissed Operations row: the row itself (parked
/// off-screen and transparent, so it can slide back in) under an overlay bar.
/// `animate` is false for rebuilds that aren't the bar's first appearance, so a
/// progress tick can't replay the slide-down.
/// The undo bar that replaces a dismissed Operations row: the row itself
/// (parked off-screen and transparent, so a swipe-back can slide it in) under an
/// overlay bar. It is built already revealed — the live row's peek animation has
/// just placed the bar exactly here, so playing an entrance would double up.
fn build_ops_pending_bar(it: &crate::operations::OpItem) -> OpsRowWidgets {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .build();
    let img = gtk::Image::builder().pixel_size(20).build();
    crate::ui::result_row::set_op_row_icon(&img, &it.icon);
    row.append(&img);
    let textbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    let title = gtk::Label::builder()
        .label(&it.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let detail = gtk::Label::builder()
        .label(&it.detail)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "caption"])
        .build();
    textbox.append(&title);
    textbox.append(&detail);
    row.append(&textbox);
    let (badge_text, badge_css) = ops_badge(&it.state);
    let badge = gtk::Label::builder()
        .label(badge_text)
        .css_classes(["caption", badge_css])
        .valign(gtk::Align::Center)
        .build();
    row.append(&badge);
    if it.state == "running" && it.op_id.is_some() {
        row.append(
            &gtk::Button::builder()
                .icon_name("process-stop-symbolic")
                .css_classes(["flat", "circular"])
                .valign(gtk::Align::Center)
                .tooltip_text(gettext("Cancel"))
                .build(),
        );
    }

    let (bar, btn) = build_undo_bar();
    let op_id = it.op_id;
    let hist_id = it.hist_id;
    {
        // The hidden row keeps the same close button a live row has, so a
        // swipe-back reveal shows the row exactly as it was. Closing it again
        // commits the dismissal instead of restoring it.
        let close_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .css_classes(["flat", "circular"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettext("Dismiss"))
            .build();
        close_btn.connect_clicked(move |_| {
            crate::operations::commit_item(op_id, hist_id);
        });
        row.append(&close_btn);
    }

    let overlay = gtk::Overlay::new();
    // Clip to the popover: the parked row is translated past the edge, so
    // without this it stays visible outside the clip and paints over the
    // bar's Undo button (the "Uninstall/Fluffy" tangle). Clipping also makes
    // a swipe-back slide in cleanly from the edge.
    overlay.set_overflow(gtk::Overflow::Hidden);
    overlay.set_child(Some(&row));
    overlay.add_overlay(&bar);
    // Insurance: also measure the bar. A GtkOverlay sizes itself from its
    // main child only, and that child is the (fully transparent) row here —
    // if the row is ever not laid out, the overlay collapses to zero and the
    // Undo bar gets no allocation and vanishes. Measuring the bar too makes
    // the bar define the size whenever the row doesn't. Measuring it in the
    // live row's overlay too keeps both slots the same height.
    overlay.set_measure_overlay(&bar, true);

    let (cleanup, restore) = attach_ops_pending_swipe(
        &overlay,
        &bar,
        &row,
        it.dismissed_dir.unwrap_or(1.0),
        Rc::new(move || {
            crate::operations::restore_item(op_id, hist_id);
        }),
        Rc::new(move || {
            crate::operations::commit_item(op_id, hist_id);
        }),
    );
    // Undo retraces the way in rather than snapping the row back, and Ctrl+Z
    // finds the same motion through this registry.
    btn.connect_clicked({
        let restore = restore.clone();
        move |_| restore()
    });
    let key = (op_id, hist_id);
    ops_register_restore(key, restore);
    let cleanup: Rc<dyn Fn()> = {
        let inner = cleanup;
        Rc::new(move || {
            ops_unregister_restore(key);
            inner();
        })
    };

    OpsRowWidgets {
        widget: gtk::Revealer::builder()
            .reveal_child(true)
            .child(&overlay)
            .build(),
        row: row.clone().upcast(),
        title,
        detail,
        badge,
        ring: None,
        cleanup: Some(cleanup),
        pulseable: false,
        pulsing: false,
    }
}


/// When a clipboard entry was captured (unix seconds), from the live history.
fn clip_time(entry: &crate::clipboard::ClipboardEntry) -> Option<u64> {
    crate::app::with_state(|st| st.clipboard.borrow().time_of(entry))
}

/// Whether a result deserves the right-hand preview pane: files and
/// clipboard entries, plus the update rows — whose pane carries the
/// boxed list of packages an update would touch (typing "update" shows
/// what can be updated).
fn previewable(r: &SearchResult) -> bool {
    match &r.action {
        Action::OpenPath(_)
        | Action::BrowseInto(_)
        | Action::OpenInFileManager(_)
        | Action::CopyImageToClipboard(_)
        | Action::CopyFileToClipboard(_)
        | Action::CopyToClipboard(_) => true,
        Action::StartOperation { args, title, .. } => {
            crate::search::cmd::is_update_run(title, args)
        }
        Action::CheckUpdates => true,
        Action::ConfirmRunCommand(cmd) => is_restart_command(cmd),
        _ => false,
    }
}

/// True for the confirmed-reboot row (the one that says the updates are waiting
/// for a restart).
fn is_restart_command(cmd: &str) -> bool {
    cmd == crate::search::system::reboot_command()
}

fn upd_preview(row: &gtk::ListBoxRow, rs: &[SearchResult], p: &PreviewPane) {
    let Some(r) = rs.get(row.index() as usize) else {
        return p.clear();
    };
    // Clipboard entries show when they were captured, above the preview.
    let clip_at = match &r.action {
        Action::CopyToClipboard(t) => {
            clip_time(&crate::clipboard::ClipboardEntry::Text(t.clone()))
        }
        Action::CopyImageToClipboard(pa) => {
            clip_time(&crate::clipboard::ClipboardEntry::Image(pa.clone()))
        }
        Action::CopyFileToClipboard(pa) => {
            clip_time(&crate::clipboard::ClipboardEntry::File(pa.clone()))
        }
        _ => None,
    };
    p.show_clip_stamp(clip_at);
    match &r.action {
        Action::OpenPath(pa) | Action::BrowseInto(pa) | Action::OpenInFileManager(pa) => {
            p.show_path(pa)
        }
        // Copied images: preview the cached PNG
        Action::CopyImageToClipboard(pa) => p.show_path(pa),
        Action::CopyFileToClipboard(pa) => p.show_path(pa),
        // Clipboard text entries: show the full text, scrollable.
        Action::CopyToClipboard(t) => p.show_text(t),
        // Update rows: the package list this run will touch.
        Action::StartOperation { args, title, .. }
            if crate::search::cmd::is_update_run(title, args) =>
        {
            let details = crate::search::cmd::details_for(title);
            let sub = r.subtitle.clone().unwrap_or_default();
            p.show_update(title, &sub, &details);
        }
        // An idle scope recommendation ("Nothing pending — press Enter to
        // check again"): same page, nothing to list.
        Action::CheckUpdates => {
            p.show_update(&r.title, r.subtitle.as_deref().unwrap_or(""), &[]);
        }
        // The restart row: say why it's there and what pressing Enter does.
        // Only a *required* restart gets the notice wording — the plain
        // Restart row (shown again while nothing is pending) just says what
        // Enter does.
        Action::ConfirmRunCommand(cmd) if is_restart_command(cmd) => {
            let head = if crate::search::cmd::reboot_pending() {
                gettext("Restart required to finish the update")
            } else {
                gettext("Restart")
            };
            p.show_update(&head, &gettext("Press Enter to restart"), &[]);
        }
        // Triggers: installed ones show usage instructions (+ screenshot once
        // its help_image is cached).
        Action::EnterMode(word) => {
            if let Some(a) = crate::triggers::keyword_for_word(word)
                .and_then(|kw| crate::triggers::by_id(&kw.id))
            {
                let help = crate::triggers::help_text(&a);
                if let Some(img) = trigger_help_image(&a, &help, p) {
                    p.show_help(&help, Some(&img));
                } else {
                    p.show_help(&help, None);
                }
            } else {
                p.clear();
            }
        }
        _ => p.clear(),
    }
}

/// Download the trigger's `help_image` into the triggers cache dir on first
/// preview; returns the cache path (even while the download is in flight).
/// When the download lands, re-renders the preview only if the same trigger
/// is still the one being previewed.
fn trigger_help_image(
    a: &crate::triggers::TriggerManifest,
    help: &str,
    p: &PreviewPane,
) -> Option<std::path::PathBuf> {
    let url = a.help_image.trim();
    if url.is_empty() {
        return None;
    }
    // Only plain web URLs — a manifest must not make curl read local files
    // (file://) or follow exotic schemes into the trigger cache.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        log::warn!("triggers: ignoring help_image with unsupported scheme: {url}");
        return None;
    }
    let dir = crate::triggers::triggers_dir().join("cache");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let cache = dir.join(format!("{}.img", a.id));
    if cache.exists() {
        return Some(cache);
    }
    let (tx, rx) = futures::channel::oneshot::channel();
    let cache2 = cache.clone();
    let url2 = url.to_string();
    std::thread::spawn(move || {
        // -f: HTTP errors fail instead of caching an error page;
        // redirects capped, 2 MiB ceiling — a hostile or broken URL can't
        // flood the disk or hang past the time budget.
        let ok = std::process::Command::new("curl")
            .args([
                "-fsSL",
                "--max-time",
                "8",
                "--max-redirs",
                "5",
                "--max-filesize",
                "2097152",
                "-o",
            ])
            .arg(&cache2)
            .arg(&url2)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            let _ = std::fs::remove_file(&cache2);
        }
        let _ = tx.send(ok);
    });
    let p2 = p.clone();
    let help2 = help.to_string();
    let cache3 = cache.clone();
    let a_id = a.id.clone();
    glib::MainContext::default().spawn_local(async move {
        if rx.await.ok().unwrap_or(false) {
            log::info!("triggers: help image cached for {}", a_id);
            if p2.current_path() == cache3 {
                p2.show_help(&help2, Some(&cache3));
            }
        }
    });
    Some(cache)
}

fn activate(
    result: &SearchResult,
    results: &[SearchResult],
    entry: &gtk::Entry,
    window: &gtk::Window,
    shown: &Rc<Cell<bool>>,
) {
    match &result.action {
        Action::LaunchDesktopFile(p) => {
            if let Some(i) = gio::DesktopAppInfo::from_filename(p) {
                let _ = i.launch(&[], Some(&gio::AppLaunchContext::new()));
            } else if std::env::var("FLATPAK_ID").is_ok() {
                if let Some(id) = p.file_stem().and_then(|s| s.to_str()) {
                    let _ = std::process::Command::new("flatpak-spawn")
                        .args(["--host", "gtk-launch", id])
                        .spawn();
                }
            }
            dismiss(window, shown, false);
        }
        Action::LaunchAppImage(p) => {
            // Detached host run (chmod + extract-and-run fallback live in
            // the appimage module) — the window closes right away.
            crate::search::appimage::launch(p);
            dismiss(window, shown, false);
        }
        Action::RemoveAppImage(_) => {
            // Handled in the row-activation / key paths (which own popover_open).
        }
        Action::OpenPath(p) => {
            crate::recent_paths::record(p);
            let _ = gio::AppInfo::launch_default_for_uri(
                &gio::File::for_path(p).uri(),
                gio::AppLaunchContext::NONE,
            );
            dismiss(window, shown, false);
        }
        Action::OpenInFileManager(p) | Action::BrowseInto(p) => {
            crate::recent_paths::record(p);
            // Enter always opens folders in the file manager, regardless of
            // which one is configured as default.
            crate::app::open_in_file_manager(p);
            dismiss(window, shown, false);
        }
        Action::OpenUrl(u) => {
            let _ = gio::AppInfo::launch_default_for_uri(u, gio::AppLaunchContext::NONE);
            dismiss(window, shown, false);
        }
        Action::OpenUrlPrivate(u) => {
            // Private or nothing: if this browser has no private window we know
            // how to ask for, the search does not open at all rather than in a
            // window that quietly isn't private.
            if let Some(argv) = crate::search::browser_launch::private_window_argv(&u) {
                crate::search::browser_launch::spawn_private_window(&argv);
                dismiss(window, shown, false);
            } else {
                log::info!("private window: no private mode known for this browser");
            }
        }
        Action::CopyToClipboard(t) => {
            crate::clipboard::set_text(t);
            dismiss(window, shown, false);
            // Clip entries: don't auto-paste — re-copying already moves the
            // entry to the front of the clipboard history (watcher's push_text).
            if result.kind == crate::search::ResultKind::Clipboard {
                return;
            }
            // Translations are copy-only: Enter copies the translation and
            // nothing is typed into the previously focused app.
            if result.kind == crate::search::ResultKind::Translate {
                return;
            }
            if result.kind == crate::search::ResultKind::Emoji {
                // Prefer typing the emoji directly via wtype (virtual-keyboard
                // protocol) — unlike XTest-based tools (xdotool), it doesn't
                // trigger GNOME's "Remote Desktop" input-control permission
                // prompt. If wtype isn't installed, fall back to a Ctrl+V
                // paste of the clipboard (already set above).
                let text = t.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(75));
                    if !crate::keysynth::do_type_text(&text) {
                        let _ = crate::keysynth::do_keybinding("Ctrl+V");
                    }
                });
            } else {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(75));
                    let _ = crate::keysynth::do_keybinding("Ctrl+V");
                });
            }
        }
        Action::CopyImageToClipboard(p) => {
                crate::clipboard::set_image(p);
                dismiss(window, shown, false);
                // Clip entries: don't auto-paste.
                if result.kind == crate::search::ResultKind::Clipboard {
                    return;
                }
                std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(75));
                let _ = crate::keysynth::do_keybinding("Ctrl+V");
            });
        }
        Action::CopyFileToClipboard(p) => {
            crate::clipboard::set_file(p);
            crate::fileops::copy(p);
            // The clipboard watcher skips our own content (is_local), so move
            // the entry to the front explicitly to preserve re-copy recency.
            crate::app::with_state(|st| st.clipboard.borrow_mut().push_file(p.clone()));
            dismiss(window, shown, false);
        }
        Action::InsertCalculatorResult(t) => {
            crate::clipboard::set_text(t);
            dismiss(window, shown, false);
            // Optional auto-paste (calculator settings): the same 75 ms +
            // Ctrl+V dance as the copy actions, so Spotty is gone by then.
            let paste = crate::app::with_state(|st| st.config.borrow().calc_paste);
            if paste {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(75));
                    let _ = crate::keysynth::do_keybinding("Ctrl+V");
                });
            }
        }
        Action::RunCommand(cmd) => {
            let _ = crate::app::spawn_host_shell_command(cmd);
            dismiss(window, shown, false);
        }
        Action::ConfirmRunCommand(_) => {
            // Handled in the row-activation / key paths (which own popover_open).
        }
        Action::RunInTerminal(cmd) => {
            crate::app::run_in_terminal(cmd);
            dismiss(window, shown, false);
        }
        Action::RunWithProgress { .. } => {
            // Handled in the row-activation path (run_fn closure), not here.
        }
        Action::Bluetooth { .. } => {
            // Handled in the row-activation / key paths (streams via the
            // progress pane), not here.
        }
        Action::StartOperation {
            title,
            source,
            icon,
            args,
        } => {
            // Normally handled in the row-activation / key paths (which keep the
            // window open and re-render in place). This is a fallback.
            crate::operations::start(title.clone(), source.clone(), icon.clone(), args.clone());
        }
        Action::EnterMode(_s) => {
            // Mode entry is handled in the key handler (which owns the chip + mode state).
        }
        // — Translate trigger: all three stay in-window (no dismiss), the
        // window keeps composing the query while options change.
        Action::SetTranslateTarget { code, strip } => {
            crate::search::translate::set_target(code);
            if !strip.is_empty() {
                let cur = entry.text().to_string();
                if let Some(head) = crate::search::translate::strip_tail_token(&cur, strip) {
                    // Drop the language token that picked this target, keep a
                    // trailing space so the next word types on.
                    let new_text = format!("{head} ");
                    entry.set_text(&new_text);
                    entry.set_position(char_count(&new_text) as i32);
                }
            }
            crate::app::refresh_search_window();
        }
        Action::TranslateExpand { source } => {
            crate::search::translate::expand(*source);
            crate::app::refresh_search_window();
        }
        Action::SetTranslateSource { code } => {
            crate::search::translate::set_source(code);
            crate::app::refresh_search_window();
        }
        Action::TranslateNow { text, target } => {
            crate::search::translate::translate_now(text, target);
            crate::app::refresh_search_window();
        }
        Action::ToggleUpdates => {
            // Flip the feature from search itself (and from Settings).
            crate::app::with_state(|st| {
                let mut c = st.config.borrow_mut();
                c.enable_updates = !c.enable_updates;
                c.save();
            });
            crate::app::refresh_search_window();
        }
        Action::SnoozeUpdates => {
            // Remind tomorrow — hides the badge/notice for 24 hours.
            crate::search::cmd::snooze_update_notice();
        }
        Action::DismissUpdates(sig) => {
            // Hide this update set until a *new* one appears.
            crate::search::cmd::dismiss_update_notice(sig);
        }
        Action::CheckUpdates => {
            // Enter on "No updates available" (or the checking row):
            // re-run the check; the list updates when the result lands.
            crate::search::cmd::check_updates_now();
            crate::app::refresh_search_window();
        }
        Action::Noop => {
            // Display-only row (e.g. "No matching updates").
        }
        Action::UninstallTrigger(id) => {
            let name = crate::triggers::by_id(id)
                .map(|a| a.name)
                .unwrap_or_else(|| id.clone());
            let dialog = adw::MessageDialog::builder()
                .transient_for(window)
                .heading(gettext("Uninstall {name}?").replace("{name}", &name))
                .body(gettext("The trigger word and its files will be removed."))
                .build();
            dialog.add_response("cancel", &gettext("Cancel"));
            dialog.add_response("uninstall", &gettext("Uninstall"));
            dialog.set_default_response(Some("cancel"));
            dialog.set_response_appearance("uninstall", adw::ResponseAppearance::Destructive);
            let entry = entry.clone();
            let id = id.clone();
            dialog.connect_response(Some("uninstall"), move |_, resp| {
                if resp == "uninstall" {
                    if let Err(e) = crate::triggers::uninstall(&id) {
                        log::warn!("triggers: uninstall failed: {e}");
                    }
                    let _ = std::fs::remove_file(
                        crate::triggers::triggers_dir().join("cache").join(format!("{id}.img")),
                    );
                    entry.emit_by_name::<()>("changed", &[]);
                }
            });
            dialog.present();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Copy-result + shortcut matching helpers
// ──────────────────────────────────────────────────────────────────────

/// Delete the previous word from the entry's TYPED text (Ctrl+Space / Ctrl+Backspace).
/// In path mode, delete one path segment back to the previous '/'.
fn delete_prev_word(entry: &gtk::Entry, typed_len: &Cell<usize>, skip_ghost: &Cell<bool>) {
    let full = entry.text().to_string();
    // The typed portion is everything before the ghost tail, even if GTK has
    // already dropped the visible selection highlight.
    let typed_chars = logical_typed_chars(entry, typed_len);
    let typed: String = full.chars().take(typed_chars).collect();
    if typed.is_empty() {
        return;
    }

    let path_mode = typed.starts_with('/') || typed.starts_with('~');
    let new_text: String = if path_mode {
        let trimmed = typed.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(idx) if idx == 0 && trimmed.starts_with('/') => "/".to_string(),
            Some(idx) => trimmed[..idx + 1].to_string(),
            None => String::new(),
        }
    } else {
        // Trim trailing whitespace, then drop the last word.
        let trimmed = typed.trim_end();
        match trimmed.rfind(char::is_whitespace) {
            Some(idx) => {
                let ws_char_len = trimmed[idx..]
                    .chars()
                    .next()
                    .map(|ch| ch.len_utf8())
                    .unwrap_or(1);
                trimmed[..idx + ws_char_len].to_string()
            }
            None => String::new(),
        }
    };

    skip_ghost.set(true);
    typed_len.set(char_count(&new_text));
    entry.set_text(&new_text);
    entry.set_position(char_count(&new_text) as i32);
}

// ──────────────────────────────────────────────────────────────────────
// File-operation helpers (copy / cut / paste destinations)
// ──────────────────────────────────────────────────────────────────────

/// The filesystem path of the currently-selected result, if it is a file or
/// folder (used by Ctrl+C / Ctrl+X).
/// Record a copied/cut filesystem item into the clipboard manager. Images are
/// stored as Image entries (shown with a picture thumbnail); all other files and
/// folders are stored as File entries (shown with a type/thumbnail preview).
fn record_clip_item(clipboard: &Rc<RefCell<ClipboardHistory>>, path: &std::path::Path) {
    let is_image = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| {
            matches!(
                e.to_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "bmp"
                    | "svg"
                    | "tiff"
                    | "tif"
                    | "avif"
                    | "heic"
                    | "heif"
                    | "ico"
            )
        })
        .unwrap_or(false);
    let mut hist = clipboard.borrow_mut();
    if is_image {
        hist.push_image(path.to_path_buf());
    } else {
        hist.push_file(path.to_path_buf());
    }
}

fn selected_path(
    list: &gtk::ListBox,
    results: &Rc<RefCell<Vec<SearchResult>>>,
) -> Option<std::path::PathBuf> {
    let row = list.selected_row()?;
    let rs = results.borrow();
    let res = rs.get(row.index() as usize)?;
    match &res.action {
        Action::OpenPath(p) | Action::BrowseInto(p) | Action::OpenInFileManager(p) => {
            Some(p.clone())
        }
        Action::CopyImageToClipboard(p) => Some(p.clone()),
        Action::CopyFileToClipboard(p) => Some(p.clone()),
        _ => None,
    }
}

/// Decide where a paste should land:
///   1. If the selected result is a FOLDER, paste INTO it.
///   2. Else if the selected result is a FILE, paste into its PARENT dir.
///   3. Else if the query is a path (browse mode), paste into that directory.
///   4. Else fall back to the home directory.
fn paste_destination(
    entry: &gtk::Entry,
    list: &gtk::ListBox,
    results: &Rc<RefCell<Vec<SearchResult>>>,
) -> Option<std::path::PathBuf> {
    // 1 & 2 — based on the selected result.
    if let Some(row) = list.selected_row() {
        let rs = results.borrow();
        if let Some(res) = rs.get(row.index() as usize) {
            match &res.action {
                Action::BrowseInto(p) => {
                    if p.is_dir() {
                        return Some(p.clone());
                    }
                }
                Action::OpenPath(p) => {
                    if let Some(parent) = p.parent() {
                        return Some(parent.to_path_buf());
                    }
                }
                _ => {}
            }
        }
    }

    // 3 — browse-mode path in the entry text.
    let text = entry.text().to_string();
    let expanded = expand_path(&text);
    if let Some(dir) = expanded {
        if dir.is_dir() {
            return Some(dir);
        }
        if let Some(parent) = dir.parent() {
            if parent.is_dir() {
                return Some(parent.to_path_buf());
            }
        }
    }

    // 4 — home directory fallback.
    dirs::home_dir()
}

/// Expand a "~"/"~/..." or absolute path string into a PathBuf.
fn expand_path(text: &str) -> Option<std::path::PathBuf> {
    let t = text.trim();
    if t == "~" {
        return dirs::home_dir();
    }
    if let Some(rest) = t.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if t.starts_with('/') {
        return Some(std::path::PathBuf::from(t));
    }
    None
}

/// Compare two actions for row-equivalence, ignoring the volatile fraction
/// field in op sentinels so that progress-only updates don't trigger a
/// full list rebuild.
fn op_actions_equal(a: &crate::search::Action, b: &crate::search::Action) -> bool {
    match (a, b) {
        (Action::EnterMode(s), Action::EnterMode(t)) if s.starts_with("__op__") && t.starts_with("__op__") => {
            let mut ps = s.split('\u{1f}');
            let mut pt = t.split('\u{1f}');
            // skip __op__ and fraction (first two fields)
            ps.next(); ps.next();
            pt.next(); pt.next();
            ps.eq(pt)
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod private_search_tests {
    use crate::config::Config;
    use crate::search::Action;

    #[test]
    fn the_private_search_shortcut_defaults_to_ctrl_enter() {
        // In the same GTK accelerator form as every other shortcut, so it is
        // matched by the same code path as open-location — which it shares the
        // combination with, deliberately: that one only acts on file rows.
        let cfg = Config::default();
        assert_eq!(cfg.private_search_shortcut, "<Control>Return");
        assert_eq!(cfg.open_location_shortcut, "<Control>Return");
        // …and an already-GTK value survives normalisation untouched.
        assert_eq!(
            crate::ui::settings_window::normalize_accel(&cfg.private_search_shortcut),
            "<Control>Return"
        );
    }

    #[test]
    fn a_private_web_action_is_its_own_action() {
        // Distinct from OpenUrl so the row, the action handler and the tests
        // can tell "open privately" from "open".
        let web = Action::OpenUrl("https://kagi.com/?q=x".into());
        let private = Action::OpenUrlPrivate("https://kagi.com/?q=x".into());
        assert!(matches!(web, Action::OpenUrl(_)));
        assert!(!matches!(private, Action::OpenUrl(_)));
        assert!(matches!(private, Action::OpenUrlPrivate(_)));
    }
}

#[cfg(test)]
mod previewable_tests {
    use super::previewable;
    use crate::search::{Action, ResultKind, SearchResult};

    fn row(action: Action) -> SearchResult {
        SearchResult {
            kind: ResultKind::System,
            title: "row".into(),
            subtitle: None,
            icon: None,
            action,
            score: 0,
        }
    }

    #[test]
    fn update_rows_open_the_preview_pane() {
        // The scope / package rows list what can be updated.
        assert!(previewable(&row(Action::StartOperation {
            title: "Update: vim.x86_64".into(),
            source: "dnf Update".into(),
            icon: "software-update-available-symbolic".into(),
            args: vec![
                "pkexec".into(),
                "dnf".into(),
                "upgrade".into(),
                "-y".into(),
                "vim.x86_64".into()
            ],
        })));
        // …idle recommendations and the checking row do too.
        assert!(previewable(&row(Action::CheckUpdates)));
        // The restart row (reboot) previews its own page.
        assert!(previewable(&row(Action::ConfirmRunCommand(
            crate::search::system::reboot_command()
        ))));
    }

    #[test]
    fn files_and_clipboard_still_preview_and_installs_stay_out() {
        assert!(previewable(&row(Action::CopyToClipboard("hi".into()))));
        assert!(previewable(&row(Action::OpenPath("/tmp/x".into()))));
        // Install/uninstall runs have no update list to show.
        assert!(!previewable(&row(Action::StartOperation {
            title: "Install: Firefox".into(),
            source: "flatpak".into(),
            icon: "org.mozilla.firefox".into(),
            args: vec!["flatpak".into(), "install".into(), "org.mozilla.firefox".into()],
        })));
        assert!(!previewable(&row(Action::EnterMode("files".into()))));
        assert!(!previewable(&row(Action::ToggleUpdates)));
    }
}

#[cfg(test)]
mod swipe_tests {
    use super::*;

    #[test]
    fn hidden_rows_park_by_their_own_width() {
        // A hidden row must clear its own width (plus 2 px), otherwise it
        // peeks out over the undo bar inside the 360 px popover.
        assert_eq!(swipe_hidden_offset(350.0, 1.0), 352.0);
        assert_eq!(swipe_hidden_offset(350.0, -1.0), -352.0);
        assert_eq!(swipe_hidden_offset(100.0, 1.0), 102.0);
        // Before/without a measurement the parking fallback applies — it
        // must clear the 360 px popover even for the widest row.
        assert_eq!(swipe_hidden_offset(0.0, 1.0), SWIPE_PARK_PX);
        assert_eq!(swipe_hidden_offset(0.0, -1.0), -SWIPE_PARK_PX);
        assert!(SWIPE_PARK_PX > 360.0, "must clear the popover width");
    }

    #[test]
    fn pending_bar_slides_out_as_the_row_slides_back_in() {
        let r = 352.0; // measured row width + 2
        // Reverse: dismissed to the right, dragged left.
        assert_eq!(pending_bar_tx(r, 1.0, -140.0), -r);
        assert_eq!(pending_bar_tx(r, 1.0, -70.0), -r / 2.0);
        assert_eq!(pending_bar_tx(r, 1.0, -400.0), -r); // clamped past the commit
        // Reverse, dismissed to the left: the mirror image.
        assert_eq!(pending_bar_tx(r, -1.0, 140.0), r);
        assert_eq!(pending_bar_tx(r, -1.0, 70.0), r / 2.0);
        // Forward (re-dismissing) keeps following the finger.
        assert_eq!(pending_bar_tx(r, 1.0, 30.0), 30.0);
        assert_eq!(pending_bar_tx(r, -1.0, -30.0), -30.0);
    }

    #[test]
    fn swipe_back_never_overlaps_row_and_bar() {
        // The bar's trailing edge must meet the row's leading edge at every
        // point of a reverse swipe — that is what keeps the two texts from
        // painting over each other inside the popover.
        for &r in &[100.0_f64, 352.0, SWIPE_PARK_PX] {
            for step in 0..=10 {
                let offset = -(step as f64) * 14.0; // 0 … -140 (dismissed right)
                let row_left = pending_row_tx(r, 1.0, offset);
                let bar_right = pending_bar_tx(r, 1.0, offset) + r;
                assert!(
                    bar_right <= row_left + 0.001,
                    "overlap: bar right {bar_right} > row left {row_left} at offset {offset}"
                );
            }
        }
    }

    #[test]
    fn peek_bar_never_overlaps_the_row_it_reveals() {
        // While a live row is swiped away the undo bar enters from the opposite
        // side. With `r` = row width + 2 the row spans 2 px of clear space from
        // the bar at every offset — the forward mirror of the swipe-back.
        let r = 352.0;
        let w = r - 2.0;
        assert_eq!(ops_peek_bar_tx(0.0, r), -r, "parked off-screen at rest");
        assert_eq!(
            ops_peek_bar_tx(500.0, r),
            0.0,
            "centred once the row has left"
        );
        for step in 0..=10 {
            let right = step as f64 * 14.0; // 0 … 140
            let bar = ops_peek_bar_tx(right, r);
            assert!(
                bar + w <= right + 0.001,
                "row leaving right overlaps the bar at {right}: bar right {}",
                bar + w
            );
            let left = -right; // mirror image (step 0 has no direction yet)
            if step == 0 {
                continue;
            }
            let bar = ops_peek_bar_tx(left, r);
            assert!(
                bar >= left + w - 0.001,
                "row leaving left overlaps the bar at {left}: bar left {bar}"
            );
        }
    }
}

#[cfg(test)]
mod ops_refresh_tests {
    use super::*;
    use crate::operations::OpItem;

    fn item(op_id: Option<u64>, hist_id: Option<u64>, state: &'static str) -> OpItem {
        OpItem {
            title: "Install: Firefox".into(),
            detail: "flatpak · 42%".into(),
            state,
            icon: "org.mozilla.firefox".into(),
            progress: Some(0.42),
            op_id,
            hist_id,
            dismissed_dir: None,
        }
    }

    #[test]
    fn progress_and_status_only_change_keeps_the_signature() {
        // The popover refreshes ~8x/sec while an operation runs. If these were
        // part of the signature, every tick would rebuild the rows — replaying
        // the undo bar's slide-down and killing any swipe in flight.
        let a = item(Some(7), None, "running");
        let mut b = a.clone();
        b.detail = "flatpak · 91%".into();
        b.progress = Some(0.91);
        assert_eq!(ops_signature(&[a.clone()]), ops_signature(&[b]));

        // Losing determinate progress (pulse on/off) is equally non-structural.
        let mut c = a.clone();
        c.progress = None;
        assert_eq!(ops_signature(&[a.clone()]), ops_signature(&[c]));
    }

    #[test]
    fn structural_changes_alter_the_signature() {
        let base = item(Some(7), None, "running");
        let sig = ops_signature(std::slice::from_ref(&base));

        let mut dismissed = base.clone();
        dismissed.dismissed_dir = Some(1.0);
        assert_ne!(sig, ops_signature(&[dismissed]), "dismissal rebuilds");

        let mut state = base.clone();
        state.state = "done";
        assert_ne!(sig, ops_signature(&[state]), "state change rebuilds");

        assert_ne!(sig, ops_signature(&[item(Some(8), None, "running")]), "id");
        assert_ne!(sig, ops_signature(&[item(None, Some(3), "running")]), "history");

        let mut retitled = base.clone();
        retitled.title = "Install: Thunderbird".into();
        assert_ne!(sig, ops_signature(&[retitled]), "title change rebuilds");

        // Order and membership matter too.
        let a = item(Some(1), None, "running");
        let b = item(Some(2), None, "running");
        assert_ne!(
            ops_signature(&[a.clone(), b.clone()]),
            ops_signature(&[b.clone(), a.clone()])
        );
        assert_ne!(
            ops_signature(std::slice::from_ref(&a)),
            ops_signature(&[a.clone(), b.clone()])
        );
    }
}
