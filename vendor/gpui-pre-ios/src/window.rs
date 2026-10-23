//! iOS Window implementation using UIWindow and UIViewController.
//!
//! iOS windows are fundamentally different from desktop windows:
//! - Always fullscreen (or split-screen on iPad)
//! - No title bar or window chrome
//! - Touch-based input
//! - Safe area insets for notch/home indicator
//!
//! The window is backed by a UIWindow containing a UIViewController
//! whose view hosts the `CAMetalLayer` of `gpui_apple::MetalRenderer`,
//! the renderer of the Mac app, as a sublayer; a `CADisplayLink` of the
//! window asks GPUI for a frame on each tick of the display.

use super::events::*;
use super::IosDisplay;

use gpui::{
    px, size, AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, GpuSpecs,
    Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler,
    PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions, Scene, Size,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowParams,
};
use gpui_apple::metal_renderer::{InstanceBufferPool, MetalRenderer};
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};

use super::cg_types::{ObjcCGPoint, ObjcCGRect};
use parking_lot::Mutex;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, UiKitDisplayHandle, UiKitWindowHandle};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    ptr::{self, NonNull},
    rc::Rc,
    sync::Arc,
};

const GPUI_WINDOW_IVAR: &str = "gpui_window_ptr";

static METAL_VIEW_CLASS_REGISTERED: std::sync::Once = std::sync::Once::new();
static FRAME_SOURCE_CLASS_REGISTERED: std::sync::Once = std::sync::Once::new();
static VC_CLASS_REGISTERED: std::sync::Once = std::sync::Once::new();
static TEXT_INPUT_VIEW_CLASS_REGISTERED: std::sync::Once = std::sync::Once::new();

/// Global storage for the current status bar style.
/// 0 = dark content, 1 = light content (the default: Bloom is dark).
/// Accessed from the main thread only.
static STATUS_BAR_STYLE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);

/// `UIInterfaceOrientationMask`: the orientations the window allows.
const ORIENTATION_PORTRAIT: usize = 1 << 1;
const ORIENTATION_LANDSCAPE: usize = (1 << 3) | (1 << 4);

/// The orientations the window allows now: portrait, or landscape while
/// a video plays (`set_landscape`).
static ORIENTATION_MASK: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(ORIENTATION_PORTRAIT);

/// Turns the window to landscape, or back to portrait: the view controller
/// allows the one set, and the window scene is asked to turn now. Main
/// thread only (UIKit).
pub fn set_landscape(on: bool) {
    let mask = if on { ORIENTATION_LANDSCAPE } else { ORIENTATION_PORTRAIT };
    if ORIENTATION_MASK.swap(mask, std::sync::atomic::Ordering::AcqRel) == mask {
        return;
    }
    let Some(window) = super::ffi::last_window() else { return };
    unsafe {
        let vc = window.view_controller;
        if !vc.is_null() {
            let _: () = msg_send![vc, setNeedsUpdateOfSupportedInterfaceOrientations];
        }
        let ui_window: *mut AnyObject = msg_send![window.view, window];
        if ui_window.is_null() {
            return;
        }
        let scene: *mut AnyObject = msg_send![ui_window, windowScene];
        if scene.is_null() {
            return;
        }
        let preferences: *mut AnyObject =
            msg_send![class!(UIWindowSceneGeometryPreferencesIOS), alloc];
        let preferences: *mut AnyObject =
            msg_send![preferences, initWithInterfaceOrientations: mask];
        let _: () = msg_send![
            scene,
            requestGeometryUpdateWithPreferences: preferences,
            errorHandler: ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![preferences, release];
    }
}

/// Register a custom UIViewController subclass that allows overriding
/// `preferredStatusBarStyle` at runtime.
fn register_view_controller_class() -> &'static AnyClass {
    VC_CLASS_REGISTERED.call_once(|| {
        let superclass = class!(UIViewController);
        let mut decl = ClassBuilder::new(c"GPUIViewController", superclass).unwrap();

        // Override preferredStatusBarStyle
        extern "C" fn preferred_status_bar_style(_this: *mut AnyObject, _sel: Sel) -> isize {
            let style = STATUS_BAR_STYLE.load(std::sync::atomic::Ordering::Relaxed);
            if style == 1 {
                1 // UIStatusBarStyleLightContent
            } else {
                3 // UIStatusBarStyleDarkContent (iOS 13+)
            }
        }

        // The orientations allowed: `ORIENTATION_MASK`.
        extern "C" fn supported_interface_orientations(_this: *mut AnyObject, _sel: Sel) -> usize {
            ORIENTATION_MASK.load(std::sync::atomic::Ordering::Acquire)
        }

        // Override viewDidLayoutSubviews — called by UIKit on rotation,
        // split-screen changes, and any other layout pass.
        extern "C" fn view_did_layout_subviews(this: *mut AnyObject, _sel: Sel) {
            // Call super
            unsafe {
                let superclass = class!(UIViewController);
                let _: () = msg_send![super(this, superclass), viewDidLayoutSubviews];
            }

            // Notify all registered GPUI windows about the layout change.
            if let Some(wrapper) = super::ffi::IOS_WINDOW_LIST.get() {
                unsafe {
                    let windows = &*wrapper.0.get();
                    for &window_ptr in windows.iter() {
                        if !window_ptr.is_null() {
                            let window = &*window_ptr;
                            window.handle_layout_change();
                        }
                    }
                }
            }
        }

        unsafe {
            decl.add_method(
                sel!(preferredStatusBarStyle),
                preferred_status_bar_style as extern "C" fn(*mut AnyObject, Sel) -> isize,
            );
            decl.add_method(
                sel!(supportedInterfaceOrientations),
                supported_interface_orientations as extern "C" fn(*mut AnyObject, Sel) -> usize,
            );
            decl.add_method(
                sel!(viewDidLayoutSubviews),
                view_did_layout_subviews as extern "C" fn(*mut AnyObject, Sel),
            );
        }

        decl.register();
    });

    class!(GPUIViewController)
}

/// Set the iOS status bar content style (light or dark text/icons).
///
/// This updates the stored style and asks the root view controller
/// to re-query `preferredStatusBarStyle`.
pub fn set_status_bar_style(style: crate::StatusBarContentStyle) {
    use crate::StatusBarContentStyle;

    let value = match style {
        StatusBarContentStyle::Light => 1,
        StatusBarContentStyle::Dark => 0,
    };
    STATUS_BAR_STYLE.store(value, std::sync::atomic::Ordering::Relaxed);

    // Ask UIKit to re-query the status bar style
    unsafe {
        if let Some(wrapper) = super::ffi::IOS_WINDOW_LIST.get() {
            let windows = &*wrapper.0.get();
            if let Some(&window_ptr) = windows.last() {
                if !window_ptr.is_null() {
                    let window = &*window_ptr;
                    let vc = window.view_controller;
                    if !vc.is_null() {
                        let _: () = msg_send![vc, setNeedsStatusBarAppearanceUpdate];
                    }
                }
            }
        }
    }
}

/// The `NSObject` that a `CADisplayLink` calls on each tick of the display:
/// `tick:` asks GPUI for a frame of the window in its ivar.
fn register_frame_source_class() -> &'static AnyClass {
    FRAME_SOURCE_CLASS_REGISTERED.call_once(|| {
        let mut decl = ClassBuilder::new(c"GPUIFrameSource", class!(NSObject)).unwrap();
        decl.add_ivar::<*mut std::ffi::c_void>(c"gpui_window_ptr");

        extern "C" fn tick(this: *mut AnyObject, _sel: Sel, _link: *mut AnyObject) {
            FRAME_TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            unsafe {
                #[allow(deprecated)]
                let window_ptr: *mut std::ffi::c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
                if let Some(window) = (window_ptr as *const IosWindow).as_ref() {
                    super::ffi::request_frame(window);
                }
            }
        }

        // A quarter of a second after the display link started with no
        // tick, a timer at 60 Hz drives the frames instead: a simulator
        // with no window on it (no DeviceHub attached) never ticks a
        // display link, and a device may hold them back in some states. A
        // link that works ticks within a frame.
        extern "C" fn check(this: *mut AnyObject, _sel: Sel, _timer: *mut AnyObject) {
            if FRAME_TICKS.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                return;
            }
            log::warn!("GPUI iOS: no tick of the display link; a timer drives the frames");
            unsafe {
                let _: *mut AnyObject = msg_send![
                    class!(NSTimer),
                    scheduledTimerWithTimeInterval: 1.0f64 / 60.0,
                    target: this,
                    selector: sel!(tick:),
                    userInfo: ptr::null::<AnyObject>(),
                    repeats: true
                ];
            }
        }

        unsafe {
            decl.add_method(
                sel!(tick:),
                tick as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            decl.add_method(
                sel!(check:),
                check as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
        }
        decl.register();
    });
    class!(GPUIFrameSource)
}

/// Ticks of the display link so far (the check of the fallback timer).
static FRAME_TICKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The video layer of the window (see `IosWindow::new`), for the player.
static VIDEO_LAYER: std::sync::atomic::AtomicPtr<AnyObject> =
    std::sync::atomic::AtomicPtr::new(ptr::null_mut());

/// The `CAMetalLayer` under GPUI's layer that a video player may draw into,
/// the size of the window. Null before the window exists.
pub fn video_layer() -> *mut c_void {
    VIDEO_LAYER.load(std::sync::atomic::Ordering::Acquire) as *mut c_void
}

/// Register the UIView subclass that takes the touches. Its own layer is a
/// plain `CALayer`; the renderer's `CAMetalLayer` sits in it as a sublayer
/// with the same frame.
fn register_metal_view_class() -> &'static AnyClass {
    METAL_VIEW_CLASS_REGISTERED.call_once(|| {
        let superclass = class!(UIView);
        let mut decl = ClassBuilder::new(c"GPUIMetalView", superclass).unwrap();

        // Add ivar to store window pointer for touch handling
        decl.add_ivar::<*mut std::ffi::c_void>(c"gpui_window_ptr");

        extern "C" fn did_move_to_window(this: *mut AnyObject, _sel: Sel) {
            unsafe {
                let _: () = msg_send![super(this, class!(UIView)), didMoveToWindow];
                #[allow(deprecated)]
                let window_ptr: *mut std::ffi::c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
                if let Some(window) = (window_ptr as *const IosWindow).as_ref() {
                    window.on_moved_to_window();
                }
            }
        }

        // Touch handling methods
        extern "C" fn touches_began(
            this: *mut AnyObject,
            _sel: Sel,
            touches: *mut AnyObject,
            event: *mut AnyObject,
        ) {
            handle_touches(this, touches, event);
        }

        extern "C" fn touches_moved(
            this: *mut AnyObject,
            _sel: Sel,
            touches: *mut AnyObject,
            event: *mut AnyObject,
        ) {
            handle_touches(this, touches, event);
        }

        extern "C" fn touches_ended(
            this: *mut AnyObject,
            _sel: Sel,
            touches: *mut AnyObject,
            event: *mut AnyObject,
        ) {
            handle_touches(this, touches, event);
        }

        extern "C" fn touches_cancelled(
            this: *mut AnyObject,
            _sel: Sel,
            touches: *mut AnyObject,
            event: *mut AnyObject,
        ) {
            handle_touches(this, touches, event);
        }

        // VoiceOver: the view is a container of the elements of the tree.
        extern "C" fn is_a11y_element(_this: *mut AnyObject, _sel: Sel) -> Bool {
            Bool::NO
        }
        extern "C" fn a11y_elements(this: *mut AnyObject, _sel: Sel) -> *mut AnyObject {
            unsafe {
                #[allow(deprecated)]
                let window_ptr: *mut std::ffi::c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
                match (window_ptr as *const IosWindow).as_ref() {
                    Some(window) => window.a11y_elements_array(),
                    None => ptr::null_mut(),
                }
            }
        }

        unsafe {
            decl.add_method(
                sel!(isAccessibilityElement),
                is_a11y_element as extern "C" fn(*mut AnyObject, Sel) -> Bool,
            );
            decl.add_method(
                sel!(accessibilityElements),
                a11y_elements as extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject,
            );
            decl.add_method(
                sel!(didMoveToWindow),
                did_move_to_window as extern "C" fn(*mut AnyObject, Sel),
            );
            // Add touch handling instance methods
            decl.add_method(
                sel!(touchesBegan:withEvent:),
                touches_began as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
            decl.add_method(
                sel!(touchesMoved:withEvent:),
                touches_moved as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
            decl.add_method(
                sel!(touchesEnded:withEvent:),
                touches_ended as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
            decl.add_method(
                sel!(touchesCancelled:withEvent:),
                touches_cancelled
                    as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
        }

        decl.register();
    });

    class!(GPUIMetalView)
}

/// UITextView supplies UIKit's complete UITextInput implementation, including
/// marked text, tokenizer and UTF-16 positions required by Chinese/Japanese IMEs.
/// Its small private document holds only the current composition; committed text
/// is forwarded to GPUI and then removed from the native scratch buffer.
fn register_text_input_view_class() -> &'static AnyClass {
    TEXT_INPUT_VIEW_CLASS_REGISTERED.call_once(|| {
        let mut decl = ClassBuilder::new(c"GPUITextInputView", class!(UITextView)).unwrap();
        decl.add_ivar::<*mut std::ffi::c_void>(c"gpui_window_ptr");
        decl.add_ivar::<usize>(c"gpui_edit_depth");
        decl.add_ivar::<Bool>(c"gpui_marked_text");

        #[allow(deprecated)]
        unsafe fn begin_edit(this: *mut AnyObject) {
            *(*this).get_mut_ivar::<usize>("gpui_edit_depth") += 1;
        }

        #[allow(deprecated)]
        unsafe fn end_edit(this: *mut AnyObject) {
            let depth = (*this).get_mut_ivar::<usize>("gpui_edit_depth");
            *depth -= 1;
            if *depth != 0 {
                return;
            }

            // GPUI updates may synchronously cause UIKit callbacks. Keep those
            // nested native edits from borrowing the input handler a second time.
            struct ForwardingGuard(*mut AnyObject);
            impl Drop for ForwardingGuard {
                fn drop(&mut self) {
                    #[allow(deprecated)]
                    unsafe {
                        *(*self.0).get_mut_ivar::<usize>("gpui_edit_depth") -= 1;
                    }
                }
            }
            begin_edit(this);
            let _forwarding = ForwardingGuard(this);

            let window_ptr: *mut c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
            if window_ptr.is_null() {
                return;
            }
            let marked: *mut AnyObject = msg_send![this, markedTextRange];
            let text: *mut AnyObject = msg_send![this, text];
            let utf8: *const i8 = msg_send![text, UTF8String];
            let text_string = if utf8.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(utf8)
                    .to_string_lossy()
                    .into_owned()
            };
            let was_marked = (*this).get_ivar::<Bool>("gpui_marked_text").as_bool();
            let is_marked = !marked.is_null();
            *(*this).get_mut_ivar::<Bool>("gpui_marked_text") = Bool::new(is_marked);
            let window = &*(window_ptr as *const IosWindow);
            // Only the view of the focused field writes to it: UIKit calls
            // `unmarkText` on a view as it resigns, and that view's empty
            // text wiped the field that had just taken the focus
            // (the username, on the way back from the password; 2026-10-22).
            if this != window.active_input_view() {
                return;
            }

            // The view mirrors the field (`sync_input_view`): its document is
            // the field's text, so UIKit's own edits (a tapped prediction,
            // an autocorrection, a replaced word) land on real text. After
            // each edit the field takes the view's whole text and selection.
            let selected: super::text_input::ObjcNSRange = msg_send![this, selectedRange];
            let selected = selected.location..selected.location + selected.length;
            if is_marked {
                // A composition in progress (an IME): the field shows the
                // whole text as marked until it is committed.
                let Some((_, len)) = window.gpui_document() else { return };
                if let Some(handler) = window.input_handler.borrow_mut().as_mut() {
                    handler.replace_and_mark_text_in_range(Some(0..len), &text_string, Some(selected));
                }
            } else {
                window.take_input_view(this, &text_string, selected, was_marked);
            }
        }

        unsafe extern "C" fn has_text(_this: *mut AnyObject, _sel: Sel) -> Bool {
            // The GPUI document can contain text even when this scratchpad is empty.
            Bool::YES
        }

        unsafe extern "C" fn insert_text(this: *mut AnyObject, _sel: Sel, text: *mut AnyObject) {
            begin_edit(this);
            let _: () = msg_send![super(this, class!(UITextView)), insertText: text];
            end_edit(this);
        }

        unsafe extern "C" fn set_marked_text(
            this: *mut AnyObject,
            _sel: Sel,
            text: *mut AnyObject,
            selected: super::text_input::ObjcNSRange,
        ) {
            begin_edit(this);
            let _: () = msg_send![super(this, class!(UITextView)),
                setMarkedText: text, selectedRange: selected];
            end_edit(this);
        }

        unsafe extern "C" fn unmark_text(this: *mut AnyObject, _sel: Sel) {
            begin_edit(this);
            let _: () = msg_send![super(this, class!(UITextView)), unmarkText];
            end_edit(this);
        }

        // UIKit's attributed composition entry point does not call the plain
        // setMarkedText override. Modern IMEs can update exclusively through it.
        unsafe extern "C" fn set_attributed_marked_text(
            this: *mut AnyObject,
            _sel: Sel,
            text: *mut AnyObject,
            selected: super::text_input::ObjcNSRange,
        ) {
            begin_edit(this);
            let _: () = msg_send![super(this, class!(UITextView)),
                setAttributedMarkedText: text, selectedRange: selected];
            end_edit(this);
        }

        /// `apply_text_traits`, deferred to the run loop.
        #[allow(deprecated)]
        unsafe extern "C" fn apply_traits(this: *mut AnyObject, _sel: Sel) {
            let window_ptr: *mut c_void = unsafe { *(*this).get_ivar(GPUI_WINDOW_IVAR) };
            if !window_ptr.is_null() {
                unsafe { (&*(window_ptr as *const IosWindow)).apply_text_traits_now() };
            }
        }

        #[allow(deprecated)]
        unsafe extern "C" fn reset_composition(this: *mut AnyObject, _sel: Sel) {
            // Focus may already belong to another GPUI input by the time the
            // keyboard is hidden. Never forward this cleanup to that handler.
            begin_edit(this);
            let _: () = msg_send![super(this, class!(UITextView)), unmarkText];
            let empty: *mut AnyObject = msg_send![class!(NSString), new];
            let _: () = msg_send![this, setText: empty];
            let _: () = msg_send![empty, release];
            *(*this).get_mut_ivar::<Bool>("gpui_marked_text") = Bool::NO;
            *(*this).get_mut_ivar::<usize>("gpui_edit_depth") -= 1;
        }

        unsafe extern "C" fn replace_range(
            this: *mut AnyObject,
            _sel: Sel,
            range: *mut AnyObject,
            text: *mut AnyObject,
        ) {
            begin_edit(this);
            let _: () =
                msg_send![super(this, class!(UITextView)), replaceRange: range, withText: text];
            end_edit(this);
        }

        #[allow(deprecated)]
        unsafe extern "C" fn delete_backward(this: *mut AnyObject, _sel: Sel) {
            let text: *mut AnyObject = msg_send![this, text];
            let length: usize = msg_send![text, length];
            let depth = *(*this).get_ivar::<usize>("gpui_edit_depth");
            if length == 0 && depth == 0 && !(*this).get_ivar::<Bool>("gpui_marked_text").as_bool()
            {
                let window_ptr: *mut c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
                if !window_ptr.is_null() {
                    (&*(window_ptr as *const IosWindow)).handle_delete_backward();
                }
                return;
            }
            begin_edit(this);
            let _: () = msg_send![super(this, class!(UITextView)), deleteBackward];
            end_edit(this);
        }

        unsafe {
            decl.add_method(
                sel!(hasText),
                has_text as unsafe extern "C" fn(*mut AnyObject, Sel) -> Bool,
            );
            decl.add_method(
                sel!(insertText:),
                insert_text as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            decl.add_method(
                sel!(setMarkedText:selectedRange:),
                set_marked_text
                    as unsafe extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *mut AnyObject,
                        super::text_input::ObjcNSRange,
                    ),
            );
            decl.add_method(
                sel!(unmarkText),
                unmark_text as unsafe extern "C" fn(*mut AnyObject, Sel),
            );
            decl.add_method(
                sel!(setAttributedMarkedText:selectedRange:),
                set_attributed_marked_text
                    as unsafe extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *mut AnyObject,
                        super::text_input::ObjcNSRange,
                    ),
            );
            decl.add_method(
                sel!(gpuiResetComposition),
                reset_composition as unsafe extern "C" fn(*mut AnyObject, Sel),
            );
            decl.add_method(
                sel!(gpuiApplyTraits),
                apply_traits as unsafe extern "C" fn(*mut AnyObject, Sel),
            );
            decl.add_method(
                sel!(replaceRange:withText:),
                replace_range
                    as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject),
            );
            decl.add_method(
                sel!(deleteBackward),
                delete_backward as unsafe extern "C" fn(*mut AnyObject, Sel),
            );
        }
        decl.register();
    });
    class!(GPUITextInputView)
}

/// Handle touch events from the GPUIMetalView
fn handle_touches(view: *mut AnyObject, touches: *mut AnyObject, event: *mut AnyObject) {
    unsafe {
        // Get the window pointer from the view's ivar
        #[allow(deprecated)]
        let window_ptr: *mut std::ffi::c_void = *(*view).get_ivar(GPUI_WINDOW_IVAR);
        if window_ptr.is_null() {
            log::warn!("GPUI iOS: Touch event but no window pointer set");
            return;
        }

        let window = &*(window_ptr as *const IosWindow);

        // Get all touches from the set
        let all_touches: *mut AnyObject = msg_send![touches, allObjects];
        let count: usize = msg_send![all_touches, count];

        for i in 0..count {
            let touch: *mut AnyObject = msg_send![all_touches, objectAtIndex: i];
            window.handle_touch(touch, event);
        }
    }
}

/// iOS Window backed by UIWindow + UIViewController.

#[allow(clippy::type_complexity)]
pub(crate) struct IosWindow {
    /// The UIWindow object
    window: *mut AnyObject,
    /// The UIViewController
    view_controller: *mut AnyObject,
    /// The Metal-backed UIView
    view: *mut AnyObject,
    /// The hidden text input view for keyboard input
    text_input_view: *mut AnyObject,
    /// A second one for a username or a password (`TextContent`): iOS
    /// keeps its AutoFill session (the Passwords bar) on a view once it
    /// held a credential, whatever its traits say after, so the other
    /// fields keep a view of their own (2026-10-22). `input_view_for`
    /// picks one by the traits.
    credential_input_view: *mut AnyObject,
    /// Current bounds in pixels
    bounds: Cell<Bounds<Pixels>>,
    /// Scale factor
    scale_factor: Cell<f32>,
    /// Input handler for text input
    input_handler: RefCell<Option<PlatformInputHandler>>,
    /// Callback for frame requests
    /// Note: pub(super) to allow ffi.rs to access this for the display link callback
    pub(super) request_frame_callback: RefCell<Option<Box<dyn FnMut(RequestFrameOptions)>>>,
    /// Callback for input events
    input_callback: RefCell<Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>>,
    /// Callback for active status changes
    active_status_callback: RefCell<Option<Box<dyn FnMut(bool)>>>,
    /// Callback for hover status changes (not really applicable on iOS)
    hover_status_callback: RefCell<Option<Box<dyn FnMut(bool)>>>,
    /// Callback for resize events
    resize_callback: RefCell<Option<Box<dyn FnMut(Size<Pixels>, f32)>>>,
    /// Callback for move events (not applicable on iOS)
    moved_callback: RefCell<Option<Box<dyn FnMut()>>>,
    /// Callback for should close
    should_close_callback: RefCell<Option<Box<dyn FnMut() -> bool>>>,
    /// Callback for hit test
    hit_test_callback: RefCell<Option<Box<dyn FnMut() -> Option<WindowControlArea>>>>,
    /// Callback for close
    close_callback: RefCell<Option<Box<dyn FnOnce()>>>,
    /// Callback for appearance changes
    appearance_changed_callback: RefCell<Option<Box<dyn FnMut()>>>,
    /// Current mouse position (from touch)
    mouse_position: Cell<Point<Pixels>>,
    /// Current modifiers
    modifiers: Cell<Modifiers>,
    /// Stable, non-reused GPUI IDs for UIKit's live touch objects.
    active_touches: RefCell<HashMap<usize, gpui::TouchId>>,
    next_touch_id: Cell<u64>,
    /// The Metal renderer of `gpui_apple`, the one of the Mac app. In a
    /// `Mutex` so that `draw()` (called from the `request_frame` callback)
    /// can take it mutably behind the `&self` of the window.
    renderer: Mutex<MetalRenderer>,
    /// The `CADisplayLink` that drives the frames, once the window has a
    /// stable address (`register_with_ffi`).
    display_link: Cell<*mut AnyObject>,
    /// The software keyboard is up: GPUI has an input handler (a text
    /// field has the focus). Checked at each draw.
    keyboard_shown: Cell<bool>,
    /// The edit menu of a text field: the `UIEditMenuInteraction` on the
    /// view and its delegate, made at the first menu; what the menu offers
    /// and does is in `edit_menu`.
    edit_menu_interaction: Cell<*mut AnyObject>,
    edit_menu_delegate: Cell<*mut AnyObject>,
    edit_menu: RefCell<EditMenuState>,
    /// The accessibility tree GPUI sent last (`a11y_tree_update`), by node,
    /// with its root: the roles, labels and bounds of the elements, for
    /// `a11y_json`. Kept only with `BLOOM_A11Y_TREE`.
    a11y_nodes: RefCell<HashMap<gpui::accesskit::NodeId, gpui::accesskit::Node>>,
    a11y_root: Cell<Option<gpui::accesskit::NodeId>>,
    /// GPUI's callbacks: the activation (asked when VoiceOver first asks
    /// for the elements) and the action (a VoiceOver activation of an
    /// element goes back as a click of its node).
    a11y_callbacks: RefCell<Option<gpui::A11yCallbacks>>,
    /// The tree is built: GPUI sends updates after each frame.
    a11y_active: Cell<bool>,
    /// The `GPUIA11yElement` objects VoiceOver reads (retained), and the
    /// NSArray of them the view answers `accessibilityElements` with.
    a11y_elements: RefCell<Vec<*mut AnyObject>>,
    a11y_array: Cell<*mut AnyObject>,
    /// A hash of the ids, roles and labels of the elements, so the list is
    /// built again only when it changed (the frames are read live).
    a11y_signature: Cell<u64>,
}

// Required for raw_window_handle
unsafe impl Send for IosWindow {}
unsafe impl Sync for IosWindow {}

impl IosWindow {
    pub fn new(handle: AnyWindowHandle, _params: WindowParams) -> anyhow::Result<Self> {
        // Create the window on the main screen
        let screen = IosDisplay::main();
        let screen_bounds = screen.bounds();
        let scale_factor = screen.scale();

        unsafe {
            // Create UIWindow
            let screen_obj: *mut AnyObject = msg_send![class!(UIScreen), mainScreen];
            let screen_bounds_cg: ObjcCGRect = msg_send![screen_obj, bounds];
            let window: *mut AnyObject = if super::ffi::is_embedded() {
                ptr::null_mut()
            } else {
                let window: *mut AnyObject = msg_send![class!(UIWindow), alloc];
                msg_send![window, initWithFrame: screen_bounds_cg]
            };

            // Create our custom UIViewController subclass that supports
            // dynamic `preferredStatusBarStyle` overrides.
            let vc_class = register_view_controller_class();
            let view_controller: *mut AnyObject = msg_send![vc_class, alloc];
            let view_controller: *mut AnyObject = msg_send![view_controller, init];

            // Create our custom Metal view using the registered class
            let metal_view_class = register_metal_view_class();
            let view: *mut AnyObject = msg_send![metal_view_class, alloc];
            let view: *mut AnyObject = msg_send![view, initWithFrame: screen_bounds_cg];

            let scale: core_graphics::base::CGFloat = msg_send![screen_obj, scale];
            // Black behind the Metal layer, for the moment before the first
            // frame and the edges of a resize.
            let black: *mut AnyObject = msg_send![class!(UIColor), blackColor];
            let _: () = msg_send![view, setBackgroundColor: black];
            let _: () = msg_send![view, setOpaque: true];

            // Auto-resize the Metal view when the parent view changes size
            // (e.g. rotation). UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight
            let _: () = msg_send![view, setAutoresizingMask: 18_usize]; // 0x02 | 0x10

            // Enable user interaction on the Metal view for touch handling
            let _: () = msg_send![view, setUserInteractionEnabled: true];
            let _: () = msg_send![view, setMultipleTouchEnabled: true];

            // Set the view as the view controller's view
            let _: () = msg_send![view_controller, setView: view];

            // Set the root view controller
            if !window.is_null() {
                let _: () = msg_send![window, setRootViewController: view_controller];
            }

            // Make the window visible
            if !window.is_null() {
                let _: () = msg_send![window, makeKeyAndVisible];
            }

            // UIKit owns the IME composition in a native UITextView; GPUI owns
            // the visible document.
            let text_input_class = register_text_input_view_class();
            let text_input_view: *mut AnyObject = msg_send![text_input_class, alloc];
            let text_input_frame = ObjcCGRect::new(0.0, 0.0, 1.0, 1.0);
            let text_input_view: *mut AnyObject =
                msg_send![text_input_view, initWithFrame: text_input_frame];
            let _: () = msg_send![text_input_view, setAlpha: 0.01_f64];
            let _: () = msg_send![text_input_view, setUserInteractionEnabled: true];
            let _: () = msg_send![text_input_view, setScrollEnabled: false];
            let _: () = msg_send![view, addSubview: text_input_view];
            let credential_input_view: *mut AnyObject = msg_send![text_input_class, alloc];
            let credential_input_view: *mut AnyObject =
                msg_send![credential_input_view, initWithFrame: text_input_frame];
            let _: () = msg_send![credential_input_view, setAlpha: 0.01_f64];
            let _: () = msg_send![credential_input_view, setUserInteractionEnabled: true];
            let _: () = msg_send![credential_input_view, setScrollEnabled: false];
            let _: () = msg_send![view, addSubview: credential_input_view];

            // --- The Metal renderer and its layer ---------------------------
            let pixel_w = (screen_bounds_cg.width * scale) as i32;
            let pixel_h = (screen_bounds_cg.height * scale) as i32;
            // GPUI's layer is not opaque: a page may leave a hole for the
            // video layer under it.
            let mut renderer =
                MetalRenderer::new(Arc::new(Mutex::new(InstanceBufferPool::default())), true);
            let metal_layer = renderer.layer_ptr() as *mut AnyObject;
            let host_layer: *mut AnyObject = msg_send![view, layer];
            let _: () = msg_send![metal_layer, setContentsScale: scale];
            let _: () = msg_send![
                metal_layer,
                setFrame: ObjcCGRect::new(0.0, 0.0, screen_bounds_cg.width, screen_bounds_cg.height)
            ];
            // The video layer: a `CAMetalLayer` of the whole view under
            // GPUI's, which the player (mpv over MoltenVK) draws into. The
            // page shows it by painting nothing over the video.
            let video_layer: *mut AnyObject = msg_send![class!(CAMetalLayer), new];
            let _: () = msg_send![video_layer, setContentsScale: scale];
            let _: () = msg_send![
                video_layer,
                setFrame: ObjcCGRect::new(0.0, 0.0, screen_bounds_cg.width, screen_bounds_cg.height)
            ];
            let _: () = msg_send![
                video_layer,
                setDrawableSize: super::cg_types::ObjcCGSize { width: pixel_w as f64, height: pixel_h as f64 }
            ];
            let _: () = msg_send![video_layer, setOpaque: true];
            let _: () = msg_send![host_layer, addSublayer: video_layer];
            let _: () = msg_send![host_layer, addSublayer: metal_layer];
            VIDEO_LAYER.store(video_layer, std::sync::atomic::Ordering::Release);
            renderer.update_drawable_size(size(DevicePixels(pixel_w), DevicePixels(pixel_h)));
            log::info!("GPUI iOS: Metal renderer created ({pixel_w}x{pixel_h} @{scale}x)");

            let _handle = handle; // consumed but not stored
            let ios_window = Self {
                window,
                view_controller,
                view,
                text_input_view,
                credential_input_view,
                bounds: Cell::new(screen_bounds),
                scale_factor: Cell::new(scale_factor),
                input_handler: RefCell::new(None),
                request_frame_callback: RefCell::new(None),
                input_callback: RefCell::new(None),
                active_status_callback: RefCell::new(None),
                hover_status_callback: RefCell::new(None),
                resize_callback: RefCell::new(None),
                moved_callback: RefCell::new(None),
                should_close_callback: RefCell::new(None),
                hit_test_callback: RefCell::new(None),
                close_callback: RefCell::new(None),
                appearance_changed_callback: RefCell::new(None),
                mouse_position: Cell::new(Point::default()),
                modifiers: Cell::new(Modifiers::default()),
                active_touches: RefCell::new(HashMap::new()),
                next_touch_id: Cell::new(0),
                renderer: Mutex::new(renderer),
                display_link: Cell::new(ptr::null_mut()),
                keyboard_shown: Cell::new(false),
                edit_menu_interaction: Cell::new(ptr::null_mut()),
                edit_menu_delegate: Cell::new(ptr::null_mut()),
                edit_menu: RefCell::new(EditMenuState::default()),
                a11y_nodes: RefCell::new(HashMap::new()),
                a11y_root: Cell::new(None),
                a11y_callbacks: RefCell::new(None),
                a11y_active: Cell::new(false),
                a11y_elements: RefCell::new(Vec::new()),
                a11y_array: Cell::new(ptr::null_mut()),
                a11y_signature: Cell::new(0),
            };

            Ok(ios_window)
        }
    }

    /// Get the raw pointer to the UIViewController.
    pub fn view_controller_ptr(&self) -> *mut AnyObject {
        self.view_controller
    }

    /// Register this window with the FFI layer after it's been stored.
    /// This must be called after the window is placed at a stable address
    /// (e.g., in a Box or Arc).
    pub(crate) fn register_with_ffi(&self) {
        super::ffi::register_window(self as *const Self);

        // Set the window pointer on the view so touch events can find us,
        // and on the text input view so keyboard input can find us.
        unsafe {
            let window_ptr = self as *const Self as *mut std::ffi::c_void;
            #[allow(deprecated)]
            {
                *(*self.view).get_mut_ivar::<*mut c_void>(GPUI_WINDOW_IVAR) = window_ptr;
            }
            #[allow(deprecated)]
            {
                *(*self.text_input_view).get_mut_ivar::<*mut c_void>(GPUI_WINDOW_IVAR) = window_ptr;
                *(*self.credential_input_view).get_mut_ivar::<*mut c_void>(GPUI_WINDOW_IVAR) =
                    window_ptr;
            }
            log::info!(
                "GPUI iOS: Set window pointer {:p} on view {:p} and text input {:p}",
                window_ptr,
                self.view,
                self.text_input_view
            );
        }

        // Listen for keyboard show/hide so we can expose the keyboard height.
        self.register_keyboard_observers();
    }

    /// The view entered a window, or left it: the `CADisplayLink` that
    /// asks GPUI for a frame on each tick of the display starts or stops.
    /// Since iOS 27 the window scene vends the link (the class method's
    /// link did not fire under a scene, and `UIScreen`'s is deprecated).
    fn on_moved_to_window(&self) {
        unsafe {
            let ui_window: *mut AnyObject = msg_send![self.view, window];
            let link = self.display_link.get();
            if ui_window.is_null() {
                if !link.is_null() {
                    let _: () = msg_send![link, invalidate];
                    self.display_link.set(ptr::null_mut());
                }
                return;
            }
            if !link.is_null() {
                return;
            }
            let target: *mut AnyObject = msg_send![register_frame_source_class(), new];
            #[allow(deprecated)]
            {
                *(*target).get_mut_ivar::<*mut c_void>(GPUI_WINDOW_IVAR) =
                    self as *const Self as *mut c_void;
            }
            let scene: *mut AnyObject = msg_send![ui_window, windowScene];
            let from_scene = !scene.is_null()
                && msg_send![scene, respondsToSelector: sel!(displayLinkWithTarget:selector:)];
            let link: *mut AnyObject = if from_scene {
                // Sent without objc2's check of the method table: the
                // selector is new in iOS 27 and not in its tables.
                let send: unsafe extern "C-unwind" fn(
                    *mut AnyObject,
                    Sel,
                    *mut AnyObject,
                    Sel,
                ) -> *mut AnyObject = std::mem::transmute(
                    objc2::ffi::objc_msgSend as unsafe extern "C-unwind" fn(),
                );
                send(scene, sel!(displayLinkWithTarget:selector:), target, sel!(tick:))
            } else {
                msg_send![class!(CADisplayLink), displayLinkWithTarget: target, selector: sel!(tick:)]
            };
            if link.is_null() {
                log::error!("GPUI iOS: no display link for the window");
                return;
            }
            let run_loop: *mut AnyObject = msg_send![class!(NSRunLoop), mainRunLoop];
            let mode = crate::util::nsstring("kCFRunLoopCommonModes");
            let _: () = msg_send![link, addToRunLoop: run_loop, forMode: mode];
            log::info!("GPUI iOS: display link {link:p} (from the scene: {from_scene})");
            self.display_link.set(link);
            let _: *mut AnyObject = msg_send![
                class!(NSTimer),
                scheduledTimerWithTimeInterval: 0.25f64,
                target: target,
                selector: sel!(check:),
                userInfo: ptr::null::<AnyObject>(),
                repeats: false
            ];
        }
    }

    /// Pauses the frames while the app is in the background: no drawable
    /// comes there, and a tick would wait for one.
    pub(crate) fn set_frames_paused(&self, paused: bool) {
        let link = self.display_link.get();
        if !link.is_null() {
            unsafe {
                let _: () = msg_send![link, setPaused: paused];
            }
        }
    }

    /// Register for keyboard show/hide notifications so we can track the
    /// keyboard height and allow the UI to shift content above the keyboard.
    pub(crate) fn register_keyboard_observers(&self) {
        unsafe {
            let notification_center: *mut AnyObject =
                msg_send![class!(NSNotificationCenter), defaultCenter];

            let show_name = crate::util::nsstring("UIKeyboardWillShowNotification");
            let hide_name = crate::util::nsstring("UIKeyboardWillHideNotification");

            // Block that fires when the keyboard appears — extracts the
            // end-frame height and stores it in the global atomic.
            let show_block = block2::RcBlock::new(move |notification: *mut AnyObject| {
                if notification.is_null() {
                    return;
                }
                let user_info: *mut AnyObject = msg_send![notification, userInfo];
                if user_info.is_null() {
                    return;
                }
                let frame_key = crate::util::nsstring("UIKeyboardFrameEndUserInfoKey");
                let frame_value: *mut AnyObject = msg_send![user_info, objectForKey: frame_key];
                // frame_key is autoreleased by util::nsstring — no manual release needed
                let _ = frame_key;
                if frame_value.is_null() {
                    return;
                }
                let frame: ObjcCGRect = msg_send![frame_value, CGRectValue];
                let height = frame.height as f32;
                let duration = keyboard_slide_seconds(user_info);
                log::info!("GPUI iOS: Keyboard will show, height={} over {}s", height, duration);
                crate::set_keyboard_height(height, duration);
            });

            let hide_block = block2::RcBlock::new(move |notification: *mut AnyObject| {
                let user_info: *mut AnyObject =
                    if notification.is_null() { std::ptr::null_mut() } else { msg_send![notification, userInfo] };
                let duration = keyboard_slide_seconds(user_info);
                log::info!("GPUI iOS: Keyboard will hide over {}s", duration);
                crate::set_keyboard_height(0.0, duration);
            });

            let _: *mut AnyObject = msg_send![notification_center,
                addObserverForName: show_name,
                object: std::ptr::null::<AnyObject>(),
                queue: std::ptr::null::<AnyObject>(),
                usingBlock: &*show_block
            ];
            let _: *mut AnyObject = msg_send![notification_center,
                addObserverForName: hide_name,
                object: std::ptr::null::<AnyObject>(),
                queue: std::ptr::null::<AnyObject>(),
                usingBlock: &*hide_block
            ];
            // show_name and hide_name are autoreleased by util::nsstring

            // Leak the blocks so they live for the app lifetime.
            std::mem::forget(show_block);
            std::mem::forget(hide_block);
        }
    }

    /// Forward raw contacts to GPUI's gesture recognizer. It owns tap synthesis,
    /// long press, claimed control drags, pan scrolling, and scroll momentum.
    pub fn handle_touch(&self, touch: *mut AnyObject, _event: *mut AnyObject) {
        let position = touch_location_in_view(touch, self.view);
        let phase = match touch_phase(touch) {
            UITouchPhase::Began => gpui::TouchPhase::Started,
            UITouchPhase::Moved => gpui::TouchPhase::Moved,
            UITouchPhase::Ended => gpui::TouchPhase::Ended,
            UITouchPhase::Cancelled => gpui::TouchPhase::Cancelled,
            UITouchPhase::Stationary => return,
        };
        let key = touch as usize;
        let id = if phase == gpui::TouchPhase::Started {
            // Finish the old input's composition before this contact can move
            // GPUI focus (including taps between two text fields).
            unsafe {
                let _: () = msg_send![self.active_input_view(), unmarkText];
            }
            // UIKit may reuse UITouch addresses, so assign a fresh ID per contact.
            let id = gpui::TouchId(self.next_touch_id.get());
            self.next_touch_id
                .set(id.0.checked_add(1).expect("touch ID exhausted"));
            self.active_touches.borrow_mut().insert(key, id);
            id
        } else {
            let Some(id) = self.active_touches.borrow().get(&key).copied() else {
                return;
            };
            id
        };

        self.mouse_position.set(position);
        if let Some(callback) = self.input_callback.borrow_mut().as_mut() {
            callback(PlatformInput::Touch(gpui::TouchEvent {
                id,
                phase,
                position,
                predicted_position: None,
                force: None,
            }));
        }
        if matches!(phase, gpui::TouchPhase::Ended | gpui::TouchPhase::Cancelled) {
            self.active_touches.borrow_mut().remove(&key);
        }
    }

    /// Query the safe area insets from the UIView.
    ///
    /// Returns `(top, bottom, left, right)` in logical points.
    /// These represent the areas occupied by system UI (status bar,
    /// home indicator, camera notch) that content should avoid.
    pub fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        if self.view.is_null() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        unsafe {
            // UIEdgeInsets { top, left, bottom, right } — all CGFloat
            #[repr(C)]
            #[derive(Debug, Clone, Copy)]
            struct UIEdgeInsets {
                top: f64,
                left: f64,
                bottom: f64,
                right: f64,
            }

            unsafe impl Encode for UIEdgeInsets {
                const ENCODING: Encoding = Encoding::Struct(
                    "UIEdgeInsets",
                    &[
                        Encoding::Double,
                        Encoding::Double,
                        Encoding::Double,
                        Encoding::Double,
                    ],
                );
            }

            unsafe impl RefEncode for UIEdgeInsets {
                const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
            }

            let insets: UIEdgeInsets = msg_send![self.view, safeAreaInsets];
            (
                insets.top as f32,
                insets.bottom as f32,
                insets.left as f32,
                insets.right as f32,
            )
        }
    }

    /// Show the software keyboard with the specified keyboard type.
    ///
    /// The actual `becomeFirstResponder` call is deferred to the next run-loop
    /// iteration via `performSelector:withObject:afterDelay:` to avoid re-entering
    /// GPUI's event dispatch while an entity lease is active (UIKit's keyboard
    /// presentation can synchronously trigger layout callbacks).
    pub fn show_keyboard_with_type(&self, keyboard_type: crate::KeyboardType) {
        log::info!("GPUI iOS: Showing keyboard (type={:?})", keyboard_type);
        unsafe {
            if self.text_input_view.is_null() {
                log::error!("GPUI iOS: text_input_view is NULL!");
                return;
            }
            // The traits of the focused field (`set_text_traits`); the
            // keyboard asked for here wins over their default one.
            let mut traits = crate::text_traits();
            if traits.keyboard == crate::KeyboardType::Default {
                traits.keyboard = keyboard_type;
            }
            log::info!("GPUI iOS: keyboard traits {:?}", traits);
            let input = self.input_view_for(&traits);
            super::text_input::configure_keyboard_traits(input, &traits);
            self.rest_other_input_view(input);
            self.attach_input_view(input);
            log::info!("GPUI iOS: scheduling becomeFirstResponder");

            // Defer becomeFirstResponder to the next run-loop iteration.
            let _: () = msg_send![input,
                performSelector: sel!(becomeFirstResponder),
                withObject: ptr::null::<AnyObject>(),
                afterDelay: 0.0_f64
            ];
            log::info!("GPUI iOS: show_keyboard_with_type done");
        }
    }

    /// The text of the focused field and its length in UTF-16 units, from
    /// GPUI's input handler; None without a handler.
    pub(crate) fn gpui_document(&self) -> Option<(String, usize)> {
        let mut handler = self.input_handler.borrow_mut();
        let handler = handler.as_mut()?;
        let mut adjusted = None;
        let text = handler.text_for_range(0..usize::MAX, &mut adjusted)?;
        let len = adjusted.map(|range| range.end).unwrap_or_else(|| text.encode_utf16().count());
        Some((text, len))
    }

    /// The composition view takes the field's text and selection, so UIKit
    /// edits a real document (the predictive bar replaces the word under
    /// the caret, autocorrection the last one). Called before each frame
    /// while the keyboard is up, outside GPUI's draw; it does nothing
    /// during a native edit or a composition, or when nothing differs.
    pub(crate) fn sync_input_view(&self) {
        if !self.keyboard_shown.get() {
            return;
        }
        let input = self.active_input_view();
        if input.is_null() {
            return;
        }
        unsafe {
            #[allow(deprecated)]
            let depth = *(*input).get_ivar::<usize>("gpui_edit_depth");
            #[allow(deprecated)]
            let marked_flag = (*input).get_ivar::<Bool>("gpui_marked_text").as_bool();
            let marked: *mut AnyObject = msg_send![input, markedTextRange];
            if depth != 0 || marked_flag || !marked.is_null() {
                return;
            }
            let Some((text, len)) = self.gpui_document() else { return };
            let selection = {
                let mut handler = self.input_handler.borrow_mut();
                handler
                    .as_mut()
                    .and_then(|handler| handler.selected_text_range(false))
                    .map(|selection| selection.range)
                    .unwrap_or(len..len)
            };
            let current: *mut AnyObject = msg_send![input, text];
            let current = nsstring_to_string(current);
            let current_selection: super::text_input::ObjcNSRange = msg_send![input, selectedRange];
            #[allow(deprecated)]
            if current != text {
                *(*input).get_mut_ivar::<usize>("gpui_edit_depth") += 1;
                let _: () = msg_send![input, setText: crate::util::nsstring(&text)];
                *(*input).get_mut_ivar::<usize>("gpui_edit_depth") -= 1;
            }
            let wanted = super::text_input::ObjcNSRange {
                location: selection.start.min(len),
                length: selection.end.min(len).saturating_sub(selection.start.min(len)),
            };
            #[allow(deprecated)]
            if current != text
                || current_selection.location != wanted.location
                || current_selection.length != wanted.length
            {
                *(*input).get_mut_ivar::<usize>("gpui_edit_depth") += 1;
                let _: () = msg_send![input, setSelectedRange: wanted];
                *(*input).get_mut_ivar::<usize>("gpui_edit_depth") -= 1;
            }
        }
    }

    /// A native edit ended: the field takes the view's whole text and its
    /// selection. The return key arrives as a newline in the text: it is
    /// taken out of the view and sent as the Enter key (a search, a
    /// sign-in).
    pub(crate) fn take_input_view(
        &self,
        input: *mut AnyObject,
        text: &str,
        selection: std::ops::Range<usize>,
        was_marked: bool,
    ) {
        let mut text = std::borrow::Cow::Borrowed(text);
        let mut selection = selection;
        let mut enter = false;
        if text.contains('\n') {
            enter = true;
            let before = text.encode_utf16().count();
            let stripped: String = text.chars().filter(|c| *c != '\n').collect();
            let after = stripped.encode_utf16().count();
            let removed = before - after;
            selection = selection.start.saturating_sub(removed)..selection.end.saturating_sub(removed);
            text = std::borrow::Cow::Owned(stripped);
            unsafe {
                #[allow(deprecated)]
                {
                    *(*input).get_mut_ivar::<usize>("gpui_edit_depth") += 1;
                    let _: () = msg_send![input, setText: crate::util::nsstring(&text)];
                    let range = super::text_input::ObjcNSRange { location: selection.start, length: selection.end - selection.start };
                    let _: () = msg_send![input, setSelectedRange: range];
                    *(*input).get_mut_ivar::<usize>("gpui_edit_depth") -= 1;
                }
            }
        }
        if let Some((current, len)) = self.gpui_document() {
            if was_marked || current != text.as_ref() {
                if let Some(handler) = self.input_handler.borrow_mut().as_mut() {
                    // The whole text, with the selection where the view has
                    // it; marked for a moment, since that is the one call
                    // that places the selection, then unmarked.
                    handler.replace_and_mark_text_in_range(Some(0..len), &text, Some(selection.clone()));
                    handler.unmark_text();
                }
            }
        }
        if enter {
            let event = PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke {
                    modifiers: Modifiers::default(),
                    key: "enter".to_string(),
                    key_char: None,
                },
                is_held: false,
                prefer_character_input: false,
            });
            if let Some(callback) = self.input_callback.borrow_mut().as_mut() {
                callback(event);
            }
        }
    }

    /// The traits of the focused field changed (`set_text_traits`): the
    /// composition view takes them, and the keyboard, if it is up, reloads
    /// to show them (another return key, a password keyboard).
    pub fn apply_text_traits(&self) {
        if self.text_input_view.is_null() {
            return;
        }
        // On the next turn of the run loop (`gpuiApplyTraits`), as the
        // first show is: this is called from the render of the field, and
        // UIKit's keyboard work (a reload, a change of first responder,
        // the passwords bar) calls back into GPUI at once. On the phone
        // that aborted the app at the tap on the password field
        // (2026-10-22).
        unsafe {
            let _: () = msg_send![self.text_input_view,
                performSelector: sel!(gpuiApplyTraits),
                withObject: ptr::null::<AnyObject>(),
                afterDelay: 0.0_f64
            ];
        }
    }

    /// `apply_text_traits`, from the run loop.
    pub(crate) fn apply_text_traits_now(&self) {
        let traits = crate::text_traits();
        let input = self.input_view_for(&traits);
        let other = self.rest_other_input_view(input);
        self.attach_input_view(input);
        unsafe {
            super::text_input::configure_keyboard_traits(input, &traits);
            if self.keyboard_shown.get() {
                let other_has_it: Bool = msg_send![other, isFirstResponder];
                if other_has_it.as_bool() {
                    // The focus went from a credential to another kind of
                    // field (or back) with the keyboard up: the other view
                    // takes over.
                    let _: () = msg_send![other, gpuiResetComposition];
                    let _: Bool = msg_send![other, resignFirstResponder];
                    // Out of the hierarchy before the other view takes
                    // the keyboard, or the keyboard keeps the Passwords
                    // bar for the username field (it is set up at
                    // `becomeFirstResponder`).
                    if other == self.credential_input_view {
                        let _: () = msg_send![other, removeFromSuperview];
                    }
                    let _: Bool = msg_send![input, becomeFirstResponder];
                } else {
                    let _: () = msg_send![input, reloadInputViews];
                }
            }
            // Now that it holds no focus, the credential view at rest
            // leaves the hierarchy (`rest_other_input_view` skips a view
            // that still has it: a removal resigns it, and the keyboard
            // went down on the switch to the username field).
            if other == self.credential_input_view {
                let _: () = msg_send![other, removeFromSuperview];
            }
        }
    }

    /// The composition view that is not `input` goes back to the default
    /// traits, and is returned. iOS offers the saved passwords on every
    /// field of a window that holds a view typed as a username or a
    /// password, in use or not: the Passwords bar stayed on the search
    /// field after a sign-in field (2026-10-22). A view at rest is typed
    /// as nothing.
    fn rest_other_input_view(&self, input: *mut AnyObject) -> *mut AnyObject {
        let other = if input == self.text_input_view {
            self.credential_input_view
        } else {
            self.text_input_view
        };
        unsafe {
            super::text_input::configure_keyboard_traits(other, &crate::TextTraits::DEFAULT);
            // Typing the view as nothing was not enough after a password
            // field: the Passwords bar stayed on the search field
            // (2026-10-23). iOS looks through the window's views for a
            // field that was secure, so the credential view leaves the
            // hierarchy while at rest (`attach_input_view` puts it back).
            let has_focus: Bool = msg_send![other, isFirstResponder];
            if other == self.credential_input_view && !has_focus.as_bool() {
                let _: () = msg_send![other, removeFromSuperview];
            }
        }
        other
    }

    /// Puts a composition view back under the window's view when it left
    /// it at rest (`rest_other_input_view`), before it takes the focus.
    fn attach_input_view(&self, input: *mut AnyObject) {
        unsafe {
            let superview: *mut AnyObject = msg_send![input, superview];
            if superview.is_null() {
                let _: () = msg_send![self.view, addSubview: input];
            }
        }
    }

    /// The composition view for a field with these traits: the credential
    /// one for a username or a password, the other for the rest.
    fn input_view_for(&self, traits: &crate::TextTraits) -> *mut AnyObject {
        use crate::TextContent;
        let credential = traits.secure
            || matches!(
                traits.content,
                TextContent::Username | TextContent::Password | TextContent::NewPassword
            );
        if credential { self.credential_input_view } else { self.text_input_view }
    }

    /// The composition view of the field that has the focus.
    fn active_input_view(&self) -> *mut AnyObject {
        self.input_view_for(&crate::text_traits())
    }

    /// Shows the system's edit menu over a text field (`UIEditMenuInteraction`,
    /// iOS 16): Cut, Copy, Paste and Select All, as the menu offers them.
    /// The chosen entry goes to `on_action`, once.
    pub fn present_edit_menu(&self, menu: crate::EditMenu, on_action: Box<dyn FnMut(crate::EditAction)>) {
        if self.view.is_null() {
            return;
        }
        {
            let mut state = self.edit_menu.borrow_mut();
            state.menu = menu;
            state.on_action = Some(on_action);
            state.blocks.clear();
        }
        unsafe {
            if self.edit_menu_interaction.get().is_null() {
                let Some(class) = AnyClass::get(c"UIEditMenuInteraction") else {
                    log::warn!("GPUI iOS: no UIEditMenuInteraction (iOS 16 and later); no edit menu");
                    return;
                };
                let delegate_class = register_edit_menu_delegate_class();
                let delegate: *mut AnyObject = msg_send![delegate_class, alloc];
                let delegate: *mut AnyObject = msg_send![delegate, init];
                #[allow(deprecated)]
                {
                    *(*delegate).get_mut_ivar::<*mut c_void>(GPUI_WINDOW_IVAR) =
                        self as *const IosWindow as *mut c_void;
                }
                let interaction: *mut AnyObject = msg_send![class, alloc];
                let interaction: *mut AnyObject = msg_send![interaction, initWithDelegate: delegate];
                let _: () = msg_send![self.view, addInteraction: interaction];
                self.edit_menu_delegate.set(delegate);
                self.edit_menu_interaction.set(interaction);
            }
            // The menu points at the top middle of the field; the delegate
            // gives the whole rect of the field as the target.
            let source = ObjcCGPoint { x: (menu.x + menu.w / 2.) as f64, y: menu.y as f64 };
            let config: *mut AnyObject = msg_send![class!(UIEditMenuConfiguration),
                configurationWithIdentifier: ptr::null::<AnyObject>(), sourcePoint: source];
            let _: () = msg_send![self.edit_menu_interaction.get(), presentEditMenuWithConfiguration: config];
        }
    }

    /// The user chose an entry of the edit menu.
    fn edit_menu_fire(&self, action: crate::EditAction) {
        // Out of the cell before the call: the app may show another menu
        // or move the focus from inside the callback.
        let on_action = self.edit_menu.borrow_mut().on_action.take();
        if let Some(mut on_action) = on_action {
            on_action(action);
        }
    }

    /// The rect of the field the edit menu is for, for the delegate.
    fn edit_menu_rect(&self) -> ObjcCGRect {
        let menu = self.edit_menu.borrow().menu;
        ObjcCGRect::new(menu.x as f64, menu.y as f64, menu.w as f64, menu.h as f64)
    }

    /// The `UIMenu` of the edit menu: an action for each entry the field
    /// offers, each with a block that fires `edit_menu_fire`.
    unsafe fn edit_menu_build(&self) -> *mut AnyObject {
        use crate::EditAction;
        let menu = self.edit_menu.borrow().menu;
        let entries: [(bool, &str, EditAction); 4] = [
            (menu.cut, "Cut", EditAction::Cut),
            (menu.copy, "Copy", EditAction::Copy),
            (menu.paste, "Paste", EditAction::Paste),
            (menu.select_all, "Select All", EditAction::SelectAll),
        ];
        let window_ptr = self as *const IosWindow as usize;
        unsafe {
            let children: *mut AnyObject = msg_send![class!(NSMutableArray), array];
            for (offered, title, action) in entries {
                if !offered {
                    continue;
                }
                let block = block2::RcBlock::new(move |_action: *mut AnyObject| {
                    let window = &*(window_ptr as *const IosWindow);
                    window.edit_menu_fire(action);
                });
                let ui_action: *mut AnyObject = msg_send![class!(UIAction),
                    actionWithTitle: crate::util::nsstring(title),
                    image: ptr::null::<AnyObject>(),
                    identifier: ptr::null::<AnyObject>(),
                    handler: &*block];
                let _: () = msg_send![children, addObject: ui_action];
                // UIKit copies the block; keeping ours is cheap insurance.
                self.edit_menu.borrow_mut().blocks.push(block);
            }
            msg_send![class!(UIMenu), menuWithChildren: children]
        }
    }

    /// Hide the software keyboard.
    ///
    /// Deferred to the next run-loop iteration (like `show_keyboard_with_type`)
    /// to avoid re-entering GPUI event dispatch.
    pub fn hide_keyboard(&self) {
        log::info!("GPUI iOS: Hiding keyboard");
        unsafe {
            for input in [self.text_input_view, self.credential_input_view] {
                // Typed as nothing while down (`rest_other_input_view`).
                super::text_input::configure_keyboard_traits(input, &crate::TextTraits::DEFAULT);
                let _: () = msg_send![input, gpuiResetComposition];
                let _: () = msg_send![input,
                    performSelector: sel!(resignFirstResponder),
                    withObject: ptr::null::<AnyObject>(),
                    afterDelay: 0.0_f64
                ];
            }
            // The credential view leaves the hierarchy while down, or the
            // next field's keyboard offers the Passwords bar.
            let _: () = msg_send![self.credential_input_view, removeFromSuperview];
        }
    }

    /// Handle text input from the software keyboard
    pub fn handle_text_input(&self, text: *mut AnyObject) {
        if text.is_null() {
            return;
        }

        unsafe {
            // Convert NSString to Rust String
            let utf8: *const i8 = msg_send![text, UTF8String];
            if utf8.is_null() {
                return;
            }

            let text_str = std::ffi::CStr::from_ptr(utf8)
                .to_string_lossy()
                .into_owned();

            log::info!("GPUI iOS: Text input: {:?}", text_str);

            // The return key of the keyboard comes as a newline: the app
            // takes it as the Enter key (a search, a sign-in), not as text.
            if text_str == "\n" {
                let event = PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: gpui::Keystroke {
                        modifiers: Modifiers::default(),
                        key: "enter".to_string(),
                        key_char: None,
                    },
                    is_held: false,
                    prefer_character_input: false,
                });
                if let Some(callback) = self.input_callback.borrow_mut().as_mut() {
                    callback(event);
                }
                return;
            }

            // Try the global text input callback (for our TextInput components).
            // The text is captured in PENDING_TEXT regardless of whether we also
            // send key events below.
            let dispatched = crate::dispatch_text_input(&text_str);

            // Try the input handler (for GPUI's built-in text fields)
            if !dispatched {
                if let Some(handler) = self.input_handler.borrow_mut().as_mut() {
                    handler.replace_text_in_range(None, &text_str);
                    return;
                }
            }

            // Send key events through GPUI's input callback.
            // Even if dispatch_text_input captured the text, we still send key
            // events so GPUI triggers a re-render cycle (which runs
            // drain_pending_text and updates the UI).
            for c in text_str.chars() {
                let keystroke = gpui::Keystroke {
                    modifiers: Modifiers::default(),
                    key: c.to_string(),
                    key_char: Some(c.to_string()),
                };

                let event = PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke,
                    is_held: false,
                    prefer_character_input: true,
                });

                if let Some(callback) = self.input_callback.borrow_mut().as_mut() {
                    callback(event);
                }
            }
        }
    }

    /// Handle the delete-backward action from the software keyboard.
    ///
    /// This is called by the `GPUITextInputView` when the user taps the
    /// backspace key.  We dispatch a special sentinel ("\x08") through the
    /// global text input callback so the active TextInput component can
    /// remove the last character.
    pub fn handle_delete_backward(&self) {
        log::info!("GPUI iOS: deleteBackward");

        // Try the global callback first (backspace = "\x08")
        crate::dispatch_text_input("\x08");

        // Always send a Backspace KeyDown event through GPUI to trigger
        // a re-render cycle (which runs drain_pending_text).
        let keystroke = gpui::Keystroke {
            modifiers: Modifiers::default(),
            key: "backspace".to_string(),
            key_char: None,
        };
        let event = PlatformInput::KeyDown(gpui::KeyDownEvent {
            keystroke,
            is_held: false,
            prefer_character_input: false,
        });
        if let Some(callback) = self.input_callback.borrow_mut().as_mut() {
            callback(event);
        }
    }

    /// Handle a key event from an external keyboard
    pub fn handle_key_event(&self, key_code: u32, modifier_flags: u32, is_key_down: bool) {
        use super::text_input::{
            key_code_to_key_down, key_code_to_key_up, key_code_to_string,
            modifier_flags_to_modifiers,
        };

        let key = key_code_to_string(key_code);
        let modifiers = modifier_flags_to_modifiers(modifier_flags);

        log::info!(
            "GPUI iOS: Key event - key: {:?}, modifiers: {:?}, down: {}",
            key,
            modifiers,
            is_key_down
        );

        // On key-down, dispatch cursor-movement control codes through the
        // global text input callback so TextField-based components receive them.
        if is_key_down {
            match key_code {
                0x50 => {
                    crate::dispatch_text_input("\x1b[D");
                } // Left arrow
                0x4F => {
                    crate::dispatch_text_input("\x1b[C");
                } // Right arrow
                0x4A => {
                    crate::dispatch_text_input("\x1b[H");
                } // Home
                0x4D => {
                    crate::dispatch_text_input("\x1b[F");
                } // End
                _ => {}
            }
        }

        let event = if is_key_down {
            key_code_to_key_down(key_code, modifier_flags)
        } else {
            key_code_to_key_up(key_code, modifier_flags)
        };

        if let Some(callback) = self.input_callback.borrow_mut().as_mut() {
            callback(event);
        }
    }

    /// Notify the window of active status changes (foreground/background).
    ///
    /// This is called by the FFI layer when the app transitions between
    /// foreground and background states.
    pub fn notify_active_status_change(&self, is_active: bool) {
        log::info!("GPUI iOS: Window active status changed to: {}", is_active);

        if let Some(callback) = self.active_status_callback.borrow_mut().as_mut() {
            callback(is_active);
        }
    }

    /// Handle a layout change (e.g. rotation, split-screen resize).
    ///
    /// Called from `viewDidLayoutSubviews` on the GPUIViewController.
    /// Queries the current UIView bounds, updates the stored bounds/scale,
    /// gives the Metal layer the new frame and drawable size, and fires the
    /// resize callback.
    pub fn handle_layout_change(&self) {
        unsafe {
            let view_bounds: ObjcCGRect = msg_send![self.view, bounds];
            let screen: *mut AnyObject = msg_send![class!(UIScreen), mainScreen];
            let scale: core_graphics::base::CGFloat = msg_send![screen, scale];

            let new_w = view_bounds.width as f32;
            let new_h = view_bounds.height as f32;
            if new_w <= 0.0 || new_h <= 0.0 {
                return;
            }
            let new_scale = scale as f32;

            let old_bounds = self.bounds.get();
            let old_scale = self.scale_factor.get();

            let new_size = size(px(new_w), px(new_h));

            // Only process if something actually changed.
            if old_bounds.size == new_size && (old_scale - new_scale).abs() < 0.01 {
                return;
            }

            log::info!(
                "GPUI iOS: Layout changed — {:?} @{:.1}x → {:?} @{:.1}x",
                old_bounds.size,
                old_scale,
                new_size,
                new_scale,
            );

            // Update stored bounds (in logical pixels, matching GPUI convention).
            let new_bounds = Bounds {
                origin: Default::default(),
                size: new_size,
            };
            self.bounds.set(new_bounds);
            self.scale_factor.set(new_scale);

            // The Metal layer takes the frame of the view and the scale of
            // the screen, without an animation of the change.
            let pixel_w = (new_w * new_scale) as i32;
            let pixel_h = (new_h * new_scale) as i32;
            {
                let mut renderer = self.renderer.lock();
                let metal_layer = renderer.layer_ptr() as *mut AnyObject;
                let _: () = msg_send![class!(CATransaction), begin];
                let _: () = msg_send![class!(CATransaction), setDisableActions: true];
                let _: () = msg_send![metal_layer, setContentsScale: scale];
                let _: () = msg_send![
                    metal_layer,
                    setFrame: ObjcCGRect::new(0.0, 0.0, view_bounds.width, view_bounds.height)
                ];
                renderer.update_drawable_size(size(DevicePixels(pixel_w), DevicePixels(pixel_h)));
                let video_layer = VIDEO_LAYER.load(std::sync::atomic::Ordering::Acquire);
                if !video_layer.is_null() {
                    let _: () = msg_send![video_layer, setContentsScale: scale];
                    let _: () = msg_send![
                        video_layer,
                        setFrame: ObjcCGRect::new(0.0, 0.0, view_bounds.width, view_bounds.height)
                    ];
                    let _: () = msg_send![
                        video_layer,
                        setDrawableSize: super::cg_types::ObjcCGSize { width: pixel_w as f64, height: pixel_h as f64 }
                    ];
                }
                let _: () = msg_send![class!(CATransaction), commit];
            }

            // Fire the resize callback so GPUI re-layouts at the new size.
            let cb = self.resize_callback.borrow_mut().take();
            if let Some(mut cb) = cb {
                cb(new_size, new_scale);
                // Restore the callback for future resize events.
                let mut slot = self.resize_callback.borrow_mut();
                if slot.is_none() {
                    *slot = Some(cb);
                }
            }
        }
    }
}

impl HasWindowHandle for IosWindow {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        let view = NonNull::new(self.view as *mut c_void)
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        let handle = UiKitWindowHandle::new(view);
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl HasDisplayHandle for IosWindow {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
    {
        let handle = UiKitDisplayHandle::new();
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(handle.into()) })
    }
}

impl PlatformWindow for IosWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds.get()
    }

    fn is_maximized(&self) -> bool {
        true // iOS windows are always "maximized"
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds.get())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.bounds.get().size
    }

    fn resize(&mut self, _size: Size<Pixels>) {
        // iOS windows cannot be resized programmatically
    }

    fn scale_factor(&self) -> f32 {
        self.scale_factor.get()
    }

    fn appearance(&self) -> WindowAppearance {
        unsafe {
            let trait_collection: *mut AnyObject = msg_send![self.view, traitCollection];
            let style: i64 = msg_send![trait_collection, userInterfaceStyle];
            match style {
                2 => WindowAppearance::Dark,
                _ => WindowAppearance::Light,
            }
        }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(IosDisplay::main()))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.mouse_position.get()
    }

    fn modifiers(&self) -> Modifiers {
        self.modifiers.get()
    }

    fn capslock(&self) -> Capslock {
        // Would need to check UIKeyModifierFlags
        Capslock { on: false }
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        *self.input_handler.borrow_mut() = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.input_handler.borrow_mut().take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        // Would use UIAlertController
        let (_tx, rx) = futures::channel::oneshot::channel();

        unsafe {
            // Create UIAlertController
            let title = msg;
            let message = detail.unwrap_or("");

            let alert_style: i64 = 1; // UIAlertControllerStyleAlert

            let title_str: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: title.as_ptr()];
            let message_str: *mut AnyObject =
                msg_send![class!(NSString), stringWithUTF8String: message.as_ptr()];

            let alert: *mut AnyObject = msg_send![
                class!(UIAlertController),
                alertControllerWithTitle: title_str,
                message: message_str,
                preferredStyle: alert_style
            ];

            // Add buttons
            for button in answers.iter() {
                let button_title: *mut AnyObject = msg_send![
                    class!(NSString),
                    stringWithUTF8String: button.label().as_str().as_ptr()
                ];

                let action_style: i64 = if button.is_cancel() { 1 } else { 0 }; // UIAlertActionStyleCancel or Default

                // Note: In production, this would need a block that calls tx.send(index)
                let action: *mut AnyObject = msg_send![
                    class!(UIAlertAction),
                    actionWithTitle: button_title,
                    style: action_style,
                    handler: ptr::null::<AnyObject>()
                ];

                let _: () = msg_send![alert, addAction: action];
            }

            // Present the alert
            let _: () = msg_send![
                self.view_controller,
                presentViewController: alert,
                animated: true,
                completion: ptr::null::<AnyObject>()
            ];
        }

        Some(rx)
    }

    fn activate(&self) {
        unsafe {
            if !self.window.is_null() {
                let _: () = msg_send![self.window, makeKeyAndVisible];
            }
        }
    }

    fn is_active(&self) -> bool {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(UIApplication), sharedApplication];
            let key_window: *mut AnyObject = msg_send![app, keyWindow];
            let host_window: *mut AnyObject = msg_send![self.view, window];
            !host_window.is_null() && host_window == key_window
        }
    }

    fn is_hovered(&self) -> bool {
        // Hover isn't really applicable on iOS
        false
    }

    fn set_title(&mut self, _title: &str) {
        // iOS apps don't have window titles
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {
        // Could adjust view background color
    }

    fn minimize(&self) {
        // iOS apps cannot be minimized
    }

    fn zoom(&self) {
        // iOS apps cannot be zoomed
    }

    fn toggle_fullscreen(&self) {
        // iOS apps are always fullscreen
    }

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        *self.request_frame_callback.borrow_mut() = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        *self.input_callback.borrow_mut() = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        *self.active_status_callback.borrow_mut() = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        *self.hover_status_callback.borrow_mut() = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        *self.resize_callback.borrow_mut() = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        *self.moved_callback.borrow_mut() = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        *self.should_close_callback.borrow_mut() = Some(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        *self.hit_test_callback.borrow_mut() = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        *self.close_callback.borrow_mut() = Some(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        *self.appearance_changed_callback.borrow_mut() = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        // The software keyboard follows the input handler: GPUI sets one
        // while a text field has the focus (each paint), and none when the
        // focus left it. Nothing in GPUI asks for the keyboard itself.
        let wants_keyboard = self.input_handler.borrow().is_some();
        if wants_keyboard != self.keyboard_shown.get() {
            self.keyboard_shown.set(wants_keyboard);
            if wants_keyboard {
                self.show_keyboard_with_type(crate::KeyboardType::Default);
            } else {
                self.hide_keyboard();
            }
        }
        self.renderer.lock().draw(scene);
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.renderer.lock().sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        // As the Mac window: grayscale antialiasing.
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        None
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // iOS handles IME positioning automatically
    }

    /// The bridge to UIAccessibility (VoiceOver): the tree is built when
    /// VoiceOver runs at the start, or the first time it asks the view for
    /// its elements (`accessibilityElements`), or with `BLOOM_A11Y_TREE=1`
    /// (the debug channel of the app, `a11y`: a test reads the labels and
    /// bounds of the elements from it and taps them).
    fn a11y_init(&self, callbacks: gpui::A11yCallbacks) {
        *self.a11y_callbacks.borrow_mut() = Some(callbacks);
        let wanted = std::env::var_os("BLOOM_A11Y_TREE").is_some_and(|v| !v.is_empty())
            || unsafe { UIAccessibilityIsVoiceOverRunning() }.as_bool();
        if wanted {
            self.a11y_activate();
        }
    }

    fn a11y_tree_update(&self, tree_update: gpui::accesskit::TreeUpdate) {
        {
            let mut nodes = self.a11y_nodes.borrow_mut();
            if let Some(tree) = tree_update.tree {
                self.a11y_root.set(Some(tree.root));
            }
            for (id, node) in tree_update.nodes {
                nodes.insert(id, node);
            }
            // What the tree does not reach any more is gone.
            if let Some(root) = self.a11y_root.get() {
                let mut keep = std::collections::HashSet::new();
                let mut stack = vec![root];
                while let Some(id) = stack.pop() {
                    if !keep.insert(id) {
                        continue;
                    }
                    if let Some(node) = nodes.get(&id) {
                        stack.extend(node.children().iter().copied());
                    }
                }
                nodes.retain(|id, _| keep.contains(id));
            }
        }
        self.a11y_rebuild_elements();
    }
}

// UIKit's accessibility functions and the traits of an element.
#[link(name = "UIKit", kind = "framework")]
unsafe extern "C" {
    fn UIAccessibilityConvertFrameToScreenCoordinates(rect: ObjcCGRect, view: *mut AnyObject) -> ObjcCGRect;
    fn UIAccessibilityPostNotification(notification: u32, argument: *mut AnyObject);
    fn UIAccessibilityIsVoiceOverRunning() -> Bool;
    static UIAccessibilityLayoutChangedNotification: u32;
    static UIAccessibilityTraitButton: u64;
    static UIAccessibilityTraitLink: u64;
    static UIAccessibilityTraitImage: u64;
    static UIAccessibilityTraitSelected: u64;
    static UIAccessibilityTraitStaticText: u64;
    static UIAccessibilityTraitNotEnabled: u64;
    static UIAccessibilityTraitSearchField: u64;
    static UIAccessibilityTraitAdjustable: u64;
    static UIAccessibilityTraitHeader: u64;
}

const A11Y_NODE_IVAR: &str = "gpui_node_id";
static A11Y_ELEMENT_CLASS_REGISTERED: std::sync::Once = std::sync::Once::new();

/// The `UIAccessibilityElement` subclass that stands for one node of the
/// tree. Its frame, label, value and traits are read from the node at
/// each ask, so a scroll moves the elements without a new list; an
/// activation goes back to GPUI as a click of the node, and the increment
/// and decrement of a slider as its actions.
fn register_a11y_element_class() -> &'static AnyClass {
    A11Y_ELEMENT_CLASS_REGISTERED.call_once(|| {
        let mut decl = ClassBuilder::new(c"GPUIA11yElement", class!(UIAccessibilityElement)).unwrap();
        decl.add_ivar::<*mut std::ffi::c_void>(c"gpui_window_ptr");
        decl.add_ivar::<u64>(c"gpui_node_id");

        fn with_node<R>(this: *mut AnyObject, f: impl FnOnce(&IosWindow, &gpui::accesskit::Node) -> R) -> Option<R> {
            unsafe {
                #[allow(deprecated)]
                let window_ptr: *mut std::ffi::c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
                #[allow(deprecated)]
                let id: u64 = *(*this).get_ivar(A11Y_NODE_IVAR);
                let window = (window_ptr as *const IosWindow).as_ref()?;
                let nodes = window.a11y_nodes.borrow();
                let node = nodes.get(&gpui::accesskit::NodeId(id))?;
                Some(f(window, node))
            }
        }

        extern "C" fn frame(this: *mut AnyObject, _sel: Sel) -> ObjcCGRect {
            with_node(this, |window, node| {
                let scale = window.scale_factor.get() as f64;
                let rect = match node.bounds() {
                    Some(r) => ObjcCGRect::new(r.x0 / scale, r.y0 / scale, (r.x1 - r.x0) / scale, (r.y1 - r.y0) / scale),
                    None => ObjcCGRect::new(0., 0., 0., 0.),
                };
                unsafe { UIAccessibilityConvertFrameToScreenCoordinates(rect, window.view) }
            })
            .unwrap_or_else(|| ObjcCGRect::new(0., 0., 0., 0.))
        }

        extern "C" fn label(this: *mut AnyObject, _sel: Sel) -> *mut AnyObject {
            with_node(this, |_, node| {
                // A label made of an id ("play", "remember audio selections")
                // is spoken with a capital, as a button's title would be; a
                // label with a capital of its own stays as it is.
                let label = node.label().map(|l| {
                    let mut chars = l.chars();
                    match chars.next() {
                        Some(first) if first.is_lowercase() && !l.chars().any(char::is_uppercase) => {
                            first.to_uppercase().to_string() + chars.as_str()
                        }
                        _ => l.to_string(),
                    }
                });
                nsstring_or_nil(label.as_deref())
            })
            .unwrap_or(ptr::null_mut())
        }

        extern "C" fn value(this: *mut AnyObject, _sel: Sel) -> *mut AnyObject {
            with_node(this, |_, node| {
                if let Some(value) = node.value() {
                    return nsstring_or_nil(Some(value));
                }
                // A slider says its share in percent, as the sliders of iOS do.
                match (node.numeric_value(), node.min_numeric_value(), node.max_numeric_value()) {
                    (Some(v), Some(min), Some(max)) if max > min => {
                        nsstring_or_nil(Some(&format!("{:.0}%", (v - min) / (max - min) * 100.)))
                    }
                    (Some(v), _, _) => nsstring_or_nil(Some(&format!("{v:.0}"))),
                    _ => ptr::null_mut(),
                }
            })
            .unwrap_or(ptr::null_mut())
        }

        extern "C" fn traits(this: *mut AnyObject, _sel: Sel) -> u64 {
            use gpui::accesskit::Role;
            with_node(this, |_, node| unsafe {
                let mut traits = match node.role() {
                    Role::Button
                    | Role::MenuItem
                    | Role::MenuItemCheckBox
                    | Role::MenuItemRadio
                    | Role::Tab
                    | Role::CheckBox
                    | Role::RadioButton
                    | Role::Switch
                    | Role::ComboBox => UIAccessibilityTraitButton,
                    Role::Link => UIAccessibilityTraitLink,
                    Role::Image => UIAccessibilityTraitImage,
                    Role::Slider => UIAccessibilityTraitAdjustable,
                    Role::TextInput => UIAccessibilityTraitSearchField,
                    Role::Heading => UIAccessibilityTraitHeader,
                    _ => UIAccessibilityTraitStaticText,
                };
                // A checked box or a picked entry reads as selected.
                if node.is_selected() == Some(true) || node.toggled() == Some(gpui::accesskit::Toggled::True) {
                    traits |= UIAccessibilityTraitSelected;
                }
                if node.is_disabled() {
                    traits |= UIAccessibilityTraitNotEnabled;
                }
                traits
            })
            .unwrap_or(0)
        }

        fn act(this: *mut AnyObject, action: gpui::accesskit::Action) -> bool {
            unsafe {
                #[allow(deprecated)]
                let window_ptr: *mut std::ffi::c_void = *(*this).get_ivar(GPUI_WINDOW_IVAR);
                #[allow(deprecated)]
                let id: u64 = *(*this).get_ivar(A11Y_NODE_IVAR);
                let Some(window) = (window_ptr as *const IosWindow).as_ref() else { return false };
                let callbacks = window.a11y_callbacks.borrow();
                let Some(callbacks) = callbacks.as_ref() else { return false };
                (callbacks.action)(gpui::accesskit::ActionRequest {
                    action,
                    target_tree: gpui::accesskit::TreeId::ROOT,
                    target_node: gpui::accesskit::NodeId(id),
                    data: None,
                });
                super::ffi::request_frame(window);
                true
            }
        }

        extern "C" fn activate(this: *mut AnyObject, _sel: Sel) -> Bool {
            Bool::new(act(this, gpui::accesskit::Action::Click))
        }
        extern "C" fn increment(this: *mut AnyObject, _sel: Sel) {
            act(this, gpui::accesskit::Action::Increment);
        }
        extern "C" fn decrement(this: *mut AnyObject, _sel: Sel) {
            act(this, gpui::accesskit::Action::Decrement);
        }

        unsafe {
            decl.add_method(sel!(accessibilityFrame), frame as extern "C" fn(*mut AnyObject, Sel) -> ObjcCGRect);
            decl.add_method(sel!(accessibilityLabel), label as extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject);
            decl.add_method(sel!(accessibilityValue), value as extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject);
            decl.add_method(sel!(accessibilityTraits), traits as extern "C" fn(*mut AnyObject, Sel) -> u64);
            decl.add_method(sel!(accessibilityActivate), activate as extern "C" fn(*mut AnyObject, Sel) -> Bool);
            decl.add_method(sel!(accessibilityIncrement), increment as extern "C" fn(*mut AnyObject, Sel));
            decl.add_method(sel!(accessibilityDecrement), decrement as extern "C" fn(*mut AnyObject, Sel));
        }
        decl.register();
    });
    class!(GPUIA11yElement)
}

/// An autoreleased `NSString` of the text, or nil for none.
fn nsstring_or_nil(text: Option<&str>) -> *mut AnyObject {
    let Some(text) = text else { return ptr::null_mut() };
    let Ok(c) = std::ffi::CString::new(text) else { return ptr::null_mut() };
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

impl IosWindow {
    /// Asks GPUI for the tree and keeps its updates from then on.
    fn a11y_activate(&self) {
        if self.a11y_active.replace(true) {
            return;
        }
        let initial = self.a11y_callbacks.borrow().as_ref().and_then(|c| (c.activation)());
        if let Some(initial) = initial {
            self.a11y_tree_update(initial);
        }
        log::info!("GPUI iOS: the accessibility tree is built");
    }

    /// The array of elements the view answers VoiceOver with; nil before
    /// the tree exists. The first ask starts the tree.
    pub(crate) fn a11y_elements_array(&self) -> *mut AnyObject {
        if !self.a11y_active.get() {
            self.a11y_activate();
            super::ffi::request_frame(self);
        }
        self.a11y_array.get()
    }

    /// Makes the list of elements again when the set of nodes changed, and
    /// tells VoiceOver. The order is the reading order: top to bottom,
    /// then left to right.
    fn a11y_rebuild_elements(&self) {
        use gpui::accesskit::Role;
        let scale = self.scale_factor.get() as f64;
        let nodes = self.a11y_nodes.borrow();
        let mut wanted: Vec<(u64, f64, f64)> = nodes
            .iter()
            .filter(|(_, node)| {
                let role = node.role();
                let spoken = matches!(
                    role,
                    Role::Button | Role::MenuItem | Role::MenuItemCheckBox | Role::MenuItemRadio | Role::Tab
                        | Role::Slider | Role::TextInput | Role::CheckBox | Role::RadioButton | Role::Switch
                        | Role::ComboBox | Role::Link | Role::Image | Role::Heading | Role::Label
                );
                spoken && node.label().is_some_and(|l| !l.is_empty()) && node.bounds().is_some_and(|r| r.x1 > r.x0 && r.y1 > r.y0)
            })
            .map(|(id, node)| {
                let r = node.bounds().unwrap();
                (id.0, (r.y0 / scale / 8.).round(), r.x0 / scale)
            })
            .collect();
        // Rows of 8 points: the buttons of a bar read left to right.
        wanted.sort_by(|a, b| (a.1, a.2).partial_cmp(&(b.1, b.2)).unwrap_or(std::cmp::Ordering::Equal));
        let mut signature: u64 = 0xcbf29ce484222325;
        for (id, _, _) in &wanted {
            let node = &nodes[&gpui::accesskit::NodeId(*id)];
            for byte in id.to_le_bytes().iter().chain(node.label().unwrap_or_default().as_bytes()) {
                signature ^= u64::from(*byte);
                signature = signature.wrapping_mul(0x100000001b3);
            }
            signature ^= node.role() as u64;
            signature = signature.wrapping_mul(0x100000001b3);
        }
        drop(nodes);
        if signature == self.a11y_signature.get() {
            return;
        }
        self.a11y_signature.set(signature);
        unsafe {
            for old in self.a11y_elements.borrow_mut().drain(..) {
                let _: () = msg_send![old, release];
            }
            let old = self.a11y_array.replace(ptr::null_mut());
            if !old.is_null() {
                let _: () = msg_send![old, release];
            }
            let class = register_a11y_element_class();
            let mut elements = Vec::with_capacity(wanted.len());
            for (id, _, _) in &wanted {
                let element: *mut AnyObject = msg_send![class, alloc];
                let element: *mut AnyObject = msg_send![element, initWithAccessibilityContainer: self.view];
                #[allow(deprecated)]
                {
                    *(*element).get_mut_ivar::<*mut std::ffi::c_void>(GPUI_WINDOW_IVAR) = self as *const IosWindow as *mut _;
                    *(*element).get_mut_ivar::<u64>(A11Y_NODE_IVAR) = *id;
                }
                elements.push(element);
            }
            let array: *mut AnyObject = msg_send![class!(NSArray), arrayWithObjects: elements.as_ptr(), count: elements.len()];
            let _: *mut AnyObject = msg_send![array, retain];
            self.a11y_array.set(array);
            *self.a11y_elements.borrow_mut() = elements;
            UIAccessibilityPostNotification(UIAccessibilityLayoutChangedNotification, ptr::null_mut());
        }
    }
}

impl IosWindow {
    /// Presents an `SFSafariViewController` with the page over the app's
    /// view controller (see `gpui_ios::present_web`).
    pub fn present_web(&self, url: &str) {
        let vc = self.view_controller;
        if vc.is_null() {
            return;
        }
        let Ok(c_url) = std::ffi::CString::new(url) else { return };
        unsafe {
            let string: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c_url.as_ptr()];
            let ns_url: *mut AnyObject = msg_send![class!(NSURL), URLWithString: string];
            if ns_url.is_null() {
                return;
            }
            let Some(class) = AnyClass::get(c"SFSafariViewController") else {
                log::warn!("GPUI iOS: SafariServices is not linked; the page is not shown");
                return;
            };
            let safari: *mut AnyObject = msg_send![class, alloc];
            let safari: *mut AnyObject = msg_send![safari, initWithURL: ns_url];
            // Over the top of the app, the way a modal sheet of iOS is.
            let _: () = msg_send![safari, setModalPresentationStyle: 2i64];
            let _: () = msg_send![vc, presentViewController: safari, animated: true, completion: ptr::null::<AnyObject>()];
        }
    }

    /// The accessibility tree as JSON: a list of nodes with `id`, `role`,
    /// `label`, `value`, `bounds` (x, y, w, h in points of the window) and
    /// `children`. Empty without `BLOOM_A11Y_TREE`.
    pub fn a11y_json(&self) -> String {
        let scale = self.scale_factor.get() as f64;
        let nodes = self.a11y_nodes.borrow();
        let list: Vec<serde_json::Value> = nodes
            .iter()
            .map(|(id, node)| {
                serde_json::json!({
                    "id": id.0.to_string(),
                    "role": format!("{:?}", node.role()),
                    "label": node.label(),
                    "value": node.value(),
                    "bounds": node.bounds().map(|r| serde_json::json!({
                        "x": r.x0 / scale, "y": r.y0 / scale,
                        "w": (r.x1 - r.x0) / scale, "h": (r.y1 - r.y0) / scale,
                    })),
                    "children": node.children().iter().map(|c| c.0.to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "root": self.a11y_root.get().map(|r| r.0.to_string()),
            "nodes": list,
        })
        .to_string()
    }
}

/// The seconds a keyboard slide takes, from the user info of a
/// `UIKeyboardWillShow` or `WillHide` notification; UIKit's usual quarter
/// second when the notification does not say.
fn keyboard_slide_seconds(user_info: *mut AnyObject) -> f64 {
    if user_info.is_null() {
        return 0.25;
    }
    unsafe {
        let key = crate::util::nsstring("UIKeyboardAnimationDurationUserInfoKey");
        let value: *mut AnyObject = msg_send![user_info, objectForKey: key];
        if value.is_null() {
            return 0.25;
        }
        let seconds: f64 = msg_send![value, doubleValue];
        if seconds > 0. { seconds } else { 0.25 }
    }
}

/// What the edit menu of a text field offers and does (`present_edit_menu`).
#[derive(Default)]
struct EditMenuState {
    menu: crate::EditMenu,
    on_action: Option<Box<dyn FnMut(crate::EditAction)>>,
    /// The handler blocks of the menu's actions, alive while the menu is.
    blocks: Vec<block2::RcBlock<dyn Fn(*mut AnyObject)>>,
}

static EDIT_MENU_DELEGATE_CLASS_REGISTERED: std::sync::Once = std::sync::Once::new();

/// The delegate of the edit menu interaction: it answers with the menu the
/// window builds (`edit_menu_build`) and the rect of the field.
fn register_edit_menu_delegate_class() -> &'static AnyClass {
    EDIT_MENU_DELEGATE_CLASS_REGISTERED.call_once(|| {
        let mut decl = ClassBuilder::new(c"GPUIEditMenuDelegate", class!(NSObject)).unwrap();
        decl.add_ivar::<*mut c_void>(c"gpui_window_ptr");

        unsafe fn window_of(this: *mut AnyObject) -> Option<&'static IosWindow> {
            #[allow(deprecated)]
            let window_ptr: *mut c_void = unsafe { *(*this).get_ivar(GPUI_WINDOW_IVAR) };
            if window_ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(window_ptr as *const IosWindow) })
            }
        }

        unsafe extern "C" fn menu_for_configuration(
            this: *mut AnyObject,
            _sel: Sel,
            _interaction: *mut AnyObject,
            _configuration: *mut AnyObject,
            _suggested: *mut AnyObject,
        ) -> *mut AnyObject {
            match unsafe { window_of(this) } {
                Some(window) => unsafe { window.edit_menu_build() },
                None => ptr::null_mut(),
            }
        }

        unsafe extern "C" fn target_rect_for_configuration(
            this: *mut AnyObject,
            _sel: Sel,
            _interaction: *mut AnyObject,
            _configuration: *mut AnyObject,
        ) -> ObjcCGRect {
            match unsafe { window_of(this) } {
                Some(window) => window.edit_menu_rect(),
                None => ObjcCGRect::new(0., 0., 0., 0.),
            }
        }

        unsafe {
            decl.add_method(
                sel!(editMenuInteraction:menuForConfiguration:suggestedActions:),
                menu_for_configuration
                    as unsafe extern "C" fn(
                        *mut AnyObject,
                        Sel,
                        *mut AnyObject,
                        *mut AnyObject,
                        *mut AnyObject,
                    ) -> *mut AnyObject,
            );
            decl.add_method(
                sel!(editMenuInteraction:targetRectForConfiguration:),
                target_rect_for_configuration
                    as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject) -> ObjcCGRect,
            );
        }
        decl.register();
    });
    class!(GPUIEditMenuDelegate)
}

/// The text of an `NSString`, empty for nil.
fn nsstring_to_string(string: *mut AnyObject) -> String {
    if string.is_null() {
        return String::new();
    }
    unsafe {
        let utf8: *const i8 = msg_send![string, UTF8String];
        if utf8.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
        }
    }
}
