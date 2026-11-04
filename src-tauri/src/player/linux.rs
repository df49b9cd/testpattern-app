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
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::ptr;
use std::sync::{Arc, OnceLock};

use gtk::prelude::*;
use tauri::{Runtime, WebviewWindow};
use webkit2gtk::WebViewExt;

use super::mpv::Mpv;
use super::mpv_sys::{
    MPV_RENDER_API_TYPE_OPENGL, MPV_RENDER_PARAM_API_TYPE, MPV_RENDER_PARAM_DRM_DISPLAY_V2,
    MPV_RENDER_PARAM_FLIP_Y, MPV_RENDER_PARAM_INVALID, MPV_RENDER_PARAM_OPENGL_FBO,
    MPV_RENDER_PARAM_OPENGL_INIT_PARAMS, MPV_RENDER_UPDATE_FRAME, mpv_error_string,
    mpv_opengl_drm_params_v2, mpv_opengl_fbo, mpv_opengl_init_params, mpv_render_context,
    mpv_render_context_create, mpv_render_context_free, mpv_render_context_render,
    mpv_render_context_report_swap, mpv_render_context_set_update_callback,
    mpv_render_context_update, mpv_render_param,
};
use super::render::{GL_FRAMEBUFFER_BINDING, RedrawFlag, gl_get_integerv};

thread_local! {
    static SURFACE: RefCell<Option<Surface>> = const { RefCell::new(None) };
}

struct Surface {
    area: gtk::GLArea,
    mpv: Arc<Mpv>,
    ctx: *mut mpv_render_context,
    /// The GPU's render node, for VA-API zero-copy; open as long as `ctx`.
    render_node: Option<File>,
}

static REDRAW: RedrawFlag = RedrawFlag::new();

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
        *s.borrow_mut() = Some(Surface {
            area: area.clone(),
            mpv,
            ctx: ptr::null_mut(),
            render_node: None,
        });
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
        // With the GPU's render node mpv hands VA-API frames to GL as dmabufs
        // (hwdec "vaapi", zero-copy). That import needs EGL — GLX sessions,
        // and systems without a node, get "vaapi-copy" (frames copied back
        // through memory) or software decoding instead.
        let render_node = if using_egl() {
            open_render_node()
        } else {
            None
        };
        let mut drm = mpv_opengl_drm_params_v2 {
            fd: -1,
            crtc_id: 0,
            connector_id: 0,
            atomic_request_ptr: ptr::null_mut(),
            render_fd: render_node.as_ref().map_or(-1, |f| f.as_raw_fd()),
        };
        let mut params = vec![
            mpv_render_param {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
        ];
        if render_node.is_some() {
            params.push(mpv_render_param {
                type_: MPV_RENDER_PARAM_DRM_DISPLAY_V2,
                data: &mut drm as *mut _ as *mut c_void,
            });
        }
        params.push(mpv_render_param {
            type_: MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        });
        let mut ctx = ptr::null_mut();
        // SAFETY: the mpv handle is alive (`surface.mpv` holds an Arc), the
        // GLArea's context was made current above, and `params` is
        // INVALID-terminated with every `data` pointing at a local
        // (`init`, `drm`) or a static (the API name) that outlives the call;
        // mpv copies what it keeps. `render_node` stays open in `surface`
        // for as long as the context exists.
        let rc =
            unsafe { mpv_render_context_create(&mut ctx, surface.mpv.raw(), params.as_mut_ptr()) };
        if rc < 0 {
            // SAFETY: mpv_error_string returns a static, NUL-terminated
            // string for any code.
            let msg = unsafe { CStr::from_ptr(mpv_error_string(rc)) };
            log::error!("mpv_render_context_create failed: {msg:?}");
            return;
        }
        surface.ctx = ctx;
        surface.render_node = render_node;
    });
    // Registering fires the callback right away, so do it with no borrow held.
    let ctx = SURFACE.with(|s| s.borrow().as_ref().map_or(ptr::null_mut(), |s| s.ctx));
    if !ctx.is_null() {
        // SAFETY: `ctx` was just created on this thread and is only freed in
        // `on_unrealize`, on this thread too. The callback ignores its
        // argument, so a null context pointer is fine.
        unsafe {
            mpv_render_context_set_update_callback(ctx, Some(on_mpv_update), ptr::null_mut())
        };
        log::info!("mpv render context ready ({} GL)", gl_flavor());
    }
}

fn on_unrealize(area: &gtk::GLArea) {
    area.make_current();
    SURFACE.with(|s| {
        if let Some(surface) = s.borrow_mut().as_mut()
            && !surface.ctx.is_null()
        {
            // SAFETY: a live context created on this (the GTK) thread, with
            // its GL context made current above; nulled right after, so it
            // is freed once.
            unsafe { mpv_render_context_free(surface.ctx) };
            surface.ctx = ptr::null_mut();
            surface.render_node = None; // mpv's VA display is gone with the context
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
        let (w, h) = (
            area.allocated_width() * scale,
            area.allocated_height() * scale,
        );
        let mut fbo: c_int = 0;
        // SAFETY: GTK makes the area's GL context current before emitting
        // `render`; GL_FRAMEBUFFER_BINDING writes one GLint into `fbo`, and
        // `get_proc_address` takes a null context.
        unsafe { gl_get_integerv(get_proc_address, GL_FRAMEBUFFER_BINDING, &mut fbo) };
        let mut target = mpv_opengl_fbo {
            fbo,
            w,
            h,
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
        // SAFETY: `surface.ctx` is non-null, so live (only `on_unrealize`
        // frees it, on this thread, and nulls it); the GL context is current
        // (GTK's `render` contract); `params` is INVALID-terminated and
        // points at locals that outlive the call. This is the render thread:
        // no other libmpv calls are made from it while rendering.
        unsafe {
            mpv_render_context_render(surface.ctx, params.as_mut_ptr());
            mpv_render_context_report_swap(surface.ctx);
        }
    });
    glib::Propagation::Stop
}

/// Called by mpv from arbitrary threads whenever a new frame is due.
unsafe extern "C" fn on_mpv_update(_: *mut c_void) {
    if !REDRAW.begin() {
        return; // a redraw is already queued on the GTK thread
    }
    // Always go through the main loop (never run inline), ahead of GTK's
    // redraw priority so the frame lands in the upcoming paint.
    glib::idle_add_full(glib::Priority::HIGH_IDLE, || {
        REDRAW.end();
        SURFACE.with(|s| {
            let Ok(s) = s.try_borrow() else { return };
            if let Some(surface) = s.as_ref() {
                if surface.ctx.is_null() {
                    return;
                }
                // SAFETY: on the GTK (render) thread, outside mpv's update
                // callback; non-null means not yet freed by `on_unrealize`.
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

struct GlLoader {
    egl_get_proc: Option<unsafe extern "C" fn(*const c_char) -> *mut c_void>,
    egl_current_ctx: Option<unsafe extern "C" fn() -> *mut c_void>,
    egl_current_display: Option<unsafe extern "C" fn() -> *mut c_void>,
    glx_get_proc: Option<unsafe extern "C" fn(*const u8) -> *mut c_void>,
    _libs: Vec<libloading::Library>,
}

fn loader() -> &'static GlLoader {
    static LOADER: OnceLock<GlLoader> = OnceLock::new();
    // SAFETY: libEGL/libGL are system libraries whose initializers have no
    // preconditions; each symbol is typed with its C prototype from the EGL
    // and GLX headers, and the libraries are kept loaded in `_libs` for the
    // process lifetime (the loader is a static), so the copied function
    // pointers never dangle.
    LOADER.get_or_init(|| unsafe {
        let mut libs = Vec::new();
        let mut l = GlLoader {
            egl_get_proc: None,
            egl_current_ctx: None,
            egl_current_display: None,
            glx_get_proc: None,
            _libs: vec![],
        };
        if let Ok(egl) = libloading::Library::new("libEGL.so.1") {
            l.egl_get_proc = egl
                .get(b"eglGetProcAddress\0")
                .ok()
                .map(|s: libloading::Symbol<_>| *s);
            l.egl_current_ctx = egl
                .get(b"eglGetCurrentContext\0")
                .ok()
                .map(|s: libloading::Symbol<_>| *s);
            l.egl_current_display = egl
                .get(b"eglGetCurrentDisplay\0")
                .ok()
                .map(|s: libloading::Symbol<_>| *s);
            libs.push(egl);
        }
        if let Ok(glx) = libloading::Library::new("libGL.so.1") {
            l.glx_get_proc = glx
                .get(b"glXGetProcAddressARB\0")
                .ok()
                .map(|s: libloading::Symbol<_>| *s);
            libs.push(glx);
        }
        l._libs = libs;
        l
    })
}

fn using_egl() -> bool {
    let l = loader();
    match l.egl_current_ctx {
        // SAFETY: eglGetCurrentContext takes no arguments and only reads
        // thread-local EGL state.
        Some(f) => !unsafe { f() }.is_null(),
        None => false,
    }
}

fn gl_flavor() -> &'static str {
    if using_egl() { "EGL" } else { "GLX" }
}

/// mpv's GL loader callback (also used by `gl_get_integerv`).
///
/// # Safety
///
/// `name` must be a valid NUL-terminated string.
unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let l = loader();
    // SAFETY: `name` is a C string (caller contract, mpv passes GL function
    // names); eglGetProcAddress/glXGetProcAddressARB only look it up.
    unsafe {
        if using_egl()
            && let Some(f) = l.egl_get_proc
        {
            return f(name);
        }
        if let Some(f) = l.glx_get_proc {
            return f(name as *const u8);
        }
    }
    ptr::null_mut()
}

/// The render node of the GPU behind the current EGL display
/// (EGL_EXT_device_drm_render_node), else the first one in /dev/dri.
fn open_render_node() -> Option<File> {
    let (path, how) = egl_render_node().map(|p| (p, "EGL device")).or_else(|| {
        let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("renderD"))
            })
            .collect();
        nodes.sort();
        nodes.into_iter().next().map(|p| (p, "first render node"))
    })?;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => {
            log::info!("VA-API zero-copy via {} ({how})", path.display());
            Some(f)
        }
        Err(e) => {
            log::warn!("{}: {e}", path.display());
            None
        }
    }
}

fn egl_render_node() -> Option<PathBuf> {
    const EGL_DEVICE_EXT: i32 = 0x322C;
    const EGL_DRM_RENDER_NODE_FILE_EXT: i32 = 0x3377;
    type QueryDisplayAttrib = unsafe extern "C" fn(*mut c_void, i32, *mut isize) -> u32;
    type QueryDeviceString = unsafe extern "C" fn(*mut c_void, i32) -> *const c_char;
    let l = loader();
    let (current_display, get_proc) = (l.egl_current_display?, l.egl_get_proc?);
    // SAFETY: called with the GLArea's EGL context current (from
    // `on_realize`). The two extension functions are looked up by name and
    // checked for null before being transmuted to their prototypes from
    // EGL_EXT_device_query / EGL_EXT_device_drm_render_node; `device` is
    // checked before use; the returned string is owned by EGL, NUL-terminated
    // and copied before returning.
    unsafe {
        let display = current_display();
        let query_attrib = get_proc(c"eglQueryDisplayAttribEXT".as_ptr());
        let query_string = get_proc(c"eglQueryDeviceStringEXT".as_ptr());
        if display.is_null() || query_attrib.is_null() || query_string.is_null() {
            return None;
        }
        let query_attrib: QueryDisplayAttrib = std::mem::transmute(query_attrib);
        let query_string: QueryDeviceString = std::mem::transmute(query_string);
        let mut device: isize = 0;
        if query_attrib(display, EGL_DEVICE_EXT, &mut device) == 0 || device == 0 {
            return None;
        }
        let node = query_string(device as *mut c_void, EGL_DRM_RENDER_NODE_FILE_EXT);
        if node.is_null() {
            return None;
        }
        Some(PathBuf::from(CStr::from_ptr(node).to_str().ok()?))
    }
}
