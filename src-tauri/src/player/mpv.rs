//! Safe(ish) wrapper around a libmpv handle. The client API is thread-safe,
//! so `Mpv` is shared between the event thread, Tauri commands and the GL
//! render callback.

use std::ffi::{CStr, CString, c_void};
use std::os::raw::{c_char, c_int};
use std::ptr;

use serde_json::{Map, Value};

use super::mpv_sys::*;

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
    let message = unsafe { CStr::from_ptr(mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned();
    Err(MpvError { code, context: context(), message })
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap()
}

pub struct Mpv {
    handle: *mut mpv_handle,
}

// SAFETY: the libmpv client API is documented as fully thread-safe.
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

#[derive(Debug)]
pub enum Event {
    Shutdown,
    Log { prefix: String, level: String, text: String },
    StartFile,
    EndFile { reason: EndReason, error: Option<String> },
    FileLoaded,
    Idle,
    VideoReconfig,
    PlaybackRestart,
    Seek,
    PropertyChange { name: String, value: Value },
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
            let rc = unsafe { mpv_set_option_string(handle, ck.as_ptr(), cv.as_ptr()) };
            if let Err(e) = check(rc, || format!("option {k}={v}")) {
                // Non-fatal: an unknown option should not prevent playback.
                log::warn!("{e}");
            }
        }
        check(unsafe { mpv_initialize(handle) }, || "initialize".into())?;
        Ok(mpv)
    }

    /// The raw handle; only the Linux render-API surface needs it today.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn raw(&self) -> *mut mpv_handle {
        self.handle
    }

    pub fn api_version() -> (u32, u32) {
        let v = unsafe { mpv_client_api_version() } as u32;
        (v >> 16, v & 0xffff)
    }

    fn with_args<R>(args: &[&str], f: impl FnOnce(*mut *const c_char) -> R) -> R {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        f(ptrs.as_mut_ptr())
    }

    pub fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        let rc = Self::with_args(args, |p| unsafe { mpv_command(self.handle, p) });
        check(rc, || format!("command {args:?}"))
    }

    /// Fire-and-forget command; the result arrives as a COMMAND_REPLY event.
    pub fn command_async(&self, args: &[&str]) -> Result<(), MpvError> {
        let rc = Self::with_args(args, |p| unsafe { mpv_command_async(self.handle, 0, p) });
        check(rc, || format!("command_async {args:?}"))
    }

    pub fn set_string(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let (n, v) = (cstr(name), cstr(value));
        let rc = unsafe { mpv_set_property_string(self.handle, n.as_ptr(), v.as_ptr()) };
        check(rc, || format!("set {name}={value}"))
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<(), MpvError> {
        let n = cstr(name);
        let mut v: c_int = value.into();
        let rc = unsafe {
            mpv_set_property(self.handle, n.as_ptr(), MPV_FORMAT_FLAG, &mut v as *mut _ as *mut c_void)
        };
        check(rc, || format!("set {name}={value}"))
    }

    pub fn set_double(&self, name: &str, value: f64) -> Result<(), MpvError> {
        let n = cstr(name);
        let mut v = value;
        let rc = unsafe {
            mpv_set_property(self.handle, n.as_ptr(), MPV_FORMAT_DOUBLE, &mut v as *mut _ as *mut c_void)
        };
        check(rc, || format!("set {name}={value}"))
    }

    pub fn set_int(&self, name: &str, value: i64) -> Result<(), MpvError> {
        let n = cstr(name);
        let mut v = value;
        let rc = unsafe {
            mpv_set_property(self.handle, n.as_ptr(), MPV_FORMAT_INT64, &mut v as *mut _ as *mut c_void)
        };
        check(rc, || format!("set {name}={value}"))
    }

    /// Sets a property from a JSON value, picking the matching mpv format.
    pub fn set_json(&self, name: &str, value: &Value) -> Result<(), MpvError> {
        match value {
            Value::Bool(b) => self.set_flag(name, *b),
            Value::Number(n) if n.is_i64() => self.set_int(name, n.as_i64().unwrap()),
            Value::Number(n) => self.set_double(name, n.as_f64().unwrap_or_default()),
            Value::String(s) => self.set_string(name, s),
            Value::Null => self.set_string(name, "no"),
            other => self.set_string(name, &other.to_string()),
        }
    }

    /// Reads any property as JSON (via MPV_FORMAT_NODE).
    pub fn get_json(&self, name: &str) -> Option<Value> {
        let n = cstr(name);
        let mut node: mpv_node = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            mpv_get_property(self.handle, n.as_ptr(), MPV_FORMAT_NODE, &mut node as *mut _ as *mut c_void)
        };
        if rc < 0 {
            return None;
        }
        let v = unsafe { node_to_json(&node) };
        unsafe { mpv_free_node_contents(&mut node) };
        Some(v)
    }

    pub fn observe(&self, name: &str, format: mpv_format) -> Result<(), MpvError> {
        let n = cstr(name);
        let rc = unsafe { mpv_observe_property(self.handle, 0, n.as_ptr(), format) };
        check(rc, || format!("observe {name}"))
    }

    pub fn request_log_messages(&self, level: &str) {
        let l = cstr(level);
        unsafe { mpv_request_log_messages(self.handle, l.as_ptr()) };
    }

    /// Blocks up to `timeout` seconds (negative = forever) for the next event.
    pub fn wait_event(&self, timeout: f64) -> Option<Event> {
        let ev = unsafe { &*mpv_wait_event(self.handle, timeout) };
        Some(match ev.event_id {
            MPV_EVENT_NONE => return None,
            MPV_EVENT_SHUTDOWN => Event::Shutdown,
            MPV_EVENT_LOG_MESSAGE => {
                let m = unsafe { &*(ev.data as *const mpv_event_log_message) };
                Event::Log {
                    prefix: unsafe { lossy(m.prefix) },
                    level: unsafe { lossy(m.level) },
                    text: unsafe { lossy(m.text) }.trim_end().to_owned(),
                }
            }
            MPV_EVENT_START_FILE => Event::StartFile,
            MPV_EVENT_END_FILE => {
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
                let p = unsafe { &*(ev.data as *const mpv_event_property) };
                let name = unsafe { lossy(p.name) };
                let value = unsafe { format_to_json(p.format, p.data) };
                Event::PropertyChange { name, value }
            }
            _ => Event::Other,
        })
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe { mpv_terminate_destroy(self.handle) }
    }
}

unsafe fn lossy(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

unsafe fn format_to_json(format: mpv_format, data: *mut c_void) -> Value {
    if data.is_null() {
        return Value::Null;
    }
    unsafe {
        match format {
            MPV_FORMAT_STRING | MPV_FORMAT_OSD_STRING => {
                Value::String(lossy(*(data as *const *const c_char)))
            }
            MPV_FORMAT_FLAG => Value::Bool(*(data as *const c_int) != 0),
            MPV_FORMAT_INT64 => Value::from(*(data as *const i64)),
            MPV_FORMAT_DOUBLE => {
                serde_json::Number::from_f64(*(data as *const f64)).map_or(Value::Null, Value::Number)
            }
            MPV_FORMAT_NODE => node_to_json(&*(data as *const mpv_node)),
            _ => Value::Null,
        }
    }
}

unsafe fn node_to_json(node: &mpv_node) -> Value {
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
