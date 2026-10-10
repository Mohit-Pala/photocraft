//! Touch input on the iPad, as in Photoshop on the iPad and Procreate: the Pencil draws, fingers
//! navigate.
//!
//! egui-winit turns the first touch into the mouse pointer and passes every touch on as
//! `Event::Touch`, from which egui computes pinch zoom over *all* touches, the Pencil's included.
//! So a palm resting on the glass while drawing would zoom, and a two-finger pinch would first
//! paint a dot with its first finger. [`TouchInput::filter`] rewrites each frame's input instead:
//!
//! - **Pencil**: drives the pointer (draws, uses tools), and fingers are ignored while it is down
//!   (palm rejection). Its `Event::Touch` events stay in, so the stylus code still reads their
//!   force if the UIKit Pencil glue isn't there.
//! - **One finger** on the UI drives the pointer (taps, drags, sliders). On the canvas it waits
//!   until it moves, so a second finger can still turn it into a gesture without painting; once
//!   it moves it paints (or uses the current tool), a tap is a click. With **Pencil-only
//!   drawing** (Settings › Apple Pencil › Only Draw with Apple Pencil, or once a Pencil touched the
//!   screen) one finger on the canvas pans instead.
//! - **Two or more fingers** pan (as scroll-wheel input, so panels scroll too) and pinch-zoom
//!   around their centre (`Event::Zoom`). A quick two-finger tap is Undo, three fingers Redo.
//! - **Pencil hover** (from the platform) moves the pointer while nothing touches the screen, so
//!   the brush cursor follows the Pencil.
//!
//! Finger `Event::Touch` events are dropped, so egui never sees more than one touch and never
//! zooms on its own. Frames without touches (a mouse or trackpad on the desktop) pass through
//! unchanged.

use egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchId, TouchPhase, Vec2};

/// A finger on the canvas becomes a stroke after moving this far (points).
const DRAG_SLOP: f32 = 6.0;
/// A gesture is a tap when it ends this soon (seconds) …
const TAP_TIME: f64 = 0.35;
/// … and its fingers moved less than this (points).
const TAP_SLOP: f32 = 12.0;

/// Commands a finger gesture asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// Two-finger tap.
    Undo,
    /// Three-finger tap.
    Redo,
}

impl Shortcut {
    pub fn command(self) -> &'static str {
        match self {
            Self::Undo => "edit.undo",
            Self::Redo => "edit.redo",
        }
    }
}

/// What a contact on the glass does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// Drives egui's pointer.
    Pointer,
    /// A finger on the canvas that hasn't moved yet: a stroke, a click or part of a gesture.
    Pending,
    /// Part of the navigation gesture.
    Gesture,
    /// Ignored until lifted (a palm while the Pencil draws, a finger interrupted by the Pencil).
    Ignored,
}

#[derive(Clone, Copy, Debug)]
struct Contact {
    id: TouchId,
    pos: Pos2,
    start: Pos2,
    pencil: bool,
    role: Role,
}

#[derive(Clone, Copy, Debug)]
struct Gesture {
    /// Centre and mean distance from it of the gesture's fingers, as of the last event.
    centre: Pos2,
    spread: f32,
    /// When the first finger landed, and the most fingers down at once.
    started: f64,
    max_fingers: usize,
    /// Moved too much to be a tap.
    moved: bool,
}

/// Per-frame context from the app.
pub struct Frame<'a> {
    /// The input time (seconds).
    pub now: f64,
    /// Settings › Apple Pencil › Only Draw with Apple Pencil.
    pub pencil_only: bool,
    /// Is this point on the canvas (not on a panel, menu or dialog)?
    pub on_canvas: &'a dyn Fn(Pos2) -> bool,
}

/// The touch state across frames (see the module docs).
#[derive(Debug, Default)]
pub struct TouchInput {
    contacts: Vec<Contact>,
    gesture: Option<Gesture>,
    /// A Pencil touched the screen during this session: fingers navigate the canvas from now on.
    pencil_seen: bool,
    /// A hover update from the platform, applied in the next frame.
    hover: Option<Option<Pos2>>,
    hovering: bool,
}

impl TouchInput {
    /// The Pencil (or a trackpad) hovers at `pos` (egui points), or left (`None`).
    pub fn set_hover(&mut self, pos: Option<Pos2>) {
        self.hover = Some(pos);
    }

    /// Is a contact drawing (driving the pointer) right now?
    pub fn pointer_down(&self) -> bool {
        self.contacts.iter().any(|c| c.role == Role::Pointer)
    }

    /// Rewrite one frame's input events (see the module docs). Returns the shortcuts a gesture
    /// asked for.
    pub fn filter(&mut self, events: &mut Vec<Event>, frame: &Frame<'_>) -> Vec<Shortcut> {
        let touching = !self.contacts.is_empty() || events.iter().any(|e| matches!(e, Event::Touch { .. }));
        let mut out = Vec::with_capacity(events.len() + 4);
        let mut shortcuts = Vec::new();
        if let Some(hover) = self.hover.take()
            && !touching
        {
            match hover {
                Some(p) => {
                    out.push(Event::PointerMoved(p));
                    self.hovering = true;
                }
                None if std::mem::take(&mut self.hovering) => out.push(Event::PointerGone),
                None => {}
            }
        }
        if !touching {
            out.append(events);
            *events = out;
            return shortcuts;
        }
        let pencil_only = frame.pencil_only || self.pencil_seen;
        let mut navigated = false;
        for e in events.drain(..) {
            match e {
                // egui-winit's pointer emulation of the first touch: rebuilt below.
                Event::PointerMoved(_) | Event::PointerButton { .. } | Event::PointerGone => {}
                // A touch somewhere undefined: nothing to point at.
                Event::Touch { pos, .. } if !(pos.x.is_finite() && pos.y.is_finite()) => {}
                Event::Touch { device_id, id, phase, pos, force } => {
                    let pencil = force.is_some() || self.contacts.iter().any(|c| c.id == id && c.pencil);
                    match phase {
                        TouchPhase::Start => {
                            // A touch that never ended (a lost event) ends first.
                            self.end(id, pos, false, frame.now, &mut out, &mut shortcuts);
                            self.start(id, pos, pencil, pencil_only, frame, &mut out);
                        }
                        TouchPhase::Move => navigated |= self.moved(id, pos, &mut out),
                        TouchPhase::End => self.end(id, pos, true, frame.now, &mut out, &mut shortcuts),
                        TouchPhase::Cancel => self.end(id, pos, false, frame.now, &mut out, &mut shortcuts),
                    }
                    if pencil {
                        out.push(Event::Touch { device_id, id, phase, pos, force });
                    }
                }
                other => out.push(other),
            }
        }
        // Once per frame, over all the fingers that moved (UIKit reports them one by one).
        if navigated {
            self.navigate(&mut out);
        }
        *events = out;
        shortcuts
    }

    fn start(&mut self, id: TouchId, pos: Pos2, pencil: bool, pencil_only: bool, frame: &Frame<'_>, out: &mut Vec<Event>) {
        self.hovering = false;
        if pencil {
            self.pencil_seen = true;
            // The Pencil wins: a finger stroke ends, a gesture stops, resting fingers are ignored.
            self.release_pointer(out);
            self.gesture = None;
            for c in &mut self.contacts {
                c.role = Role::Ignored;
            }
            self.contacts.push(Contact { id, pos, start: pos, pencil, role: Role::Pointer });
            press(out, pos);
            return;
        }
        if self.contacts.iter().any(|c| c.pencil) {
            // A palm while the Pencil draws.
            self.contacts.push(Contact { id, pos, start: pos, pencil, role: Role::Ignored });
            return;
        }
        let fingers = self.contacts.iter().filter(|c| c.role != Role::Ignored).count() + 1;
        if fingers >= 2 {
            // A second finger: whatever the first one did becomes a navigation gesture (a finger
            // stroke ends where it is).
            for c in &mut self.contacts {
                match c.role {
                    Role::Pointer => {
                        release(out, c.pos);
                        c.role = Role::Gesture;
                    }
                    Role::Pending => c.role = Role::Gesture,
                    Role::Gesture | Role::Ignored => {}
                }
            }
            self.contacts.push(Contact { id, pos, start: pos, pencil, role: Role::Gesture });
            let started = self.gesture.map_or(frame.now, |g| g.started);
            let moved = self.gesture.is_some_and(|g| g.moved);
            self.rebase(started, moved, fingers);
            if let Some(g) = self.gesture {
                out.push(Event::PointerMoved(g.centre));
            }
        } else if (frame.on_canvas)(pos) {
            let role = if pencil_only { Role::Gesture } else { Role::Pending };
            self.contacts.push(Contact { id, pos, start: pos, pencil, role });
            if role == Role::Gesture {
                self.rebase(frame.now, false, 1);
            }
            out.push(Event::PointerMoved(pos));
        } else {
            self.contacts.push(Contact { id, pos, start: pos, pencil, role: Role::Pointer });
            press(out, pos);
        }
    }

    /// A contact moved; returns whether it navigates (see [`Self::navigate`]).
    fn moved(&mut self, id: TouchId, pos: Pos2, out: &mut Vec<Event>) -> bool {
        let Some(c) = self.contacts.iter_mut().find(|c| c.id == id) else { return false };
        c.pos = pos;
        match c.role {
            Role::Pointer => out.push(Event::PointerMoved(pos)),
            Role::Pending if c.start.distance(pos) > DRAG_SLOP => {
                c.role = Role::Pointer;
                let start = c.start;
                press(out, start);
                out.push(Event::PointerMoved(pos));
            }
            Role::Gesture => return true,
            Role::Pending | Role::Ignored => {}
        }
        false
    }

    fn end(&mut self, id: TouchId, pos: Pos2, ended: bool, now: f64, out: &mut Vec<Event>, shortcuts: &mut Vec<Shortcut>) {
        let Some(i) = self.contacts.iter().position(|c| c.id == id) else { return };
        let c = self.contacts.remove(i);
        match c.role {
            Role::Pointer => {
                out.push(Event::PointerMoved(pos));
                release(out, pos);
                out.push(Event::PointerGone);
            }
            // A tap on the canvas is a click.
            Role::Pending if ended => {
                press(out, c.start);
                release(out, c.start);
                out.push(Event::PointerGone);
            }
            Role::Gesture => {
                let left = self.contacts.iter().filter(|c| c.role == Role::Gesture).count();
                if left == 0 {
                    if let Some(g) = self.gesture.take()
                        && ended
                        && !g.moved
                        && now - g.started < TAP_TIME
                    {
                        match g.max_fingers {
                            2 => shortcuts.push(Shortcut::Undo),
                            3 => shortcuts.push(Shortcut::Redo),
                            _ => {}
                        }
                    }
                    out.push(Event::PointerGone);
                } else if let Some(g) = self.gesture {
                    self.rebase(g.started, g.moved, g.max_fingers);
                }
            }
            Role::Pending | Role::Ignored => {}
        }
    }

    /// End the pointer's press (a finger stroke interrupted by the Pencil) and ignore the
    /// fingers.
    fn release_pointer(&mut self, out: &mut Vec<Event>) {
        for c in &mut self.contacts {
            match c.role {
                Role::Pointer => {
                    release(out, c.pos);
                    c.role = Role::Ignored;
                }
                Role::Pending => c.role = Role::Ignored,
                _ => {}
            }
        }
    }

    /// Centre and spread of the gesture's fingers.
    fn shape(&self) -> Option<(Pos2, f32)> {
        let pts: Vec<Pos2> = self.contacts.iter().filter(|c| c.role == Role::Gesture).map(|c| c.pos).collect();
        if pts.is_empty() {
            return None;
        }
        let n = pts.len() as f32;
        let centre = Pos2::ZERO + pts.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2()) / n;
        let spread = pts.iter().map(|p| p.distance(centre)).sum::<f32>() / n;
        Some((centre, spread))
    }

    /// Restart the gesture's reference shape (a finger landed or lifted), without moving the view.
    fn rebase(&mut self, started: f64, moved: bool, fingers: usize) {
        self.gesture = self.shape().map(|(centre, spread)| Gesture {
            centre,
            spread,
            started,
            max_fingers: self.gesture.map_or(fingers, |g| g.max_fingers.max(fingers)),
            moved,
        });
    }

    /// Pan and pinch-zoom by the gesture's change since the last event.
    fn navigate(&mut self, out: &mut Vec<Event>) {
        let (Some(g), Some((centre, spread))) = (self.gesture, self.shape()) else { return };
        let delta = centre - g.centre;
        if delta != Vec2::ZERO {
            out.push(Event::PointerMoved(centre));
            out.push(Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase: TouchPhase::Move, modifiers: Modifiers::NONE });
        }
        let fingers = self.contacts.iter().filter(|c| c.role == Role::Gesture).count();
        if fingers >= 2 && g.spread > 1.0 && spread > 1.0 {
            let zoom = spread / g.spread;
            if zoom.is_finite() && (zoom - 1.0).abs() > 1e-4 {
                out.push(Event::Zoom(zoom));
            }
        }
        let travelled = self.contacts.iter().filter(|c| c.role == Role::Gesture).any(|c| c.start.distance(c.pos) > TAP_SLOP);
        self.gesture = Some(Gesture { centre, spread, moved: g.moved || travelled, ..g });
    }
}

fn press(out: &mut Vec<Event>, pos: Pos2) {
    out.push(Event::PointerMoved(pos));
    out.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
}

fn release(out: &mut Vec<Event>, pos: Pos2) {
    out.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{TouchDeviceId, pos2};

    fn touch(id: u64, phase: TouchPhase, x: f32, y: f32, pencil: bool) -> Event {
        Event::Touch { device_id: TouchDeviceId(1), id: TouchId(id), phase, pos: pos2(x, y), force: pencil.then_some(0.5) }
    }

    /// The canvas is everything right of x = 100.
    fn run(t: &mut TouchInput, now: f64, pencil_only: bool, events: Vec<Event>) -> (Vec<Event>, Vec<Shortcut>) {
        let on_canvas = |p: Pos2| p.x > 100.0;
        let mut events = events;
        let s = t.filter(&mut events, &Frame { now, pencil_only, on_canvas: &on_canvas });
        (events, s)
    }

    fn presses(events: &[Event]) -> Vec<bool> {
        events.iter().filter_map(|e| if let Event::PointerButton { pressed, .. } = e { Some(*pressed) } else { None }).collect()
    }

    fn zoom(events: &[Event]) -> f32 {
        events.iter().filter_map(|e| if let Event::Zoom(z) = e { Some(*z) } else { None }).product()
    }

    fn pan(events: &[Event]) -> Vec2 {
        events.iter().filter_map(|e| if let Event::MouseWheel { delta, .. } = e { Some(*delta) } else { None }).fold(Vec2::ZERO, |a, d| a + d)
    }

    #[test]
    fn mouse_frames_pass_through() {
        let mut t = TouchInput::default();
        let input = vec![
            Event::PointerMoved(pos2(5.0, 5.0)),
            Event::PointerButton { pos: pos2(5.0, 5.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
        ];
        let (out, s) = run(&mut t, 0.0, false, input.clone());
        assert_eq!(out, input);
        assert!(s.is_empty());
    }

    #[test]
    fn the_pencil_draws_and_keeps_its_touch_events() {
        let mut t = TouchInput::default();
        // egui-winit's emulated pointer events are replaced by ours.
        let emulated = Event::PointerButton { pos: pos2(0.0, 0.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
        let (out, _) = run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 50.0, true), emulated]);
        assert_eq!(presses(&out), vec![true]);
        assert!(out.iter().any(|e| matches!(e, Event::Touch { force: Some(_), .. })), "the stylus code reads the force");
        assert!(t.pointer_down());
        let (out, _) = run(&mut t, 0.1, false, vec![touch(1, TouchPhase::Move, 210.0, 55.0, true)]);
        assert!(out.contains(&Event::PointerMoved(pos2(210.0, 55.0))));
        let (out, _) = run(&mut t, 0.2, false, vec![touch(1, TouchPhase::End, 212.0, 56.0, true)]);
        assert_eq!(presses(&out), vec![false]);
        assert!(out.contains(&Event::PointerGone));
        assert!(!t.pointer_down());
    }

    #[test]
    fn a_palm_while_drawing_is_ignored() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 50.0, true)]);
        let (out, _) = run(&mut t, 0.0, false, vec![touch(2, TouchPhase::Start, 300.0, 300.0, false), touch(3, TouchPhase::Start, 340.0, 320.0, false)]);
        assert!(presses(&out).is_empty());
        let (out, _) = run(&mut t, 0.1, false, vec![touch(2, TouchPhase::Move, 330.0, 330.0, false), touch(3, TouchPhase::Move, 400.0, 400.0, false)]);
        assert_eq!(zoom(&out), 1.0, "no pinch from a palm");
        assert_eq!(pan(&out), Vec2::ZERO);
        assert!(!out.iter().any(|e| matches!(e, Event::Touch { force: None, .. })), "finger touches never reach egui");
    }

    #[test]
    fn a_finger_on_the_ui_clicks_at_once() {
        let mut t = TouchInput::default();
        let (out, _) = run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 50.0, 50.0, false)]);
        assert_eq!(presses(&out), vec![true]);
        let (out, _) = run(&mut t, 0.1, false, vec![touch(1, TouchPhase::End, 50.0, 50.0, false)]);
        assert_eq!(presses(&out), vec![false]);
    }

    #[test]
    fn a_finger_on_the_canvas_paints_once_it_moves() {
        let mut t = TouchInput::default();
        let (out, _) = run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false)]);
        assert!(presses(&out).is_empty(), "waits: a second finger may still come");
        let (out, _) = run(&mut t, 0.05, false, vec![touch(1, TouchPhase::Move, 202.0, 200.0, false)]);
        assert!(presses(&out).is_empty(), "within the slop");
        let (out, _) = run(&mut t, 0.1, false, vec![touch(1, TouchPhase::Move, 220.0, 200.0, false)]);
        assert_eq!(presses(&out), vec![true]);
        let i = out.iter().position(|e| matches!(e, Event::PointerButton { .. })).unwrap();
        assert!(matches!(out[i], Event::PointerButton { pos, .. } if pos == pos2(200.0, 200.0)), "the stroke starts where the finger landed");
        let (out, _) = run(&mut t, 0.2, false, vec![touch(1, TouchPhase::End, 220.0, 200.0, false)]);
        assert_eq!(presses(&out), vec![false]);
    }

    #[test]
    fn a_tap_on_the_canvas_is_a_click() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false)]);
        let (out, _) = run(&mut t, 0.1, false, vec![touch(1, TouchPhase::End, 200.0, 200.0, false)]);
        assert_eq!(presses(&out), vec![true, false]);
    }

    #[test]
    fn two_fingers_pan_and_pinch_without_painting() {
        let mut t = TouchInput::default();
        let (out, _) = run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false), touch(2, TouchPhase::Start, 300.0, 200.0, false)]);
        assert!(presses(&out).is_empty());
        // Both move right by 20: a pan, no zoom.
        let (out, _) = run(&mut t, 0.1, false, vec![touch(1, TouchPhase::Move, 220.0, 200.0, false), touch(2, TouchPhase::Move, 320.0, 200.0, false)]);
        assert_eq!(pan(&out), Vec2::new(20.0, 0.0));
        assert!((zoom(&out) - 1.0).abs() < 1e-4);
        assert!(presses(&out).is_empty());
        // Spread from 100 to 200 apart: ×2 zoom.
        let (out, _) = run(&mut t, 0.2, false, vec![touch(1, TouchPhase::Move, 170.0, 200.0, false), touch(2, TouchPhase::Move, 370.0, 200.0, false)]);
        assert!((zoom(&out) - 2.0).abs() < 1e-3, "{}", zoom(&out));
        let (_, s) = run(&mut t, 0.3, false, vec![touch(1, TouchPhase::End, 170.0, 200.0, false), touch(2, TouchPhase::End, 370.0, 200.0, false)]);
        assert!(s.is_empty(), "a pinch is no tap");
    }

    #[test]
    fn a_second_finger_ends_a_finger_stroke() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 50.0, 50.0, false)]);
        let (out, _) = run(&mut t, 0.1, false, vec![touch(2, TouchPhase::Start, 80.0, 50.0, false)]);
        assert_eq!(presses(&out), vec![false]);
        assert!(!t.pointer_down());
    }

    #[test]
    fn two_and_three_finger_taps_undo_and_redo() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false), touch(2, TouchPhase::Start, 260.0, 200.0, false)]);
        let (_, s) = run(&mut t, 0.15, false, vec![touch(1, TouchPhase::End, 200.0, 200.0, false), touch(2, TouchPhase::End, 260.0, 200.0, false)]);
        assert_eq!(s, vec![Shortcut::Undo]);

        let fingers = |phase| (1..=3).map(|i| touch(i, phase, 150.0 + 40.0 * i as f32, 200.0, false)).collect::<Vec<_>>();
        run(&mut t, 1.0, false, fingers(TouchPhase::Start));
        let (_, s) = run(&mut t, 1.2, false, fingers(TouchPhase::End));
        assert_eq!(s, vec![Shortcut::Redo]);

        // Too slow: no tap.
        run(&mut t, 2.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false), touch(2, TouchPhase::Start, 260.0, 200.0, false)]);
        let (_, s) = run(&mut t, 3.0, false, vec![touch(1, TouchPhase::End, 200.0, 200.0, false), touch(2, TouchPhase::End, 260.0, 200.0, false)]);
        assert!(s.is_empty());
        // Cancelled by the system: no tap.
        run(&mut t, 4.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false), touch(2, TouchPhase::Start, 260.0, 200.0, false)]);
        let (_, s) = run(&mut t, 4.1, false, vec![touch(1, TouchPhase::Cancel, 200.0, 200.0, false), touch(2, TouchPhase::Cancel, 260.0, 200.0, false)]);
        assert!(s.is_empty());
    }

    #[test]
    fn pencil_only_drawing_makes_one_finger_pan() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, true, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false)]);
        let (out, _) = run(&mut t, 0.1, true, vec![touch(1, TouchPhase::Move, 240.0, 230.0, false)]);
        assert!(presses(&out).is_empty());
        assert_eq!(pan(&out), Vec2::new(40.0, 30.0));
        run(&mut t, 0.2, true, vec![touch(1, TouchPhase::End, 240.0, 230.0, false)]);
        // The UI still takes finger taps.
        let (out, _) = run(&mut t, 1.0, true, vec![touch(2, TouchPhase::Start, 50.0, 50.0, false)]);
        assert_eq!(presses(&out), vec![true]);
    }

    #[test]
    fn after_the_pencil_fingers_navigate_the_canvas() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, true)]);
        run(&mut t, 0.1, false, vec![touch(1, TouchPhase::End, 200.0, 200.0, true)]);
        run(&mut t, 1.0, false, vec![touch(2, TouchPhase::Start, 200.0, 200.0, false)]);
        let (out, _) = run(&mut t, 1.1, false, vec![touch(2, TouchPhase::Move, 260.0, 200.0, false)]);
        assert!(presses(&out).is_empty());
        assert_eq!(pan(&out), Vec2::new(60.0, 0.0));
    }

    #[test]
    fn the_pencil_interrupts_a_finger_stroke() {
        let mut t = TouchInput::default();
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, false)]);
        run(&mut t, 0.1, false, vec![touch(1, TouchPhase::Move, 240.0, 200.0, false)]);
        let (out, _) = run(&mut t, 0.2, false, vec![touch(2, TouchPhase::Start, 400.0, 400.0, true)]);
        assert_eq!(presses(&out), vec![false, true], "the finger stroke ends, the Pencil's begins");
        let (out, _) = run(&mut t, 0.3, false, vec![touch(1, TouchPhase::End, 240.0, 200.0, false)]);
        assert!(presses(&out).is_empty(), "the lifted finger no longer counts");
        assert!(t.pointer_down());
    }

    #[test]
    fn hover_moves_the_pointer_only_while_nothing_touches() {
        let mut t = TouchInput::default();
        t.set_hover(Some(pos2(10.0, 20.0)));
        let (out, _) = run(&mut t, 0.0, false, vec![]);
        assert_eq!(out, vec![Event::PointerMoved(pos2(10.0, 20.0))]);
        t.set_hover(None);
        let (out, _) = run(&mut t, 0.1, false, vec![]);
        assert_eq!(out, vec![Event::PointerGone]);
        // A hover update in the frame the Pencil lands is dropped.
        t.set_hover(Some(pos2(10.0, 20.0)));
        let (out, _) = run(&mut t, 0.2, false, vec![touch(1, TouchPhase::Start, 200.0, 200.0, true)]);
        assert!(!out.contains(&Event::PointerMoved(pos2(10.0, 20.0))));
    }

    #[test]
    fn unknown_or_repeated_touches_never_panic() {
        let mut t = TouchInput::default();
        let (out, s) = run(&mut t, 0.0, false, vec![touch(9, TouchPhase::Move, 1.0, 1.0, false), touch(9, TouchPhase::End, 1.0, 1.0, false)]);
        assert!(presses(&out).is_empty() && s.is_empty());
        run(&mut t, 0.0, false, vec![touch(1, TouchPhase::Start, f32::NAN, f32::INFINITY, false), touch(1, TouchPhase::Start, 200.0, 200.0, false)]);
        let (out, _) = run(&mut t, 0.1, false, vec![touch(1, TouchPhase::End, 200.0, 200.0, false)]);
        assert_eq!(presses(&out), vec![true, false]);
        assert!(t.contacts.is_empty());
    }
}
