//! Pieces the Linux and macOS video surfaces share: redraw coalescing for
//! mpv's update callback and the one GL call the platform code makes itself.

use std::os::raw::c_int;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use super::mpv_sys::GetProcAddressFn;

pub const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;

/// Coalesces mpv's update callbacks: mpv may call back many times before the
/// UI thread gets to the redraw, which only needs to run once.
pub struct RedrawFlag(AtomicBool);

impl RedrawFlag {
    pub const fn new() -> Self {
        RedrawFlag(AtomicBool::new(false))
    }

    /// From mpv's update callback: true if the caller should queue a redraw,
    /// false if one is already queued.
    pub fn begin(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    /// From the queued redraw, before it asks mpv for the frame, so a
    /// callback arriving meanwhile queues the next one.
    pub fn end(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// `glGetIntegerv`, resolved once through the platform's `get_proc_address`
/// (the address is cached process-wide, so every call must pass the same
/// loader). Does nothing if the loader cannot find it.
///
/// # Safety
///
/// A GL context must be current on the calling thread, `out` must be valid
/// for writing as many `GLint`s as `pname` returns (one for the scalar
/// queries used here), and `get_proc_address` must accept a null context
/// and resolve GL entry points by NUL-terminated name.
pub unsafe fn gl_get_integerv(get_proc_address: GetProcAddressFn, pname: u32, out: *mut c_int) {
    type GetIntegerv = unsafe extern "C" fn(u32, *mut c_int);
    static FN: OnceLock<usize> = OnceLock::new();
    let addr = *FN.get_or_init(|| {
        // SAFETY: the caller guarantees `get_proc_address` takes a null
        // context; the name is a static C string.
        unsafe { get_proc_address(ptr::null_mut(), c"glGetIntegerv".as_ptr()) as usize }
    });
    if addr != 0 {
        // SAFETY: a non-null address the GL loader returned for
        // `glGetIntegerv`, whose C prototype is `void (GLenum, GLint *)`.
        let f: GetIntegerv = unsafe { std::mem::transmute(addr) };
        // SAFETY: the caller guarantees a current context and a writable
        // `out` of the size `pname` needs.
        unsafe { f(pname, out) };
    }
}
