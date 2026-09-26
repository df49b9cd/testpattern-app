//! Linux video surface: a GtkGLArea rendered by libmpv, stacked *under* the
//! (transparent) WebKitGTK webview inside a GtkOverlay.
//!
//!   GtkWindow
//!    └─ GtkOverlay               (replaces tao's vbox as the window child)
//!        ├─ GtkGLArea            ← mpv render API (OpenGL)
//!        └─ WebKitWebView        ← React UI, transparent where video shows
//!
//! The webview's grandparent must stay the GtkWindow: tauri-runtime-wry's
//! button-press handler does `webview.parent().parent()` → `gtk::Window` and
//! unwraps, so any deeper nesting crashes on the first click.
//!
//! The web UI decides what is visible: opaque pages hide the video, the
//! player page is transparent and draws its controls on top of it.

use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use gtk::prelude::*;
use tauri::{Runtime, WebviewWindow};
use webkit2gtk::WebViewExt;

use super::mpv::Mpv;
use super::mpv_sys::*;

thread_local! {
    static SURFACE: RefCell<Option<Surface>> = const { RefCell::new(None) };
}

struct Surface {
    area: gtk::GLArea,
    mpv: Arc<Mpv>,
    ctx: *mut mpv_render_context,
}

static REDRAW_PENDING: AtomicBool = AtomicBool::new(false);

/// Installs the video surface under the window's webview. Must be called
/// from the setup hook; the actual widget surgery runs on the GTK thread.
pub fn attach<R: Runtime>(window: &WebviewWindow<R>, mpv: Arc<Mpv>) -> tauri::Result<()> {
    window.with_webview(move |platform| {
        let webview: webkit2gtk::WebView = platform.inner();
        if let Err(e) = install(&webview, mpv) {
            log::error!("video surface: {e}");
        }
    })
}

fn install(webview: &webkit2gtk::WebView, mpv: Arc<Mpv>) -> Result<(), String> {
    let vbox = webview
        .parent()
        .and_then(|p| p.downcast::<gtk::Box>().ok())
        .ok_or("webview is not inside a GtkBox")?;
    let window = vbox
        .parent()
        .and_then(|p| p.downcast::<gtk::Window>().ok())
        .ok_or("webview box is not a window child")?;

    let area = gtk::GLArea::new();
    area.set_auto_render(false);
    area.set_has_alpha(false);
    area.set_hexpand(true);
    area.set_vexpand(true);

    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    overlay.set_vexpand(true);

    // Reparent the webview into the overlay (keep a strong ref while moving)
    // and put the overlay directly into the window. tao keeps its own
    // reference to the vbox, so detaching it is safe.
    let wv = webview.clone();
    vbox.remove(&wv);
    window.remove(&vbox);
    overlay.add(&area);
    overlay.add_overlay(&wv);
    window.add(&overlay);

    // Transparent page background so the GLArea shows through wherever the
    // UI does not paint.
    wv.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));

    area.connect_realize(on_realize);
    area.connect_unrealize(on_unrealize);
    area.connect_render(on_render);
    area.connect_resize(|area, _, _| area.queue_render());

    SURFACE.with(|s| {
        *s.borrow_mut() = Some(Surface { area: area.clone(), mpv, ctx: ptr::null_mut() });
    });

    overlay.show_all();
    wv.grab_focus();
    Ok(())
}

fn on_realize(area: &gtk::GLArea) {
    area.make_current();
    if let Some(err) = area.error() {
        log::error!("GLArea realize failed: {err}");
        return;
    }
    SURFACE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(surface) = s.as_mut() else { return };
        if !surface.ctx.is_null() {
            return;
        }
        let mut init = mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            mpv_render_param { type_: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        let mut ctx = ptr::null_mut();
        let rc = unsafe {
            mpv_render_context_create(&mut ctx, surface.mpv.raw(), params.as_mut_ptr())
        };
        if rc < 0 {
            let msg = unsafe { CStr::from_ptr(mpv_error_string(rc)) };
            log::error!("mpv_render_context_create failed: {msg:?}");
            return;
        }
        surface.ctx = ctx;
    });
    // Registering fires the callback right away, so do it with no borrow held.
    let ctx = SURFACE.with(|s| s.borrow().as_ref().map_or(ptr::null_mut(), |s| s.ctx));
    if !ctx.is_null() {
        unsafe { mpv_render_context_set_update_callback(ctx, Some(on_mpv_update), ptr::null_mut()) };
        log::info!("mpv render context ready ({} GL)", gl_flavor());
    }
}

fn on_unrealize(area: &gtk::GLArea) {
    area.make_current();
    SURFACE.with(|s| {
        if let Some(surface) = s.borrow_mut().as_mut()
            && !surface.ctx.is_null() {
                unsafe { mpv_render_context_free(surface.ctx) };
                surface.ctx = ptr::null_mut();
            }
    });
}

fn on_render(area: &gtk::GLArea, _ctx: &gdk::GLContext) -> glib::Propagation {
    SURFACE.with(|s| {
        let s = s.borrow();
        let Some(surface) = s.as_ref() else { return };
        if surface.ctx.is_null() {
            return;
        }
        let scale = area.scale_factor();
        let (w, h) = (area.allocated_width() * scale, area.allocated_height() * scale);
        let mut fbo: c_int = 0;
        unsafe { gl_get_integerv(GL_FRAMEBUFFER_BINDING, &mut fbo) };
        let mut target = mpv_opengl_fbo { fbo, w, h, internal_format: 0 };
        let mut flip_y: c_int = 1;
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut target as *mut _ as *mut c_void,
            },
            mpv_render_param { type_: MPV_RENDER_PARAM_FLIP_Y, data: &mut flip_y as *mut _ as *mut c_void },
            mpv_render_param { type_: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        unsafe {
            mpv_render_context_render(surface.ctx, params.as_mut_ptr());
            mpv_render_context_report_swap(surface.ctx);
        }
    });
    glib::Propagation::Stop
}

/// Called by mpv from arbitrary threads whenever a new frame is due.
unsafe extern "C" fn on_mpv_update(_: *mut c_void) {
    if REDRAW_PENDING.swap(true, Ordering::AcqRel) {
        return; // a redraw is already queued on the GTK thread
    }
    // Always go through the main loop (never run inline), ahead of GTK's
    // redraw priority so the frame lands in the upcoming paint.
    glib::idle_add_full(glib::Priority::HIGH_IDLE, || {
        REDRAW_PENDING.store(false, Ordering::Release);
        SURFACE.with(|s| {
            let Ok(s) = s.try_borrow() else { return };
            if let Some(surface) = s.as_ref() {
                if surface.ctx.is_null() {
                    return;
                }
                let flags = unsafe { mpv_render_context_update(surface.ctx) };
                if flags & MPV_RENDER_UPDATE_FRAME != 0 {
                    surface.area.queue_render();
                }
            }
        });
        glib::ControlFlow::Break
    });
}

// ------------------------------------------------------------ GL loading
//
// GTK3 uses EGL on Wayland and GLX on X11; resolve GL entry points through
// whichever one owns the current context.

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;

struct GlLoader {
    egl_get_proc: Option<unsafe extern "C" fn(*const c_char) -> *mut c_void>,
    egl_current_ctx: Option<unsafe extern "C" fn() -> *mut c_void>,
    glx_get_proc: Option<unsafe extern "C" fn(*const u8) -> *mut c_void>,
    _libs: Vec<libloading::Library>,
}

fn loader() -> &'static GlLoader {
    static LOADER: OnceLock<GlLoader> = OnceLock::new();
    LOADER.get_or_init(|| unsafe {
        let mut libs = Vec::new();
        let mut l = GlLoader { egl_get_proc: None, egl_current_ctx: None, glx_get_proc: None, _libs: vec![] };
        if let Ok(egl) = libloading::Library::new("libEGL.so.1") {
            l.egl_get_proc = egl.get(b"eglGetProcAddress\0").ok().map(|s: libloading::Symbol<_>| *s);
            l.egl_current_ctx = egl.get(b"eglGetCurrentContext\0").ok().map(|s: libloading::Symbol<_>| *s);
            libs.push(egl);
        }
        if let Ok(glx) = libloading::Library::new("libGL.so.1") {
            l.glx_get_proc = glx.get(b"glXGetProcAddressARB\0").ok().map(|s: libloading::Symbol<_>| *s);
            libs.push(glx);
        }
        l._libs = libs;
        l
    })
}

fn using_egl() -> bool {
    let l = loader();
    match l.egl_current_ctx {
        Some(f) => !unsafe { f() }.is_null(),
        None => false,
    }
}

fn gl_flavor() -> &'static str {
    if using_egl() { "EGL" } else { "GLX" }
}

unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let l = loader();
    unsafe {
        if using_egl()
            && let Some(f) = l.egl_get_proc {
                return f(name);
            }
        if let Some(f) = l.glx_get_proc {
            return f(name as *const u8);
        }
    }
    ptr::null_mut()
}

unsafe fn gl_get_integerv(pname: u32, out: *mut c_int) {
    type GetIntegerv = unsafe extern "C" fn(u32, *mut c_int);
    static FN: OnceLock<usize> = OnceLock::new();
    let addr = *FN.get_or_init(|| unsafe {
        get_proc_address(ptr::null_mut(), c"glGetIntegerv".as_ptr()) as usize
    });
    if addr != 0 {
        let f: GetIntegerv = unsafe { std::mem::transmute(addr) };
        unsafe { f(pname, out) };
    }
}
