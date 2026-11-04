//! macOS video surface: an NSOpenGLView rendered by libmpv, inserted *below*
//! the (transparent) WKWebView in the window's content view.
//!
//!   NSView (tao content view)
//!    ├─ MpvOpenGLView     (NSOpenGLView, fills the window)  ← mpv render API
//!    └─ WKWebView         ← React UI, transparent where video shows
//!
//! Mirrors `linux.rs` (GtkOverlay + GtkGLArea). The webview is made
//! transparent via WKWebView's `drawsBackground` KVC key (belt and braces
//! next to wry's `transparent` + `macOSPrivateApi` in tauri.conf.json), and
//! on the window side by clearing its background color.
//!
//! The `drawRect:` override lives in Swift (media_macos/, class
//! `TestpatternSurface.MpvOpenGLView`): NSOpenGLView subclasses cannot be
//! declared through plain objc2 FFI, and a dylib-free static archive keeps
//! the binary layout identical to Linux's. Rust hands Swift a render
//! callback pointer once; from then on AppKit's display cycle drives mpv.
//!
//! The web UI decides what is visible: opaque pages hide the video, the
//! player page is transparent and draws its controls on top of it.

// NSOpenGLView/CGL is deprecated since macOS 10.14, but it remains the only
// supported surface for mpv's OpenGL render API on macOS; silence the SDK
// warnings for the whole module.
#![allow(deprecated)]

use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::os::raw::{c_char, c_int};
use std::ptr;

// The Swift companion's static library is mostly ObjC metadata that nothing
// else references; force it in so dyld registers the class.
#[link(name = "TestpatternSurface", kind = "static")]
#[allow(dead_code)]
unsafe extern "C" {
    fn tp_surface_link_hack() -> c_int;
}
use std::sync::{Arc, OnceLock};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_app_kit::{NSOpenGLPixelFormat, NSOpenGLView, NSWindowOrderingMode};
use objc2_foundation::NSString;
use tauri::{Runtime, WebviewWindow, WindowEvent};

use super::mpv::Mpv;
use super::mpv_sys::{
    MPV_RENDER_API_TYPE_OPENGL, MPV_RENDER_PARAM_ADVANCED_CONTROL, MPV_RENDER_PARAM_API_TYPE,
    MPV_RENDER_PARAM_FLIP_Y, MPV_RENDER_PARAM_INVALID, MPV_RENDER_PARAM_OPENGL_FBO,
    MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, MPV_RENDER_UPDATE_FRAME, mpv_error_string, mpv_opengl_fbo,
    mpv_opengl_init_params, mpv_render_context, mpv_render_context_create, mpv_render_context_free,
    mpv_render_context_render, mpv_render_context_report_swap,
    mpv_render_context_set_update_callback, mpv_render_context_update, mpv_render_param,
};
use super::render::{GL_FRAMEBUFFER_BINDING, RedrawFlag, gl_get_integerv};

thread_local! {
    static SURFACE: RefCell<Option<Surface>> = const { RefCell::new(None) };
}

struct Surface {
    gl_view: Retained<NSOpenGLView>,
    mpv: Arc<Mpv>,
    ctx: *mut mpv_render_context,
}

static REDRAW: RedrawFlag = RedrawFlag::new();

/// Installs the video surface under the window's webview. Must be called
/// from the setup hook; `with_webview` runs on the main thread.
///
/// Single-window invariant: one app = one window = one video surface. `SURFACE`
/// is a single global slot; a second install would silently replace the first
/// surface and cross-render GL contexts, so we refuse loudly instead.
pub fn attach<R: Runtime>(window: &WebviewWindow<R>, mpv: Arc<Mpv>) -> tauri::Result<()> {
    if SURFACE.with(|s| s.borrow().is_some()) {
        log::error!(
            "video surface: a surface is already installed (single-window app); refusing second attach"
        );
        return Ok(());
    }
    // Mirror Linux's on_unrealize teardown: free the mpv render context and
    // drop the surface when the window closes so Arc<Mpv> can be released.
    window.on_window_event(|event| {
        if matches!(
            event,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            teardown();
        }
    });
    window.with_webview(move |platform| {
        // SAFETY: wry's `inner()` is its live, retained `*mut WKWebView`;
        // `with_webview` runs this closure on the main thread while wry keeps
        // the view alive, and the reference does not escape the closure.
        let webview: &objc2_web_kit::WKWebView = unsafe { &*(platform.inner() as *const _) };
        if let Err(e) = install(webview, mpv) {
            log::error!("video surface: {e}");
        }
    })
}

fn install(webview: &objc2_web_kit::WKWebView, mpv: Arc<Mpv>) -> Result<(), String> {
    // SAFETY: AppKit view-hierarchy call on the main thread (`with_webview`).
    let parent = unsafe { webview.superview() }.ok_or("WKWebView has no superview")?;

    // CGL/OpenGL view sized to the window, inserted *below* the webview.
    // Pixel format made on the Swift side — the objc2 binding of
    // initWithAttributes: goes through a calling convention that delivers nil.
    let frame = webview.frame();
    // SAFETY: main thread. `tp_mpvPixelFormat` is a class method of the
    // Swift class returning an autoreleased NSOpenGLPixelFormat (checked for
    // nil); `alloc` + `initWithFrame:pixelFormat:` is NSOpenGLView's
    // designated initializer with matching argument types; `Retained::retain`
    // takes its own +1 reference on the (checked) result.
    let gl_view = unsafe {
        let cls = swift_view_class()?;
        let pf: *mut NSOpenGLPixelFormat = msg_send![cls, tp_mpvPixelFormat];
        if pf.is_null() {
            log::error!("Swift tp_mpvPixelFormat returned nil");
            return Err("Swift tp_mpvPixelFormat failed".into());
        }
        let view: *mut NSOpenGLView = msg_send![cls, alloc];
        let view: *mut NSOpenGLView = msg_send![view, initWithFrame: frame, pixelFormat: pf];
        Retained::retain(view).ok_or("MpvOpenGLView init failed")?
    };
    // The CSS clip-path in the UI is what cuts the video hole; the view
    // sticks to the window's edges and paints nothing else.
    gl_view.setAutoresizingMask(
        objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
            | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    // SAFETY: NSView's `setWantsBestResolutionOpenGLSurface:` takes a BOOL
    // and returns void; main thread, live view.
    unsafe {
        let _: () = msg_send![&*gl_view, setWantsBestResolutionOpenGLSurface: true];
    }
    parent.addSubview_positioned_relativeTo(&gl_view, NSWindowOrderingMode::Below, Some(webview));

    // Let the GL view show through wherever the UI doesn't paint: the webview
    // stops drawing its background, the window below it goes clear.
    // SAFETY: KVC `setValue:forKey:` with an NSNumber and NSString that live
    // across the call; WKWebView has a `drawsBackground` key (otherwise KVC
    // raises, it does not corrupt memory); main thread.
    unsafe {
        let key = NSString::from_str("drawsBackground");
        let value = bool_number(false);
        let _: () = msg_send![webview, setValue: &*value, forKey: &*key];
    }
    if let Some(w) = webview.window() {
        let clear = objc2_app_kit::NSColor::clearColor();
        w.setBackgroundColor(Some(&clear));
        w.setOpaque(false);
    }

    SURFACE.with(|s| {
        *s.borrow_mut() = Some(Surface {
            gl_view,
            mpv,
            ctx: ptr::null_mut(),
        });
    });
    init_mpv();
    Ok(())
}

fn bool_number(b: bool) -> Retained<objc2_foundation::NSNumber> {
    objc2_foundation::NSNumber::new_bool(b)
}

/// The Swift class object, via its Objective-C runtime name. The static
/// archive is linked into the app, so the class is registered by the time
/// `install` runs.
fn swift_view_class() -> Result<&'static AnyClass, String> {
    // Swift registers ObjC classes by their *mangled* name
    // (`_TtC<len(module)><module><len(class)><class>`); the plain Swift name
    // isn't visible to NSClassFromString/AnyClass::get.
    AnyClass::get(c"_TtC18TestpatternSurface13MpvOpenGLView")
        .ok_or_else(|| "TestpatternSurface.MpvOpenGLView not linked (swift build?)".to_string())
}

fn init_mpv() {
    // Create the render context; registering the callback fires it right
    // away, so do that afterwards with no borrow held.
    let ctx = SURFACE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(surface) = s.as_mut() else {
            return ptr::null_mut();
        };
        // GL calls mpv makes while probing the context need a current CGL
        // context on this thread.
        // SAFETY: NSOpenGLView's `openGLContext` returns an object pointer
        // (possibly nil, checked) owned by the view, which `surface` keeps
        // alive; main thread.
        let gl_ctx: *mut AnyObject = unsafe { msg_send![&*surface.gl_view, openGLContext] };
        if gl_ctx.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `gl_ctx` is a non-nil NSOpenGLContext (above);
        // `makeCurrentContext` takes no arguments and returns void.
        let _: () = unsafe { msg_send![gl_ctx, makeCurrentContext] };

        let mut init = mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut advanced: c_int = 1;
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: &mut advanced as *mut c_int as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let mut ctx = ptr::null_mut();
        // SAFETY: the mpv handle is alive (`surface.mpv` holds an Arc), the
        // view's CGL context is current (above), and `params` is
        // INVALID-terminated with every `data` pointing at a local or a
        // static that outlives the call; mpv copies what it keeps.
        let rc =
            unsafe { mpv_render_context_create(&mut ctx, surface.mpv.raw(), params.as_mut_ptr()) };
        if rc < 0 {
            // SAFETY: mpv_error_string returns a static, NUL-terminated
            // string for any code.
            let msg = unsafe { CStr::from_ptr(mpv_error_string(rc)) };
            log::error!("mpv_render_context_create failed: {msg:?}");
            return ptr::null_mut();
        }
        surface.ctx = ctx;
        ctx
    });
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` was just created on this (the main) thread and is only
    // freed in `teardown`, also on the main thread. The callback ignores its
    // argument, so a null context pointer is fine.
    unsafe { mpv_render_context_set_update_callback(ctx, Some(on_mpv_update), ptr::null_mut()) };
    // Hand Swift the frame renderer; `drawRect:` calls it with the CGL
    // context already locked and current.
    SURFACE.with(|s| {
        if let Some(surface) = s.borrow().as_ref() {
            set_swift_render_callback(&surface.gl_view, render_frame);
        }
    });
    log::info!("mpv render context ready (macOS CGL OpenGL)");
}

/// Window-close teardown (the macOS mirror of Linux's `on_unrealize`):
/// stop update callbacks, free the mpv render context, then take the surface
/// out of `SURFACE` so the `Arc<Mpv>` it holds can be released. Runs on the
/// main thread via the window event handler.
fn teardown() {
    let surface = SURFACE.with(|s| s.borrow_mut().take());
    if let Some(mut surface) = surface {
        if !surface.ctx.is_null() {
            // SAFETY: a live context created on this (the main) thread;
            // the callback is cleared first so mpv stops calling into us,
            // and the pointer is nulled right after so it is freed once.
            unsafe {
                mpv_render_context_set_update_callback(surface.ctx, None, ptr::null_mut());
                mpv_render_context_free(surface.ctx);
            }
            surface.ctx = ptr::null_mut();
        }
        log::info!("mpv render context freed (macOS window closing)");
        // `surface` drops here, releasing the GL view and Arc<Mpv>.
    }
}

/// Invoked by `MpvOpenGLView.draw(_:)` with the view's CGL context current
/// and locked. Draws a fresh frame into the default framebuffer, reports the
/// swap; AppKit's `flushBuffer` (in Swift) presents.
extern "C" fn render_frame(_view: *mut c_void) {
    SURFACE.with(|s| {
        if let Some(surface) = s.borrow().as_ref()
            && !surface.ctx.is_null()
        {
            render(surface.ctx, &surface.gl_view);
        }
    });
}

fn render(ctx: *mut mpv_render_context, gl_view: &NSOpenGLView) {
    let bounds = gl_view.bounds();
    let backing = gl_view.convertRectToBacking(bounds);
    let mut fbo: c_int = 0;
    // SAFETY: `drawRect:` (Swift) made the view's CGL context current;
    // GL_FRAMEBUFFER_BINDING writes one GLint into `fbo`, and
    // `get_proc_address` ignores its context argument.
    unsafe { gl_get_integerv(get_proc_address, GL_FRAMEBUFFER_BINDING, &mut fbo) };
    let mut target = mpv_opengl_fbo {
        fbo,
        w: backing.size.width as c_int,
        h: backing.size.height as c_int,
        internal_format: 0,
    };
    let mut flip_y: c_int = 1;
    let mut params = [
        mpv_render_param {
            type_: MPV_RENDER_PARAM_OPENGL_FBO,
            data: &mut target as *mut _ as *mut c_void,
        },
        mpv_render_param {
            type_: MPV_RENDER_PARAM_FLIP_Y,
            data: &mut flip_y as *mut _ as *mut c_void,
        },
        mpv_render_param {
            type_: MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    // SAFETY: `ctx` is non-null, so live (only `teardown` frees it, on this
    // thread); the CGL context is current and locked (`drawRect:`);
    // `params` is INVALID-terminated and points at locals that outlive the
    // call.
    unsafe {
        mpv_render_context_render(ctx, params.as_mut_ptr());
        // drawRect: (Swift) flushes; report the swap straight away.
        mpv_render_context_report_swap(ctx);
    }
}

/// Called by mpv from arbitrary threads whenever a new frame is due.
unsafe extern "C" fn on_mpv_update(_: *mut c_void) {
    if !REDRAW.begin() {
        return; // a redraw is already queued on the main queue
    }
    let block = RcBlock::new(|| {
        REDRAW.end();
        SURFACE.with(|s| {
            let Ok(s) = s.try_borrow() else { return };
            let Some(surface) = s.as_ref() else { return };
            if surface.ctx.is_null() {
                return;
            }
            // SAFETY: on the main (render) thread, outside mpv's update
            // callback; non-null means not yet freed by `teardown`.
            let flags = unsafe { mpv_render_context_update(surface.ctx) };
            if flags & MPV_RENDER_UPDATE_FRAME != 0 {
                surface.gl_view.setNeedsDisplay(true);
            }
        });
    });
    // Never render on mpv's thread; hop onto AppKit's display cycle.
    // SAFETY: the main queue is a valid, never-released dispatch queue;
    // `into_raw` hands the block's +1 reference to libdispatch, and
    // `trampoline` takes it back exactly once when the work item runs.
    unsafe {
        dispatch_async_f(
            main_queue(),
            RcBlock::into_raw(block) as *mut c_void,
            trampoline,
        )
    };
}

/// # Safety
///
/// `ctx` must come from `RcBlock::into_raw` of an `RcBlock<dyn Fn()>`, and
/// be passed here once.
unsafe extern "C" fn trampoline(ctx: *mut c_void) {
    // SAFETY: `on_mpv_update` passes the block it leaked with `into_raw`;
    // libdispatch runs each work item once, so the reference is reclaimed
    // (and released on drop) once.
    let block = unsafe { RcBlock::<dyn Fn()>::from_raw(ctx.cast()) };
    if let Some(b) = block {
        b.call(());
    }
}

// ------------------------------------------------------------ dispatch

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn dispatch_async_f(
        queue: *mut c_void,
        ctx: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    static _dispatch_main_q: c_void;
}

fn main_queue() -> *mut c_void {
    // SAFETY: only takes the address of libdispatch's static main-queue
    // object (what `dispatch_get_main_queue()` expands to); nothing is read.
    unsafe { &_dispatch_main_q as *const c_void as *mut c_void }
}

// ------------------------------------------------------------ Swift glue

/// `+[MpvOpenGLView setRenderCallback:]`; installed once at init.
fn set_swift_render_callback(view: &NSOpenGLView, cb: extern "C" fn(*mut c_void)) {
    let cls = view.class();
    // SAFETY: `+setRenderCallback:` takes a `@convention(c)` function pointer
    // (`RenderFn`), which `cb` is (`extern "C" fn(*mut c_void)`, a `'static`
    // fn item); main thread.
    unsafe {
        let _: () = msg_send![cls, setRenderCallback: cb as *const c_void];
    }
}

// ------------------------------------------------------------ GL loading
//
// macOS's OpenGL ABI is stable (3.2 Core / 4.1); CGL has no public
// GetProcAddress, so mpv's entry points come from the framework via dlsym.

/// mpv's GL loader callback (also used by `gl_get_integerv`).
///
/// # Safety
///
/// `name` must be a valid NUL-terminated string.
unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // OpenGL.framework never gets unloaded; dlopen it once and look symbols
    // up from the cached handle instead of a per-call dlopen + mem::forget.
    static LIB: OnceLock<Option<libloading::os::unix::Library>> = OnceLock::new();
    let Some(lib) = LIB
        .get_or_init(|| {
            // SAFETY: OpenGL.framework is a system framework with no
            // initializer preconditions; it stays loaded for the process (a
            // static), so looked-up symbols never dangle.
            unsafe {
                libloading::os::unix::Library::new(
                    "/System/Library/Frameworks/OpenGL.framework/OpenGL",
                )
                .ok()
            }
        })
        .as_ref()
    else {
        return ptr::null_mut();
    };
    // SAFETY: `name` is a C string (caller contract).
    let name = unsafe { CStr::from_ptr(name).to_bytes() };
    // SAFETY: the symbol is read as an untyped address and handed back to
    // mpv, which knows its real prototype.
    let Ok(sym) = (unsafe { lib.get::<*mut c_void>(name) }) else {
        return ptr::null_mut();
    };
    *sym
}
