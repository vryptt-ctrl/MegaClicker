//! Win32 input, hotkey, and process-wide lock wrappers.
//!
//! MegaClicker needs six small Win32 surfaces:
//! * `SendInput` for synthetic mouse clicks,
//! * `GetCursorPos`/`SetCursorPos` for optional cursor jitter,
//! * `RegisterHotKey`/`UnregisterHotKey` for global hotkeys,
//! * the thread message queue for `WM_HOTKEY` delivery,
//! * a message pump for the tray and settings windows,
//! * a named mutex for single-instance enforcement.
//!
//! Every FFI call is wrapped in a safe Rust API and every `unsafe` block has a
//! `SAFETY:` comment explaining the invariants it relies on. The notification
//! area wrapper lives in `tray.rs` and the settings window in `gui.rs`, the only
//! other modules with `unsafe` code.

use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, POINT,
};
use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_MOUSE, MOD_NOREPEAT,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VK_F1, VK_F10,
    VK_F11, VK_F12, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, MsgWaitForMultipleObjectsEx, PeekMessageW, SetCursorPos,
    TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT, WM_HOTKEY,
};

use crate::config::{Button, ClickType, HotkeyKey};

// A click is injected through a single SendInput batch. Two events for a
// single click, four for a double click.
const SINGLE_CLICK_EVENTS: u32 = 2;
const DOUBLE_CLICK_EVENTS: u32 = 4;

#[derive(Debug, Error)]
pub enum InputError {
    #[error("SendInput delivered {sent} of {expected} mouse events: {detail}")]
    SendFailed {
        sent: usize,
        expected: usize,
        detail: String,
    },
}

/// Inject one click action at the current cursor position.
///
/// A "double" action sends two down/up pairs in one batch. No cursor movement
/// is requested, so the target is whatever is under the pointer.
pub fn send_click(button: Button, click_type: ClickType) -> Result<(), InputError> {
    let (down, up) = button_flags(button);
    let inputs = [
        mouse_input(down),
        mouse_input(up),
        mouse_input(down),
        mouse_input(up),
    ];
    let count = match click_type {
        ClickType::Single => SINGLE_CLICK_EVENTS,
        ClickType::Double => DOUBLE_CLICK_EVENTS,
    };

    // SAFETY: `inputs` is a valid, properly aligned array of `INPUT` values that
    // lives for the duration of the call. `count` never exceeds its length and
    // `cbSize` is the exact size of one `INPUT` for this architecture.
    let sent = unsafe { SendInput(count, inputs.as_ptr(), size_of::<INPUT>() as i32) };

    if sent == count {
        return Ok(());
    }

    Err(InputError::SendFailed {
        sent: sent as usize,
        expected: count as usize,
        detail: send_error_detail(),
    })
}

/// Current cursor position in physical screen pixels.
pub fn cursor_position() -> io::Result<(i32, i32)> {
    let mut point = POINT { x: 0, y: 0 };

    // SAFETY: `point` is a valid writable POINT that lives for the call.
    let ok = unsafe { GetCursorPos(&mut point) };

    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok((point.x, point.y))
    }
}

/// Move the cursor to absolute screen coordinates. Windows clamps values that
/// fall outside the desktop.
pub fn set_cursor_position(x: i32, y: i32) -> io::Result<()> {
    // SAFETY: Coordinates are plain integers; SetCursorPos has no pointer
    // preconditions.
    let ok = unsafe { SetCursorPos(x, y) };

    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn send_error_detail() -> String {
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code != 0 => format!(
            "{error}. If the target window runs elevated (as administrator), \
             Windows UIPI blocks synthetic input; run MegaClicker with the same \
             privilege level as the target."
        ),
        _ => "Windows reported no error code. If the target window runs elevated, \
              Windows UIPI blocks synthetic input."
            .to_owned(),
    }
}

fn button_flags(button: Button) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match button {
        Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn virtual_key_code(key: HotkeyKey) -> u32 {
    (match key {
        HotkeyKey::F1 => VK_F1,
        HotkeyKey::F2 => VK_F2,
        HotkeyKey::F3 => VK_F3,
        HotkeyKey::F4 => VK_F4,
        HotkeyKey::F5 => VK_F5,
        HotkeyKey::F6 => VK_F6,
        HotkeyKey::F7 => VK_F7,
        HotkeyKey::F8 => VK_F8,
        HotkeyKey::F9 => VK_F9,
        HotkeyKey::F10 => VK_F10,
        HotkeyKey::F11 => VK_F11,
        HotkeyKey::F12 => VK_F12,
    }) as u32
}

/// Register a thread-global hotkey. `id` must be unique for this thread.
pub fn register_hotkey(id: i32, key: HotkeyKey) -> io::Result<()> {
    // SAFETY: A null window handle associates the hotkey with the calling
    // thread's message queue, which is where `wait_for_hotkey` reads it. `id`
    // is supplied by our own single call site and the key code is a constant.
    let registered =
        unsafe { RegisterHotKey(ptr::null_mut(), id, MOD_NOREPEAT, virtual_key_code(key)) };

    if registered == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Unregister a hotkey registered by `register_hotkey`.
pub fn unregister_hotkey(id: i32) -> io::Result<()> {
    // SAFETY: A null window handle selects the current thread's hotkey table;
    // unregistering an id that is not registered simply fails safely.
    let unregistered = unsafe { UnregisterHotKey(ptr::null_mut(), id) };

    if unregistered == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Wait up to `timeout` for a global hotkey and return its id.
///
/// This uses `MsgWaitForMultipleObjectsEx` so the thread sleeps instead of
/// polling. `QS_ALLINPUT` wakes the call as soon as any message is queued,
/// including the messages that [`pump_messages`] dispatches for the tray and
/// settings windows. Only `WM_HOTKEY` is removed here; every other message
/// stays queued.
pub fn wait_for_hotkey(timeout: Duration) -> Option<i32> {
    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

    // SAFETY: Waiting for zero kernel objects with a null handle array is the
    // documented way to wait for messages only. `QS_ALLINPUT` is a valid wake
    // mask and the parameter types match the Win32 signature.
    unsafe {
        MsgWaitForMultipleObjectsEx(0, ptr::null(), milliseconds, QS_ALLINPUT, 0);
    }

    let mut message = MSG::default();

    // SAFETY: `message` is a valid writable MSG. Filtering to WM_HOTKEY means
    // only this message type is removed from the queue; all other messages are
    // left untouched. A null hwnd selects the current thread's queue.
    let has_message = unsafe {
        PeekMessageW(
            &mut message,
            ptr::null_mut(),
            WM_HOTKEY,
            WM_HOTKEY,
            PM_REMOVE,
        )
    };

    if has_message == 0 {
        return None;
    }

    i32::try_from(message.wParam).ok()
}

/// Upper bound on messages dispatched per call so a chatty window cannot starve
/// the clicker loop. Remaining messages are handled on the next pass.
const MAX_MESSAGES_PER_PUMP: usize = 64;

/// Dispatch queued window messages for this thread, leaving `WM_HOTKEY` in the
/// queue for [`wait_for_hotkey`].
///
/// The tray icon and the settings window both receive messages here, so the main
/// loop must call this regularly. Dispatching never blocks: a menu is only shown
/// while the user interacts with it, which happens inside `Tray`.
pub fn pump_messages() {
    let mut message = MSG::default();
    let mut handled = 0;

    while handled < MAX_MESSAGES_PER_PUMP {
        // SAFETY: `message` is a valid writable MSG and a null hwnd selects the
        // current thread's queue. The two ranges deliberately skip WM_HOTKEY so
        // hotkey delivery stays with `wait_for_hotkey`.
        let mut found = false;
        for (low, high) in [(0, WM_HOTKEY - 1), (WM_HOTKEY + 1, u32::MAX)] {
            if unsafe { PeekMessageW(&mut message, ptr::null_mut(), low, high, PM_REMOVE) } != 0 {
                found = true;
                break;
            }
        }

        if !found {
            break;
        }

        // SAFETY: `message` was filled in by a successful PeekMessageW and is
        // dispatched exactly once, which is the documented pump sequence.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        handled += 1;
    }
}

static CTRL_C_PRESSED: AtomicBool = AtomicBool::new(false);

// SAFETY: Windows calls this handler on its own thread with a valid control
// event code. The body only performs an atomic store on a `'static` variable,
// which is safe from any thread and never unwinds across the FFI boundary.
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
        CTRL_C_PRESSED.store(true, Ordering::SeqCst);
        // Returning TRUE tells Windows the event was handled, so the default
        // handler does not terminate the process immediately. The main loop
        // notices the flag and performs an orderly shutdown.
        1
    } else {
        0
    }
}

/// Install a Ctrl+C handler that requests a clean shutdown instead of letting
/// Windows terminate the process mid-click.
pub fn install_ctrl_c_handler() -> io::Result<()> {
    // SAFETY: The handler is a plain `extern "system"` function with no captured
    // state; it only touches a `'static` atomic. The callback remains valid for
    // the process lifetime because it is a function item.
    let installed = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };

    if installed == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Whether Ctrl+C (or Ctrl+Break) has been pressed since startup.
pub fn ctrl_c_pressed() -> bool {
    CTRL_C_PRESSED.load(Ordering::SeqCst)
}

/// A process-wide named mutex used to enforce one running instance.
pub struct NamedMutex {
    handle: HANDLE,
}

impl NamedMutex {
    /// Try to create/acquire a named mutex.
    ///
    /// `Ok(None)` means another process already owns the name.
    pub fn try_acquire(name: &str) -> io::Result<Option<Self>> {
        let wide_name: Vec<u16> = OsStr::new(name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `SetLastError` has no preconditions. Clearing it first prevents
        // a stale ERROR_ALREADY_EXISTS from an unrelated earlier call being
        // misinterpreted as an ownership conflict below.
        unsafe {
            SetLastError(0);
        }

        // SAFETY: `wide_name` is a NUL-terminated UTF-16 string that outlives
        // the call. Default security attributes and non-initial ownership are
        // requested. The returned handle is checked for null before use.
        let handle = unsafe { CreateMutexW(ptr::null(), 0, wide_name.as_ptr()) };

        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `GetLastError` has no preconditions and is read immediately
        // after CreateMutexW.
        let last_error = unsafe { GetLastError() };

        if last_error == ERROR_ALREADY_EXISTS {
            // SAFETY: `handle` is a valid handle returned by CreateMutexW. We
            // must close it even though we lost the race, otherwise the handle
            // would leak.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Ok(None);
        }

        Ok(Some(Self { handle }))
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` was returned by CreateMutexW and is owned by this
            // struct. Drop runs at most once, so it is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer for the `W` APIs.
pub(crate) fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Last Win32 error, or `fallback` when Windows reported no error code.
pub(crate) fn last_error(fallback: &str) -> io::Error {
    let error = io::Error::last_os_error();
    if error.raw_os_error().unwrap_or(0) == 0 {
        io::Error::other(fallback.to_owned())
    } else {
        error
    }
}

pub(crate) fn module_handle() -> HINSTANCE {
    // SAFETY: A null module name requests the executable's module handle, which
    // exists for the lifetime of the process.
    unsafe { GetModuleHandleW(ptr::null()) }
}
