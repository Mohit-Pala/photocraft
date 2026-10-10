//! Apple Pencil on iPad: UIKit glue. Besides `macos.rs`, the only module in the workspace that
//! allows `unsafe`: subclassing UIKit classes and calling their superclass needs Objective-C
//! interop that objc2 can't prove sound on its own (each block has a `SAFETY:` comment).
//! Everything read here goes through the pure, tested mapping in [`crate::uikit`].
//!
//! winit 0.30 forwards the Pencil's force as touch force (so pressure works without this), but
//! drops its azimuth, roll, hover and the barrel's double-tap and squeeze. [`Pencil::install`]
//! adds three things to winit's `UIView`:
//!
//! - a *touch observer*, a gesture recognizer that never recognizes anything: UIKit hands it
//!   every touch before the view, so the pen sample is current when the UI handles the touch.
//!   It neither cancels nor delays the view's touches;
//! - a `UIHoverGestureRecognizer`: the Pencil hovering over the screen (iPadOS 16.1, M2 iPad Pro
//!   and later), or a trackpad pointer;
//! - a `UIPencilInteraction`: double-tap (and squeeze on Apple Pencil Pro, iPadOS 17.5), with the
//!   action the user picked in Settings › Apple Pencil.

#![allow(unsafe_code)]

use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, available, define_class, msg_send, sel};
use objc2_foundation::NSSet;
use objc2_ui_kit::{
    UIEvent, UIGestureRecognizer, UIGestureRecognizerState, UIHoverGestureRecognizer, UIPencilInteraction, UIPencilInteractionDelegate,
    UIPencilInteractionPhase, UIPencilInteractionSqueeze, UIPencilInteractionTap, UITouch, UIView,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::Error;
use crate::uikit::{PencilAction, PencilEvent, RawPencil, deliver, on_touches};

type Callback = Rc<dyn Fn(PencilEvent)>;

/// Keeps the Pencil glue installed on the view; dropping it removes it. Main-thread only (not
/// `Send`).
pub struct Pencil {
    view: Retained<UIView>,
    observer: Retained<TouchObserver>,
    hover: Retained<UIHoverGestureRecognizer>,
    _hover_target: Retained<HoverTarget>,
    interaction: Retained<UIPencilInteraction>,
    _delegate: Retained<PencilDelegate>,
}

impl Pencil {
    /// Install on `window`'s `UIView` (eframe's creation context or frame). `callback` runs on
    /// the main thread, inside UIKit's touch handling, before winit sees the same touch.
    pub fn install(window: &impl HasWindowHandle, callback: impl Fn(PencilEvent) + 'static) -> Result<Self, Error> {
        let mtm = MainThreadMarker::new().ok_or(Error::NotMainThread)?;
        let handle = window.window_handle().map_err(|e| Error::Platform(format!("no window handle: {e}")))?;
        let RawWindowHandle::UiKit(ui) = handle.as_raw() else {
            return Err(Error::Unsupported("not a UIKit window".into()));
        };
        // SAFETY: `ui_view` of a `UiKitWindowHandle` is a valid `UIView` for as long as the
        // handle is borrowed (raw-window-handle's contract), and we take our own strong
        // reference here, before that borrow ends.
        let view: Retained<UIView> =
            unsafe { Retained::retain(ui.ui_view.as_ptr().cast::<UIView>()) }.ok_or_else(|| Error::Platform("the window has no UIView".into()))?;
        let callback: Callback = Rc::new(callback);

        let observer = TouchObserver::new(mtm, callback.clone());
        observer.setCancelsTouchesInView(false);
        observer.setDelaysTouchesBegan(false);
        observer.setDelaysTouchesEnded(false);
        view.addGestureRecognizer(&observer);

        let hover_target = HoverTarget::new(mtm, callback.clone());
        let target: &AnyObject = &hover_target;
        // SAFETY: `hover_target` implements `hover:` taking the recognizer (below), and outlives
        // the recognizer, which only holds it weakly: both live in `Pencil`, which removes the
        // recognizer from the view when dropped.
        let hover = unsafe { UIHoverGestureRecognizer::initWithTarget_action(UIHoverGestureRecognizer::alloc(mtm), Some(target), Some(sel!(hover:))) };
        hover.setCancelsTouchesInView(false);
        view.addGestureRecognizer(&hover);

        let delegate = PencilDelegate::new(mtm, callback);
        let interaction = UIPencilInteraction::initWithDelegate(UIPencilInteraction::alloc(mtm), ProtocolObject::from_ref(&*delegate));
        view.addInteraction(ProtocolObject::from_ref(&*interaction));

        Ok(Self { view, observer, hover, _hover_target: hover_target, interaction, _delegate: delegate })
    }
}

impl Drop for Pencil {
    fn drop(&mut self) {
        self.view.removeGestureRecognizer(&self.observer);
        self.view.removeGestureRecognizer(&self.hover);
        self.view.removeInteraction(ProtocolObject::from_ref(&*self.interaction));
    }
}

/// Settings › Apple Pencil › Only Draw with Apple Pencil (iPadOS 17.5): fingers navigate and
/// only the Pencil draws. `false` on older systems and off the main thread.
pub fn prefers_pencil_only_drawing() -> bool {
    match MainThreadMarker::new() {
        Some(mtm) if available!(ios = 17.5) => UIPencilInteraction::prefersPencilOnlyDrawing(mtm),
        _ => false,
    }
}

/// The user's preferred double-tap action (Settings › Apple Pencil).
fn preferred_tap_action(mtm: MainThreadMarker) -> PencilAction {
    PencilAction::from_raw(UIPencilInteraction::preferredTapAction(mtm).0)
}

/// The user's preferred squeeze action (Apple Pencil Pro, iPadOS 17.5).
fn preferred_squeeze_action(mtm: MainThreadMarker) -> PencilAction {
    if available!(ios = 17.5) { PencilAction::from_raw(UIPencilInteraction::preferredSqueezeAction(mtm).0) } else { PencilAction::Ignore }
}

struct Shared {
    callback: Callback,
}

impl Shared {
    fn emit(&self, event: PencilEvent) {
        deliver(&*self.callback, event);
    }
}

define_class!(
    // SAFETY: UIKit documents subclassing UIGestureRecognizer (UIGestureRecognizerSubclass.h),
    // and every override calls the superclass implementation. `TouchObserver` doesn't
    // implement `Drop`.
    #[unsafe(super(UIGestureRecognizer, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PhotoCraftPencilTouchObserver"]
    #[ivars = Shared]
    struct TouchObserver;

    impl TouchObserver {
        #[unsafe(method(touchesBegan:withEvent:))]
        fn touches_began(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.read(touches, true);
            // SAFETY: the superclass implementation, with the arguments UIKit gave us.
            let _: () = unsafe { msg_send![super(self), touchesBegan: touches, withEvent: event] };
        }

        #[unsafe(method(touchesMoved:withEvent:))]
        fn touches_moved(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.read(touches, false);
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), touchesMoved: touches, withEvent: event] };
        }

        #[unsafe(method(touchesEnded:withEvent:))]
        fn touches_ended(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            // A lifted Pencil keeps its sample (see `uikit::on_touches`).
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), touchesEnded: touches, withEvent: event] };
        }

        #[unsafe(method(touchesCancelled:withEvent:))]
        fn touches_cancelled(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![super(self), touchesCancelled: touches, withEvent: event] };
        }
    }
);

impl TouchObserver {
    fn new(mtm: MainThreadMarker, callback: Callback) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Shared { callback });
        // SAFETY: `init` is UIGestureRecognizer's initializer without a target; this recognizer
        // only acts in its touch overrides.
        unsafe { msg_send![super(this), init] }
    }

    /// Read the changed touches' type and Pencil values and report the new sample, if any.
    fn read(&self, touches: &NSSet<UITouch>, began: bool) {
        let view = self.view();
        let raw: Vec<(isize, RawPencil)> = touches
            .iter()
            .map(|touch| {
                let kind = touch.r#type().0;
                let pencil = if kind == crate::uikit::touch_type::PENCIL {
                    RawPencil {
                        force: touch.force(),
                        max_force: touch.maximumPossibleForce(),
                        altitude: touch.altitudeAngle(),
                        azimuth: touch.azimuthAngleInView(view.as_deref()),
                        roll: available!(ios = 17.5).then(|| touch.rollAngle()),
                    }
                } else {
                    RawPencil::default()
                };
                (kind, pencil)
            })
            .collect();
        if let Some(sample) = on_touches(&raw, began) {
            self.ivars().emit(PencilEvent::Sample(sample));
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; `HoverTarget` doesn't implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PhotoCraftPencilHoverTarget"]
    #[ivars = Shared]
    struct HoverTarget;

    impl HoverTarget {
        /// The hover recognizer's action.
        #[unsafe(method(hover:))]
        fn hover(&self, recognizer: &UIHoverGestureRecognizer) {
            let state = recognizer.state();
            let point = if state == UIGestureRecognizerState::Began || state == UIGestureRecognizerState::Changed {
                let view = recognizer.view();
                let p = recognizer.locationInView(view.as_deref());
                Some([p.x as f32, p.y as f32])
            } else {
                None
            };
            self.ivars().emit(PencilEvent::Hover(point));
        }
    }
);

impl HoverTarget {
    fn new(mtm: MainThreadMarker, callback: Callback) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Shared { callback });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; `PencilDelegate` doesn't implement
    // `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PhotoCraftPencilInteractionDelegate"]
    #[ivars = Shared]
    struct PencilDelegate;

    unsafe impl NSObjectProtocol for PencilDelegate {}

    unsafe impl UIPencilInteractionDelegate for PencilDelegate {
        /// Double-tap before iPadOS 17.5 (UIKit calls only `pencilInteraction:didReceiveTap:`
        /// where both exist).
        #[allow(deprecated)]
        #[unsafe(method(pencilInteractionDidTap:))]
        fn did_tap(&self, _interaction: &UIPencilInteraction) {
            self.ivars().emit(PencilEvent::Tap(preferred_tap_action(MainThreadMarker::from(self))));
        }

        #[unsafe(method(pencilInteraction:didReceiveTap:))]
        fn did_receive_tap(&self, _interaction: &UIPencilInteraction, _tap: &UIPencilInteractionTap) {
            self.ivars().emit(PencilEvent::Tap(preferred_tap_action(MainThreadMarker::from(self))));
        }

        #[unsafe(method(pencilInteraction:didReceiveSqueeze:))]
        fn did_receive_squeeze(&self, _interaction: &UIPencilInteraction, squeeze: &UIPencilInteractionSqueeze) {
            if squeeze.phase() == UIPencilInteractionPhase::Ended {
                self.ivars().emit(PencilEvent::Squeeze(preferred_squeeze_action(MainThreadMarker::from(self))));
            }
        }
    }
);

impl PencilDelegate {
    fn new(mtm: MainThreadMarker, callback: Callback) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Shared { callback });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}
