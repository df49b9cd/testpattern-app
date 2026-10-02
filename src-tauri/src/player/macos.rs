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
use std::sync::atomic::{AtomicBool, Ordering};

// The Swift companion's static library is mostly ObjC metadata that nothing
// else references; force it in so dyld registers the class.
#[link(name = "TestpatternSurface", kind = "static")]
#[allow(dead_code)]
unsafe extern "C" { fn tp_surface_link_hack() -> c_int; }
use std::sync::{Arc, OnceLock};

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_app_kit::{NSOpenGLPixelFormat, NSOpenGLView, NSWindowOrderingMode};
use objc2_foundation::NSString;
use tauri::{Runtime, WebviewWindow};

use super::mpv::Mpv;
use super::mpv_sys::*;

thread_local! {
    static SURFACE: RefCell<Option<Surface>> = const { RefCell::new(None) };
}

struct Surface {
    gl_view: Retained<NSOpenGLView>,
    mpv: Arc<Mpv>,
    ctx: *mut mpv_render_context,
}

static REDRAW_PENDING: AtomicBool = AtomicBool::new(false);

/// Installs the video surface under the window's webview. Must be called
/// from the setup hook; `with_webview` runs on the main thread.
pub fn attach<R: Runtime>(window: &WebviewWindow<R>, mpv: Arc<Mpv>) -> tauri::Result<()> {
    window.with_webview(move |platform| {
        // wry's `inner()` is a `*mut WKWebView`.
        let webview: &objc2_web_kit::WKWebView = unsafe { &*(platform.inner() as *const _) };
        if let Err(e) = install(webview, mpv) {
            log::error!("video surface: {e}");
        }
    })
}

fn install(webview: &objc2_web_kit::WKWebView, mpv: Arc<Mpv>) -> Result<(), String> {
    let parent = unsafe { webview.superview() }.ok_or("WKWebView has no superview")?;

    // CGL/OpenGL view sized to the window, inserted *below* the webview.
    // Pixel format made on the Swift side — the objc2 binding of
    // initWithAttributes: goes through a calling convention that delivers nil.
    let frame = webview.frame();
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
    unsafe {
        let _: () = msg_send![&*gl_view, setWantsBestResolutionOpenGLSurface: true];
    }
    parent.addSubview_positioned_relativeTo(&gl_view, NSWindowOrderingMode::Below, Some(webview));

    // Let the GL view show through wherever the UI doesn't paint: the webview
    // stops drawing its background, the window below it goes clear.
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
        *s.borrow_mut() = Some(Surface { gl_view, mpv, ctx: ptr::null_mut() });
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
        let Some(surface) = s.as_mut() else { return ptr::null_mut() };
        // GL calls mpv makes while probing the context need a current CGL
        // context on this thread.
        let gl_ctx: *mut AnyObject = unsafe { msg_send![&*surface.gl_view, openGLContext] };
        if gl_ctx.is_null() {
            return ptr::null_mut();
        }
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
            mpv_render_param { type_: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        let mut ctx = ptr::null_mut();
        let rc = unsafe { mpv_render_context_create(&mut ctx, surface.mpv.raw(), params.as_mut_ptr()) };
        if rc < 0 {
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
    unsafe { gl_get_integerv(GL_FRAMEBUFFER_BINDING, &mut fbo) };
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
        mpv_render_param { type_: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
    ];
    unsafe {
        mpv_render_context_render(ctx, params.as_mut_ptr());
        // drawRect: (Swift) flushes; report the swap straight away.
        mpv_render_context_report_swap(ctx);
    }
}

/// Called by mpv from arbitrary threads whenever a new frame is due.
unsafe extern "C" fn on_mpv_update(_: *mut c_void) {
    if REDRAW_PENDING.swap(true, Ordering::AcqRel) {
        return; // a redraw is already queued on the main queue
    }
    let block = RcBlock::new(|| {
        REDRAW_PENDING.store(false, Ordering::Release);
        SURFACE.with(|s| {
            let Ok(s) = s.try_borrow() else { return };
            let Some(surface) = s.as_ref() else { return };
            if surface.ctx.is_null() {
                return;
            }
            let flags = unsafe { mpv_render_context_update(surface.ctx) };
            if flags & MPV_RENDER_UPDATE_FRAME != 0 {
                surface.gl_view.setNeedsDisplay(true);
            }
        });
    });
    // Never render on mpv's thread; hop onto AppKit's display cycle.
    unsafe { dispatch_async_f(main_queue(), RcBlock::into_raw(block) as *mut c_void, trampoline) };
}

unsafe extern "C" fn trampoline(ctx: *mut c_void) {
    let block = unsafe { RcBlock::<dyn Fn()>::from_raw(ctx.cast()) };
    if let Some(b) = block {
        b.call(());
    }
}

// ------------------------------------------------------------ dispatch

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn dispatch_async_f(queue: *mut c_void, ctx: *mut c_void, work: unsafe extern "C" fn(*mut c_void));
    static _dispatch_main_q: c_void;
}

fn main_queue() -> *mut c_void {
    unsafe { &_dispatch_main_q as *const c_void as *mut c_void }
}

// ------------------------------------------------------------ Swift glue

/// `+[MpvOpenGLView setRenderCallback:]`; installed once at init.
fn set_swift_render_callback(view: &NSOpenGLView, cb: extern "C" fn(*mut c_void)) {
    let cls = view.class();
    unsafe {
        let _: () = msg_send![cls, setRenderCallback: cb as *const c_void];
    }
}

// ------------------------------------------------------------ GL loading
//
// macOS's OpenGL ABI is stable (3.2 Core / 4.1); CGL has no public
// GetProcAddress, so mpv's entry points come from the framework via dlsym.

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;

unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let name = unsafe { CStr::from_ptr(name).to_bytes() };
    let lib = unsafe { libloading::os::unix::Library::new("/System/Library/Frameworks/OpenGL.framework/OpenGL") };
    let Ok(lib) = lib else {
        return ptr::null_mut();
    };
    let Ok(sym) = (unsafe { lib.get::<*mut c_void>(name) }) else {
        return ptr::null_mut();
    };
    let p = *sym;
    std::mem::forget(lib); // keep OpenGL.framework loaded
    p
}

unsafe fn gl_get_integerv(pname: u32, out: *mut c_int) {
    type GetIntegerv = unsafe extern "C" fn(u32, *mut c_int);
    static FN: OnceLock<usize> = OnceLock::new();
    let addr = *FN.get_or_init(|| unsafe { get_proc_address(ptr::null_mut(), c"glGetIntegerv".as_ptr()) as usize });
    if addr != 0 {
        let f: GetIntegerv = unsafe { std::mem::transmute(addr) };
        unsafe { f(pname, out) };
    }
}
