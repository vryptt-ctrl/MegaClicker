//! Native notification-area icon, popup menu, and shell launching.
//!
//! This module is the second and last place that contains `unsafe` code. It
//! owns a hidden top-level window that receives notification-area callbacks and
//! turns menu selections into events. Events are queued instead of being handled
//! inside the window procedure, so the main loop stays the only place that
//! changes clicker state.
//!
//! Every FFI call is wrapped in a safe Rust API and every `unsafe` block has a
//! `SAFETY:` comment explaining the invariants it relies on.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, LoadIconW, PeekMessageW, PostMessageW, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, IDI_APPLICATION, MF_SEPARATOR,
    MF_STRING, MSG, PM_REMOVE, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

const WINDOW_CLASS: &str = "MegaClickerTrayWindow";
const WINDOW_TITLE: &str = "MegaClicker";
const TRAY_ICON_ID: u32 = 1;

/// Notification-area callback message for this icon.
const TRAY_CALLBACK: u32 = WM_APP + 1;

const MENU_TOGGLE: usize = 1;
const MENU_NEXT_PROFILE: usize = 2;
const MENU_OPEN_CONFIG: usize = 3;
const MENU_OPEN_PROFILES: usize = 4;
const MENU_QUIT: usize = 5;

/// Actions requested from the tray icon. The main loop drains these and performs
/// the same work as the matching hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Toggle,
    NextProfile,
    OpenConfig,
    OpenProfiles,
    Quit,
}

static EVENTS: Mutex<VecDeque<TrayEvent>> = Mutex::new(VecDeque::new());

fn pending_events() -> MutexGuard<'static, VecDeque<TrayEvent>> {
    EVENTS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn push_event(event: TrayEvent) {
    pending_events().push_back(event);
}

#[derive(Debug, Error)]
pub enum TrayError {
    #[error("could not register the tray window class: {0}")]
    RegisterClass(io::Error),

    #[error("could not create the tray window: {0}")]
    CreateWindow(io::Error),

    #[error("could not load the tray icon: {0}")]
    LoadIcon(io::Error),

    #[error("could not add the notification-area icon: {0}")]
    AddIcon(io::Error),
}

/// A live notification-area icon. Dropping it removes the icon and destroys the
/// helper window.
pub struct Tray {
    hwnd: HWND,
    data: NOTIFYICONDATAW,
}

impl Tray {
    /// Install the icon and its hidden helper window. The tooltip is truncated
    /// to the 127 characters Windows accepts.
    pub fn install(tooltip: &str) -> Result<Self, TrayError> {
        let class_name = wide(WINDOW_CLASS);
        let window_title = wide(WINDOW_TITLE);
        let instance = module_handle();

        let window_class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };

        // SAFETY: `window_class` is a fully initialized WNDCLASSW whose class
        // name string outlives the call. Registering the same class twice fails
        // with ERROR_CLASS_ALREADY_EXISTS, which is harmless.
        let atom = unsafe { RegisterClassW(&window_class) };
        if atom == 0 {
            // SAFETY: `GetLastError` has no preconditions and is read directly
            // after the failed call.
            let code = unsafe { GetLastError() };
            if code != ERROR_CLASS_ALREADY_EXISTS {
                return Err(TrayError::RegisterClass(last_error(
                    "RegisterClassW reported failure without an error code",
                )));
            }
        }

        // SAFETY: Both class and title strings are NUL-terminated and outlive
        // the call. A null parent creates a hidden top-level window; null menu
        // and parameter values are documented as valid.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                window_title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null(),
            )
        };

        if hwnd.is_null() {
            return Err(TrayError::CreateWindow(last_error(
                "CreateWindowExW reported failure without an error code",
            )));
        }

        // SAFETY: A null instance with IDI_APPLICATION requests the shared
        // system application icon, which must not be destroyed by us.
        let icon = unsafe { LoadIconW(ptr::null_mut(), IDI_APPLICATION) };
        if icon.is_null() {
            // SAFETY: `hwnd` was created above, is still owned by this function,
            // and is not used again on this error path.
            unsafe {
                DestroyWindow(hwnd);
            }
            return Err(TrayError::LoadIcon(last_error(
                "LoadIconW reported failure without an error code",
            )));
        }

        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK,
            hIcon: icon,
            ..Default::default()
        };
        fill_tooltip(&mut data.szTip, tooltip);

        // SAFETY: `data` is a fully initialized NOTIFYICONDATAW with `cbSize`
        // set for this architecture and handles owned by this process.
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        if added == 0 {
            let error = last_error("Shell_NotifyIconW reported failure without an error code");
            // SAFETY: Same as above: the window is live and owned by this
            // function, and it is not used after this cleanup.
            unsafe {
                DestroyWindow(hwnd);
            }
            return Err(TrayError::AddIcon(error));
        }

        Ok(Self { hwnd, data })
    }

    /// Dispatch every pending message for this icon's window.
    ///
    /// Messages for other windows and thread-wide messages such as `WM_HOTKEY`
    /// are deliberately left in the queue.
    pub fn pump(&self) {
        let mut message = MSG::default();

        loop {
            // SAFETY: `message` is a writable MSG. Restricting the query to
            // `self.hwnd` leaves thread messages (including WM_HOTKEY) in the
            // queue for the hotkey wait path.
            let has_message = unsafe { PeekMessageW(&mut message, self.hwnd, 0, 0, PM_REMOVE) };
            if has_message == 0 {
                break;
            }

            // SAFETY: `message` was filled by PeekMessageW and still refers to
            // our live window. Translating and dispatching it is the documented
            // pump sequence.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// Pop the next event produced by a tray interaction.
    pub fn try_recv(&self) -> Option<TrayEvent> {
        pending_events().pop_front()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // SAFETY: `self.data` still describes the icon added in `install`, and
        // `self.hwnd` is the window that owns it. Both are destroyed exactly
        // once by this Drop implementation.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &self.data);
            DestroyWindow(self.hwnd);
        }
    }
}

/// Open a file or folder with its default shell handler.
pub fn open_path(path: &Path) -> io::Result<()> {
    let operation = wide("open");
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: All strings are NUL-terminated and outlive the call. The shell
    // treats the return value as an HINSTANCE whose value is greater than 32 on
    // success.
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    if result as isize <= 32 {
        Err(last_error(
            "ShellExecuteW reported failure without an error code",
        ))
    } else {
        Ok(())
    }
}

fn module_handle() -> HINSTANCE {
    // SAFETY: A null module name requests the executable's module handle, which
    // exists for the lifetime of the process.
    unsafe { GetModuleHandleW(ptr::null()) }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn fill_tooltip(buffer: &mut [u16; 128], text: &str) {
    let encoded: Vec<u16> = OsStr::new(text)
        .encode_wide()
        .take(buffer.len() - 1)
        .collect();

    buffer[..encoded.len()].copy_from_slice(&encoded);
    buffer[encoded.len()] = 0;
}

fn last_error(fallback: &str) -> io::Error {
    let error = io::Error::last_os_error();
    if error.raw_os_error().unwrap_or(0) == 0 {
        io::Error::other(fallback.to_owned())
    } else {
        error
    }
}

fn show_menu(hwnd: HWND) {
    let toggle = wide("Start / stop clicking");
    let next_profile = wide("Next profile");
    let open_config = wide("Open config");
    let open_profiles = wide("Open profiles folder");
    let quit = wide("Exit");

    // SAFETY: `CreatePopupMenu` has no preconditions. The returned handle is
    // checked before use and destroyed on every path below.
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }

    // SAFETY: `menu` is a live popup menu and every string pointer refers to a
    // local Vec<u16> that is NUL-terminated and outlives the call.
    unsafe {
        AppendMenuW(menu, MF_STRING, MENU_TOGGLE, toggle.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_NEXT_PROFILE, next_profile.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_OPEN_CONFIG, open_config.as_ptr());
        AppendMenuW(menu, MF_STRING, MENU_OPEN_PROFILES, open_profiles.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(menu, MF_STRING, MENU_QUIT, quit.as_ptr());
    }

    let mut point = POINT::default();

    // SAFETY: `point` is a writable POINT and `GetCursorPos` fills it when the
    // call succeeds.
    if unsafe { GetCursorPos(&mut point) } == 0 {
        // SAFETY: `menu` was created above and is not used after this point.
        unsafe {
            DestroyMenu(menu);
        }
        return;
    }

    // SAFETY: `hwnd` is the live tray window, `point` holds screen coordinates,
    // and `menu` is a live popup menu. TPM_RETURNCMD makes the call return the
    // selected command id instead of posting WM_COMMAND.
    let command = unsafe {
        SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            0,
            hwnd,
            ptr::null(),
        ) as usize;
        // Documented requirement so the menu closes when the user clicks away.
        PostMessageW(hwnd, WM_NULL, 0, 0);
        command
    };

    // SAFETY: `menu` was created by CreatePopupMenu and is no longer needed.
    unsafe {
        DestroyMenu(menu);
    }

    match command {
        MENU_TOGGLE => push_event(TrayEvent::Toggle),
        MENU_NEXT_PROFILE => push_event(TrayEvent::NextProfile),
        MENU_OPEN_CONFIG => push_event(TrayEvent::OpenConfig),
        MENU_OPEN_PROFILES => push_event(TrayEvent::OpenProfiles),
        MENU_QUIT => push_event(TrayEvent::Quit),
        _ => {}
    }
}

// SAFETY: The window procedure is only invoked by Windows for this module's
// window. It never unwinds: every path either returns a value or calls the
// default handler, and the shared event queue tolerates poisoned locks.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        TRAY_CALLBACK => {
            // Versions of the callback protocol pack the mouse message in the
            // low word; masking keeps both the legacy and version 4 layouts
            // working.
            match (lparam as u32) & 0xffff {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                WM_LBUTTONDBLCLK => push_event(TrayEvent::OpenConfig),
                _ => {}
            }
            0
        }
        // SAFETY: Every unhandled message is forwarded to the default handler
        // with the exact arguments Windows supplied.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_is_nul_terminated() {
        let mut buffer = [0u16; 128];
        fill_tooltip(&mut buffer, "abc");

        assert_eq!(
            &buffer[..4],
            &[u16::from(b'a'), u16::from(b'b'), u16::from(b'c'), 0]
        );
        assert!(buffer[4..].iter().all(|value| *value == 0));
    }

    #[test]
    fn tooltip_is_truncated_to_leave_room_for_the_terminator() {
        let mut buffer = [1u16; 128];
        fill_tooltip(&mut buffer, &"x".repeat(200));

        assert_eq!(buffer[127], 0);
        assert!(buffer[..127].iter().all(|value| *value == u16::from(b'x')));
    }

    #[test]
    fn wide_strings_end_with_a_nul() {
        let encoded = wide("open");

        assert_eq!(encoded.len(), 5);
        assert_eq!(
            &encoded[..4],
            &[
                u16::from(b'o'),
                u16::from(b'p'),
                u16::from(b'e'),
                u16::from(b'n')
            ]
        );
        assert_eq!(encoded[4], 0);
    }
}
