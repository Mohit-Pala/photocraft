//! Apple Pencil on iPad: the pure mapping from the values UIKit reports (`UITouch`,
//! `UIPencilInteraction`) to [`Sample`]s and [`PencilEvent`]s. The UIKit glue in `ios.rs` only
//! reads raw values and hands them here, so this is tested on every platform.
//!
//! - **Pressure**: `force / maximumPossibleForce`, 0..1.
//! - **Tilt**: UIKit gives the pen's `altitudeAngle` (0 = lying on the glass, π/2 = upright) and
//!   `azimuthAngleInView:` (the direction the barrel leans, 0 = towards +x, clockwise with y
//!   down). [`tilt`] converts them to the W3C Pointer Events `tiltX` / `tiltY` the canvas uses.
//! - **Rotation**: `rollAngle` (Apple Pencil Pro, iPadOS 17.5), radians → degrees 0..360.
//! - **Double-tap / squeeze**: reported with the action the user chose in Settings › Apple
//!   Pencil ([`PencilAction`]); the app decides what each action does.

use crate::Sample;

/// `UITouchType` raw values.
pub mod touch_type {
    /// A finger on the screen.
    pub const DIRECT: isize = 0;
    pub const INDIRECT: isize = 1;
    /// Apple Pencil (and other styluses UIKit reports as pencils).
    pub const PENCIL: isize = 2;
    /// A trackpad or mouse pointer (only with `UIApplicationSupportsIndirectInputEvents`).
    pub const INDIRECT_POINTER: isize = 3;
}

/// One Apple Pencil reading, as UIKit reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawPencil {
    /// `UITouch.force`.
    pub force: f64,
    /// `UITouch.maximumPossibleForce`.
    pub max_force: f64,
    /// `altitudeAngle`, radians: 0 = lying flat on the glass, π/2 = upright.
    pub altitude: f64,
    /// `azimuthAngleInView:`, radians: the direction the barrel leans, 0 = towards +x,
    /// increasing clockwise (UIKit's y axis points down).
    pub azimuth: f64,
    /// `rollAngle` (Apple Pencil Pro on iPadOS 17.5 and later), radians; `None` without one.
    pub roll: Option<f64>,
}

/// The pen sample for a Pencil reading.
pub fn sample(raw: &RawPencil) -> Sample {
    // No force reading (max 0, or non-finite values): paint at full strength, like a mouse.
    let pressure = if raw.max_force.is_finite() && raw.max_force > 0.0 && raw.force.is_finite() { raw.force / raw.max_force } else { 1.0 };
    let (tilt_x, tilt_y) = tilt(raw.altitude, raw.azimuth);
    let rotation = raw.roll.filter(|r| r.is_finite()).map_or(0.0, f64::to_degrees);
    Sample { pressure: pressure as f32, tilt_x, tilt_y, rotation: rotation as f32, eraser: false }.sanitized()
}

/// W3C Pointer Events tilt (degrees, -90..90; +x tilts the top of the pen to the right, +y
/// towards the user) from UIKit's altitude and azimuth (radians). This is the Pointer Events
/// spec's conversion, `tiltX = atan(cos(azimuth) / tan(altitude))`, written with `atan2` so a
/// pen lying flat (altitude 0) gives ±90° instead of dividing by zero.
pub fn tilt(altitude: f64, azimuth: f64) -> (f32, f32) {
    if !altitude.is_finite() || !azimuth.is_finite() {
        return (0.0, 0.0);
    }
    let altitude = altitude.clamp(0.0, std::f64::consts::FRAC_PI_2);
    let (sin_alt, cos_alt) = altitude.sin_cos();
    let (sin_az, cos_az) = azimuth.sin_cos();
    // `cos(π/2)` is 6e-17, not 0: without snapping, a flat pen leaning along y would read
    // tiltX = 90° (`atan2(6e-17, 0)`) instead of the spec's 0.
    let snap = |v: f64| if v.abs() < 1e-9 { 0.0 } else { v };
    let x = snap(cos_az * cos_alt).atan2(sin_alt).to_degrees();
    let y = snap(sin_az * cos_alt).atan2(sin_alt).to_degrees();
    (x as f32, y as f32)
}

/// What a touch-down or touch-move means for the pen sample: `Some(Some(_))` while the Pencil
/// is down, `Some(None)` when a finger (or pointer) starts a touch with no Pencil on the glass,
/// `None` to keep the current sample. A lifted Pencil keeps its last sample, so the stroke's
/// final points (handled a moment later) keep their pressure and tilt; the next touch replaces
/// it. `touches` holds each changed touch's `UITouchType` raw value and reading.
pub fn on_touches(touches: &[(isize, RawPencil)], began: bool) -> Option<Option<Sample>> {
    match touches.iter().find(|(kind, _)| *kind == touch_type::PENCIL) {
        Some((_, raw)) => Some(Some(sample(raw))),
        None if began && !touches.is_empty() => Some(None),
        None => None,
    }
}

/// The user's preferred action for a Pencil double-tap or squeeze (Settings › Apple Pencil),
/// `UIPencilPreferredAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PencilAction {
    /// "Off": do nothing.
    Ignore,
    /// Switch between the current tool and the eraser.
    SwitchEraser,
    /// Switch between the current tool and the last one used.
    SwitchPrevious,
    /// Show the colour palette.
    ShowColorPalette,
    /// Show the current tool's settings (ink attributes).
    ShowInkAttributes,
    /// Show a contextual palette (squeeze, Apple Pencil Pro).
    ShowContextualPalette,
    /// Run a system shortcut (handled by the system; the app does nothing).
    RunSystemShortcut,
}

impl PencilAction {
    /// From the raw `UIPencilPreferredAction` value; values newer than this list are ignored.
    pub fn from_raw(value: isize) -> Self {
        match value {
            1 => Self::SwitchEraser,
            2 => Self::SwitchPrevious,
            3 => Self::ShowColorPalette,
            4 => Self::ShowInkAttributes,
            5 => Self::ShowContextualPalette,
            6 => Self::RunSystemShortcut,
            _ => Self::Ignore,
        }
    }
}

/// What the iPad's Pencil glue reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PencilEvent {
    /// The pen sample changed: `Some` while the Pencil touches the screen, `None` when a finger
    /// starts a touch (see [`on_touches`]).
    Sample(Option<Sample>),
    /// The Pencil (or a trackpad pointer) hovers over the view at this point, in the view's
    /// points; `None` when it leaves or touches down. UIKit hover needs iPadOS 16.1 and a
    /// hover-capable iPad (M2 iPad Pro and later) for the Pencil.
    Hover(Option<[f32; 2]>),
    /// Double-tap on the Pencil's barrel, with the user's preferred action.
    Tap(PencilAction),
    /// Squeeze (Apple Pencil Pro), with the user's preferred action; reported once, on release.
    Squeeze(PencilAction),
}

/// Run a user callback without letting a panic unwind into UIKit (an unwind through an
/// Objective-C frame aborts the app).
pub fn deliver(callback: &dyn Fn(PencilEvent), event: PencilEvent) {
    let event = match event {
        PencilEvent::Sample(s) => PencilEvent::Sample(s.map(Sample::sanitized)),
        PencilEvent::Hover(p) => PencilEvent::Hover(p.filter(|[x, y]| x.is_finite() && y.is_finite())),
        other => other,
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(event)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn upright_pencil_has_no_tilt() {
        for az in [0.0, 1.0, PI, -2.0] {
            let (x, y) = tilt(FRAC_PI_2, az);
            assert!(near(x, 0.0) && near(y, 0.0), "azimuth {az}: {x} {y}");
        }
    }

    #[test]
    fn tilt_follows_the_azimuth() {
        // Leaning 45° to the right (+x), towards the user (+y, UIKit's y is down), to the left.
        let (x, y) = tilt(FRAC_PI_4, 0.0);
        assert!(near(x, 45.0) && near(y, 0.0), "{x} {y}");
        let (x, y) = tilt(FRAC_PI_4, FRAC_PI_2);
        assert!(near(x, 0.0) && near(y, 45.0), "{x} {y}");
        let (x, y) = tilt(FRAC_PI_4, PI);
        assert!(near(x, -45.0) && near(y, 0.0), "{x} {y}");
        // Diagonal: the W3C conversion, atan(cos(az) / tan(alt)).
        let (x, y) = tilt(FRAC_PI_4, FRAC_PI_4);
        let w3c = (FRAC_PI_4.cos() / FRAC_PI_4.tan()).atan().to_degrees() as f32;
        assert!(near(x, w3c) && near(y, w3c), "{x} {y} vs {w3c}");
    }

    #[test]
    fn a_pencil_lying_flat_or_bad_angles_stay_in_range() {
        let (x, y) = tilt(0.0, 0.0);
        assert!(near(x, 90.0) && near(y, 0.0), "{x} {y}");
        let (x, y) = tilt(0.0, -FRAC_PI_2);
        assert!(near(x, 0.0) && near(y, -90.0), "{x} {y}");
        assert_eq!(tilt(f64::NAN, 0.0), (0.0, 0.0));
        assert_eq!(tilt(1.0, f64::INFINITY), (0.0, 0.0));
        // Out-of-range altitudes are clamped, not mirrored.
        let (x, _) = tilt(-1.0, 0.0);
        assert!(near(x, 90.0), "{x}");
    }

    #[test]
    fn pressure_is_force_over_the_maximum() {
        let raw = RawPencil { force: 1.0, max_force: 4.0, altitude: FRAC_PI_2, ..Default::default() };
        assert!(near(sample(&raw).pressure, 0.25));
        // Harder than the maximum clamps; no maximum (or NaN) paints like a mouse.
        assert_eq!(sample(&RawPencil { force: 9.0, ..raw }).pressure, 1.0);
        assert_eq!(sample(&RawPencil { max_force: 0.0, ..raw }).pressure, 1.0);
        assert_eq!(sample(&RawPencil { force: f64::NAN, ..raw }).pressure, 1.0);
        assert!(!sample(&raw).eraser, "the Pencil has no eraser end");
    }

    #[test]
    fn roll_becomes_rotation_in_degrees() {
        let raw = RawPencil { force: 1.0, max_force: 1.0, altitude: FRAC_PI_2, roll: Some(FRAC_PI_2), ..Default::default() };
        assert!(near(sample(&raw).rotation, 90.0));
        assert!(near(sample(&RawPencil { roll: Some(-FRAC_PI_2), ..raw }).rotation, 270.0));
        assert_eq!(sample(&RawPencil { roll: None, ..raw }).rotation, 0.0);
        assert_eq!(sample(&RawPencil { roll: Some(f64::NAN), ..raw }).rotation, 0.0);
    }

    #[test]
    fn touches_set_and_clear_the_sample() {
        let pencil = RawPencil { force: 2.0, max_force: 4.0, altitude: FRAC_PI_2, ..Default::default() };
        let finger = RawPencil::default();
        let down = on_touches(&[(touch_type::PENCIL, pencil)], true);
        assert!(matches!(down, Some(Some(s)) if near(s.pressure, 0.5)));
        // A finger starting a touch clears the sample, a finger moving keeps it.
        assert_eq!(on_touches(&[(touch_type::DIRECT, finger)], true), Some(None));
        assert_eq!(on_touches(&[(touch_type::DIRECT, finger)], false), None);
        // The Pencil wins when a finger lands in the same event.
        assert!(matches!(on_touches(&[(touch_type::DIRECT, finger), (touch_type::PENCIL, pencil)], true), Some(Some(_))));
        assert_eq!(on_touches(&[], true), None);
    }

    #[test]
    fn preferred_actions_map_from_raw_values() {
        assert_eq!(PencilAction::from_raw(0), PencilAction::Ignore);
        assert_eq!(PencilAction::from_raw(1), PencilAction::SwitchEraser);
        assert_eq!(PencilAction::from_raw(2), PencilAction::SwitchPrevious);
        assert_eq!(PencilAction::from_raw(3), PencilAction::ShowColorPalette);
        assert_eq!(PencilAction::from_raw(6), PencilAction::RunSystemShortcut);
        assert_eq!(PencilAction::from_raw(99), PencilAction::Ignore);
        assert_eq!(PencilAction::from_raw(-1), PencilAction::Ignore);
    }

    #[test]
    fn deliver_sanitizes_and_contains_panics() {
        let seen = std::cell::Cell::new(None);
        deliver(&|e| seen.set(Some(e)), PencilEvent::Sample(Some(Sample { pressure: 3.0, ..Default::default() })));
        assert!(matches!(seen.get(), Some(PencilEvent::Sample(Some(s))) if s.pressure == 1.0));
        deliver(&|e| seen.set(Some(e)), PencilEvent::Hover(Some([f32::NAN, 1.0])));
        assert_eq!(seen.get(), Some(PencilEvent::Hover(None)));
        deliver(&|_| panic!("boom"), PencilEvent::Tap(PencilAction::SwitchEraser));
    }
}
