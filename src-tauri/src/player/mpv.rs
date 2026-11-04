//! Safe(ish) wrapper around a libmpv handle. The client API is thread-safe,
//! so `Mpv` is shared between the event thread, Tauri commands and the GL
//! render callback.

use std::ffi::{CStr, CString, c_void};
use std::os::raw::{c_char, c_int};
use std::ptr;

use serde_json::{Map, Value};

use super::mpv_sys::{
    MPV_END_FILE_REASON_EOF, MPV_END_FILE_REASON_ERROR, MPV_END_FILE_REASON_QUIT,
    MPV_END_FILE_REASON_REDIRECT, MPV_END_FILE_REASON_STOP, MPV_EVENT_END_FILE,
    MPV_EVENT_FILE_LOADED, MPV_EVENT_IDLE, MPV_EVENT_LOG_MESSAGE, MPV_EVENT_NONE,
    MPV_EVENT_PLAYBACK_RESTART, MPV_EVENT_PROPERTY_CHANGE, MPV_EVENT_SEEK, MPV_EVENT_SHUTDOWN,
    MPV_EVENT_START_FILE, MPV_EVENT_VIDEO_RECONFIG, MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG,
    MPV_FORMAT_INT64, MPV_FORMAT_NODE, MPV_FORMAT_NODE_ARRAY, MPV_FORMAT_NODE_MAP,
    MPV_FORMAT_OSD_STRING, MPV_FORMAT_STRING, mpv_client_api_version, mpv_command,
    mpv_command_async, mpv_create, mpv_error_string, mpv_event_end_file, mpv_event_log_message,
    mpv_event_property, mpv_format, mpv_free_node_contents, mpv_get_property, mpv_handle,
    mpv_initialize, mpv_node, mpv_observe_property, mpv_request_log_messages,
    mpv_set_option_string, mpv_set_property, mpv_set_property_string, mpv_terminate_destroy,
    mpv_wait_event,
};

#[derive(Debug, thiserror::Error)]
#[error("mpv: {context}: {message}")]
pub struct MpvError {
    pub code: i32,
    pub context: String,
    pub message: String,
}

fn check(code: c_int, context: impl FnOnce() -> String) -> Result<(), MpvError> {
    if code >= 0 {
        return Ok(());
    }
    // SAFETY: mpv_error_string returns a static, NUL-terminated string for
    // any code ("unknown error" for unknown ones).
    let message = unsafe { CStr::from_ptr(mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned();
    Err(MpvError {
        code,
        context: context(),
        message,
    })
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("NUL bytes were removed")
}

pub struct Mpv {
    handle: *mut mpv_handle,
}

// SAFETY: the handle is an opaque pointer owned by this struct (destroyed
// once, in `Drop`), and the libmpv client API is documented as fully
// thread-safe ("Multithreading" in client.h), so it may move between threads.
unsafe impl Send for Mpv {}
// SAFETY: every `&self` method is a client API call, which libmpv allows
// concurrently on one handle. The one exception, `mpv_wait_event` (one
// caller at a time; its result is only valid until the next call), is
// confined to the single event thread of each `Mpv` (the player's
// `mpv-events` thread, the probe's loop); see `wait_event`.
unsafe impl Sync for Mpv {}

#[derive(Debug)]
pub enum Event {
    Shutdown,
    Log {
        prefix: String,
        level: String,
        text: String,
    },
    StartFile,
    EndFile {
        reason: EndReason,
        error: Option<String>,
    },
    FileLoaded,
    Idle,
    VideoReconfig,
    PlaybackRestart,
    Seek,
    PropertyChange {
        name: String,
        value: Value,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error,
    Redirect,
    Unknown,
}

impl Mpv {
    /// Creates and initializes an mpv instance with the given options.
    pub fn new(options: &[(&str, &str)]) -> Result<Self, MpvError> {
        // SAFETY: no preconditions; failure (out of memory, LC_NUMERIC not
        // "C") is a null return, handled below.
        let handle = unsafe { mpv_create() };
        if handle.is_null() {
            return Err(MpvError {
                code: -1,
                context: "mpv_create".into(),
                message: "out of memory".into(),
            });
        }
        let mpv = Mpv { handle };
        for (k, v) in options {
            let (ck, cv) = (cstr(k), cstr(v));
            // SAFETY: `handle` is valid and not yet shared; both C strings
            // outlive the call (mpv copies them).
            let rc = unsafe { mpv_set_option_string(handle, ck.as_ptr(), cv.as_ptr()) };
            if let Err(e) = check(rc, || format!("option {k}={v}")) {
                // Non-fatal: an unknown option should not prevent playback.
                log::warn!("{e}");
            }
        }
        // SAFETY: `handle` is valid and initialized only once, before it is
        // shared with other threads.
        check(unsafe { mpv_initialize(handle) }, || "initialize".into())?;
        Ok(mpv)
    }

    /// The raw handle; the Linux and macOS render-API surfaces use it to
    /// create their mpv render contexts.
    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    pub fn raw(&self) -> *mut mpv_handle {
        self.handle
    }

    pub fn api_version() -> (u32, u32) {
        // SAFETY: no arguments, no preconditions.
        let v = unsafe { mpv_client_api_version() } as u32;
        (v >> 16, v & 0xffff)
    }

    /// Calls `f` with a NULL-terminated `char **` of `args` that stays valid
    /// for the duration of the call.
    fn with_args<R>(args: &[&str], f: impl FnOnce(*mut *const c_char) -> R) -> R {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        f(ptrs.as_mut_ptr())
    }

    pub fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        // SAFETY: valid handle; `p` is a NULL-terminated argument array alive
        // for the call (`with_args`).
        let rc = Self::with_args(args, |p| unsafe { mpv_command(self.handle, p) });
        check(rc, || format!("command {args:?}"))
    }

    /// Fire-and-forget command; the result arrives as a COMMAND_REPLY event.
    pub fn command_async(&self, args: &[&str]) -> Result<(), MpvError> {
        // SAFETY: valid handle; `p` is a NULL-terminated argument array alive
        // for the call (`with_args`); mpv copies the arguments before it
        // returns.
        let rc = Self::with_args(args, |p| unsafe { mpv_command_async(self.handle, 0, p) });
        check(rc, || format!("command_async {args:?}"))
    }

    pub fn set_string(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let (n, v) = (cstr(name), cstr(value));
        // SAFETY: valid handle; both C strings outlive the call.
        let rc = unsafe { mpv_set_property_string(self.handle, n.as_ptr(), v.as_ptr()) };
        check(rc, || format!("set {name}={value}"))
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<(), MpvError> {
        let n = cstr(name);
        let mut v: c_int = value.into();
        // SAFETY: valid handle; `n` outlives the call and `data` points at
        // a live `c_int`, the C type MPV_FORMAT_FLAG reads (mpv copies the value).
        let rc = unsafe {
            mpv_set_property(
                self.handle,
                n.as_ptr(),
                MPV_FORMAT_FLAG,
                &mut v as *mut _ as *mut c_void,
            )
        };
        check(rc, || format!("set {name}={value}"))
    }

    pub fn set_double(&self, name: &str, value: f64) -> Result<(), MpvError> {
        let n = cstr(name);
        let mut v = value;
        // SAFETY: valid handle; `n` outlives the call and `data` points at
        // a live `f64`, the C type MPV_FORMAT_DOUBLE reads (mpv copies the value).
        let rc = unsafe {
            mpv_set_property(
                self.handle,
                n.as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut v as *mut _ as *mut c_void,
            )
        };
        check(rc, || format!("set {name}={value}"))
    }

    pub fn set_int(&self, name: &str, value: i64) -> Result<(), MpvError> {
        let n = cstr(name);
        let mut v = value;
        // SAFETY: valid handle; `n` outlives the call and `data` points at
        // a live `i64`, the C type MPV_FORMAT_INT64 reads (mpv copies the value).
        let rc = unsafe {
            mpv_set_property(
                self.handle,
                n.as_ptr(),
                MPV_FORMAT_INT64,
                &mut v as *mut _ as *mut c_void,
            )
        };
        check(rc, || format!("set {name}={value}"))
    }

    /// Sets a property from a JSON value, picking the matching mpv format.
    pub fn set_json(&self, name: &str, value: &Value) -> Result<(), MpvError> {
        match value {
            Value::Bool(b) => self.set_flag(name, *b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => self.set_int(name, i),
                None => self.set_double(name, n.as_f64().unwrap_or_default()),
            },
            Value::String(s) => self.set_string(name, s),
            Value::Null => self.set_string(name, "no"),
            other => self.set_string(name, &other.to_string()),
        }
    }

    /// Reads any property as JSON (via MPV_FORMAT_NODE).
    pub fn get_json(&self, name: &str) -> Option<Value> {
        let n = cstr(name);
        // SAFETY: mpv_node is a plain C struct (a union of scalars and raw
        // pointers plus an int); all-zero is MPV_FORMAT_NONE with null data.
        let mut node: mpv_node = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle; `n` outlives the call and `data` points at an
        // mpv_node, what MPV_FORMAT_NODE writes.
        let rc = unsafe {
            mpv_get_property(
                self.handle,
                n.as_ptr(),
                MPV_FORMAT_NODE,
                &mut node as *mut _ as *mut c_void,
            )
        };
        if rc < 0 {
            return None;
        }
        // SAFETY: on success mpv filled `node` with a well-formed tree it
        // owns until it is freed below.
        let v = unsafe { node_to_json(&node) };
        // SAFETY: the node came from a successful mpv_get_property and is
        // freed exactly once; `v` holds only owned copies.
        unsafe { mpv_free_node_contents(&mut node) };
        Some(v)
    }

    pub fn observe(&self, name: &str, format: mpv_format) -> Result<(), MpvError> {
        let n = cstr(name);
        // SAFETY: valid handle; `n` outlives the call (mpv copies the name).
        let rc = unsafe { mpv_observe_property(self.handle, 0, n.as_ptr(), format) };
        check(rc, || format!("observe {name}"))
    }

    pub fn request_log_messages(&self, level: &str) {
        let l = cstr(level);
        // SAFETY: valid handle; `l` outlives the call.
        unsafe { mpv_request_log_messages(self.handle, l.as_ptr()) };
    }

    /// Blocks up to `timeout` seconds (negative = forever) for the next event.
    ///
    /// libmpv allows only one thread at a time to wait on a handle: call
    /// this from a single event thread per `Mpv` (see the `Sync` impl).
    pub fn wait_event(&self, timeout: f64) -> Option<Event> {
        // SAFETY: valid handle; mpv_wait_event never returns null, and the
        // event (with everything it points to) stays valid until the next
        // mpv_wait_event on this handle, which only this function makes, on
        // the one event thread. Everything is copied out before returning.
        let ev = unsafe { &*mpv_wait_event(self.handle, timeout) };
        Some(match ev.event_id {
            MPV_EVENT_NONE => return None,
            MPV_EVENT_SHUTDOWN => Event::Shutdown,
            MPV_EVENT_LOG_MESSAGE => {
                // SAFETY: for MPV_EVENT_LOG_MESSAGE, `data` points to an
                // mpv_event_log_message valid as long as `ev`.
                let m = unsafe { &*(ev.data as *const mpv_event_log_message) };
                // SAFETY: the fields are NUL-terminated strings owned by
                // the event, valid as long as `ev`.
                let (prefix, level, text) =
                    unsafe { (lossy(m.prefix), lossy(m.level), lossy(m.text)) };
                Event::Log {
                    prefix,
                    level,
                    text: text.trim_end().to_owned(),
                }
            }
            MPV_EVENT_START_FILE => Event::StartFile,
            MPV_EVENT_END_FILE => {
                // SAFETY: for MPV_EVENT_END_FILE, `data` points to an
                // mpv_event_end_file valid as long as `ev`.
                let e = unsafe { &*(ev.data as *const mpv_event_end_file) };
                let reason = match e.reason {
                    MPV_END_FILE_REASON_EOF => EndReason::Eof,
                    MPV_END_FILE_REASON_STOP => EndReason::Stop,
                    MPV_END_FILE_REASON_QUIT => EndReason::Quit,
                    MPV_END_FILE_REASON_ERROR => EndReason::Error,
                    MPV_END_FILE_REASON_REDIRECT => EndReason::Redirect,
                    _ => EndReason::Unknown,
                };
                let error = (reason == EndReason::Error).then(|| {
                    // SAFETY: mpv_error_string returns a static,
                    // NUL-terminated string for any code.
                    unsafe { CStr::from_ptr(mpv_error_string(e.error)) }
                        .to_string_lossy()
                        .into_owned()
                });
                Event::EndFile { reason, error }
            }
            MPV_EVENT_FILE_LOADED => Event::FileLoaded,
            MPV_EVENT_IDLE => Event::Idle,
            MPV_EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
            MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
            MPV_EVENT_SEEK => Event::Seek,
            MPV_EVENT_PROPERTY_CHANGE => {
                // SAFETY: for MPV_EVENT_PROPERTY_CHANGE, `data` points to an
                // mpv_event_property valid as long as `ev`.
                let p = unsafe { &*(ev.data as *const mpv_event_property) };
                // SAFETY: `name` is a NUL-terminated string owned by the event.
                let name = unsafe { lossy(p.name) };
                // SAFETY: mpv guarantees `data` matches `format` (or is null
                // with MPV_FORMAT_NONE), valid as long as `ev`.
                let value = unsafe { format_to_json(p.format, p.data) };
                Event::PropertyChange { name, value }
            }
            _ => Event::Other,
        })
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and owned by this struct; `Drop` runs
        // once and no `&Mpv` outlives it, so no other call can race with or
        // follow the destroy.
        unsafe { mpv_terminate_destroy(self.handle) }
    }
}

/// Copies a C string; null reads as empty.
///
/// # Safety
///
/// `p` must be null or point to a NUL-terminated string valid for the call.
unsafe fn lossy(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: non-null, so a valid C string (caller contract).
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Converts an mpv value of `format` to JSON (unknown formats are null).
///
/// # Safety
///
/// `data` must be null or point to a live value of the C type `format`
/// denotes (`char *` for strings, `int` for flags, `int64_t`, `double`, or
/// an `mpv_node` tree), as mpv lays it out for property events.
unsafe fn format_to_json(format: mpv_format, data: *mut c_void) -> Value {
    if data.is_null() {
        return Value::Null;
    }
    // SAFETY: non-null, and each arm reads the C type its format denotes
    // (caller contract); strings and nodes are valid per the same contract.
    unsafe {
        match format {
            MPV_FORMAT_STRING | MPV_FORMAT_OSD_STRING => {
                Value::String(lossy(*(data as *const *const c_char)))
            }
            MPV_FORMAT_FLAG => Value::Bool(*(data as *const c_int) != 0),
            MPV_FORMAT_INT64 => Value::from(*(data as *const i64)),
            MPV_FORMAT_DOUBLE => serde_json::Number::from_f64(*(data as *const f64))
                .map_or(Value::Null, Value::Number),
            MPV_FORMAT_NODE => node_to_json(&*(data as *const mpv_node)),
            _ => Value::Null,
        }
    }
}

/// Converts an mpv node tree to JSON (unknown formats are null).
///
/// # Safety
///
/// `node` must be a well-formed node as mpv produces it: `u` holds the
/// member `format` names, strings are NUL-terminated, lists hold `num` valid
/// values (and keys, for maps), recursively.
unsafe fn node_to_json(node: &mpv_node) -> Value {
    // SAFETY: each arm reads the union member `format` names; list pointers
    // and counts are checked before building slices of `num` elements, which
    // a well-formed node guarantees are valid (caller contract), and the
    // contract holds for every child.
    unsafe {
        match node.format {
            MPV_FORMAT_STRING | MPV_FORMAT_OSD_STRING => Value::String(lossy(node.u.string)),
            MPV_FORMAT_FLAG => Value::Bool(node.u.flag != 0),
            MPV_FORMAT_INT64 => Value::from(node.u.int64),
            MPV_FORMAT_DOUBLE => {
                serde_json::Number::from_f64(node.u.double_).map_or(Value::Null, Value::Number)
            }
            MPV_FORMAT_NODE_ARRAY => {
                let list = &*node.u.list;
                if list.num <= 0 || list.values.is_null() {
                    return Value::Array(Vec::new());
                }
                let values = std::slice::from_raw_parts(list.values, list.num as usize);
                Value::Array(values.iter().map(|v| node_to_json(v)).collect())
            }
            MPV_FORMAT_NODE_MAP => {
                let list = &*node.u.list;
                if list.num <= 0 || list.values.is_null() || list.keys.is_null() {
                    return Value::Object(Map::new());
                }
                let n = list.num as usize;
                let values = std::slice::from_raw_parts(list.values, n);
                let keys = std::slice::from_raw_parts(list.keys, n);
                let mut map = Map::with_capacity(n);
                for (k, v) in keys.iter().zip(values) {
                    map.insert(lossy(*k), node_to_json(v));
                }
                Value::Object(map)
            }
            _ => Value::Null,
        }
    }
}
