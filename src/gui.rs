//! Native Win32 settings window for click settings and hotkey rebinding.
//!
//! The window is created once, lazily, and kept hidden between opens so opening
//! settings costs nothing at startup. It uses plain `STATIC`/`EDIT`/`COMBOBOX`/
//! `BUTTON` controls instead of a GUI toolkit, which keeps the executable small
//! and dependency-free.
//!
//! The window procedure never mutates application state. It validates what the
//! user typed and queues a [`SettingsEvent`] for the main loop, which then
//! rebinds hotkeys, saves the config, and decides whether the window closes.
//! Every `unsafe` block has a `SAFETY:` comment explaining its invariants.

use std::collections::VecDeque;
use std::io;
use std::ptr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use thiserror::Error;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, COLOR_WINDOW, DEFAULT_GUI_FONT, HBRUSH};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, GetDlgItem, GetWindowTextW,
    LoadCursorW, MessageBoxW, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowTextW,
    ShowWindow, BN_CLICKED, BS_PUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_SETCURSEL, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_NUMBER, HMENU, IDC_ARROW, MB_ICONWARNING,
    MB_OK, SW_HIDE, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION,
    WS_CHILD, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{Button, ClickType, Config, ConfigError, ConfigFields, HotkeyKey};
use crate::input::{last_error, module_handle, wide};

const WINDOW_CLASS: &str = "MegaClickerSettingsWindow";
const WINDOW_TITLE: &str = "MegaClicker settings";

/// Window style: a fixed-size window with a title bar and a close button.
const WINDOW_STYLE: u32 = WS_CAPTION | WS_SYSMENU;

const CLIENT_WIDTH: i32 = 460;
const CLIENT_HEIGHT: i32 = 440;

const FIRST_ROW: i32 = 16;
const ROW_STEP: i32 = 32;
const LABEL_X: i32 = 16;
const LABEL_WIDTH: i32 = 152;
const CONTROL_X: i32 = 176;
const CONTROL_WIDTH: i32 = 260;
const CONTROL_HEIGHT: i32 = 22;
/// Height passed when creating a combo box; it only affects the drop-down list.
const COMBO_HEIGHT: i32 = 200;
const BUTTON_WIDTH: i32 = 125;
const BUTTON_HEIGHT: i32 = 28;

/// Position and size of a child control, in client coordinates.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

const ID_INTERVAL: i32 = 101;
const ID_BUTTON: i32 = 102;
const ID_CLICK_TYPE: i32 = 103;
const ID_LIMIT: i32 = 104;
const ID_DELAY: i32 = 105;
const ID_TOGGLE_KEY: i32 = 106;
const ID_QUIT_KEY: i32 = 107;
const ID_PROFILE_KEY: i32 = 108;
const ID_STATUS: i32 = 109;
const ID_INTERVAL_JITTER: i32 = 110;
const ID_CURSOR_JITTER: i32 = 111;
const ID_SAVE: i32 = 1;
const ID_CANCEL: i32 = 2;

const CONTROL_IDS: [i32; 13] = [
    ID_INTERVAL,
    ID_INTERVAL_JITTER,
    ID_BUTTON,
    ID_CLICK_TYPE,
    ID_CURSOR_JITTER,
    ID_LIMIT,
    ID_DELAY,
    ID_TOGGLE_KEY,
    ID_QUIT_KEY,
    ID_PROFILE_KEY,
    ID_STATUS,
    ID_SAVE,
    ID_CANCEL,
];

/// Result of interacting with the settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    /// The user pressed Save with values that passed validation.
    Saved(Config),
    /// The user hid the window without saving.
    Closed,
}

static EVENTS: Mutex<VecDeque<SettingsEvent>> = Mutex::new(VecDeque::new());

fn pending_events() -> MutexGuard<'static, VecDeque<SettingsEvent>> {
    EVENTS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn push_event(event: SettingsEvent) {
    pending_events().push_back(event);
}

#[derive(Debug, Error)]
pub enum GuiError {
    #[error("could not register the settings window class: {0}")]
    RegisterClass(io::Error),

    #[error("could not create the settings window: {0}")]
    CreateWindow(io::Error),
}

/// A live settings window. Dropping it destroys the window.
pub struct Settings {
    hwnd: HWND,
}

impl Settings {
    /// Create the window, hidden and ready to be filled in by [`Self::open`].
    pub fn install() -> Result<Self, GuiError> {
        let class_name = wide(WINDOW_CLASS);
        let window_title = wide(WINDOW_TITLE);

        let window_class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module_handle(),
            hIcon: ptr::null_mut(),
            // SAFETY: A null instance with IDC_ARROW requests the shared system
            // arrow cursor, which must not be destroyed by us.
            hCursor: unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) },
            // System colour brushes are passed as a palette index plus one.
            hbrBackground: (COLOR_WINDOW + 1) as usize as HBRUSH,
            lpszMenuName: ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };

        // SAFETY: `window_class` is a fully initialized WNDCLASSW whose class
        // name string outlives the call. A previous failed install may already
        // have registered this class, which is safe to reuse.
        if unsafe { RegisterClassW(&window_class) } == 0 {
            // SAFETY: `GetLastError` has no preconditions and is read directly
            // after the failed registration call.
            let code = unsafe { GetLastError() };
            if code != ERROR_CLASS_ALREADY_EXISTS {
                return Err(GuiError::RegisterClass(last_error(
                    "RegisterClassW reported failure without an error code",
                )));
            }
        }

        let mut rect = RECT {
            left: 0,
            top: 0,
            right: CLIENT_WIDTH,
            bottom: CLIENT_HEIGHT,
        };

        // SAFETY: `rect` is a valid RECT describing the desired client area and
        // is updated in place with the matching window size.
        let adjusted = unsafe { AdjustWindowRect(&mut rect, WINDOW_STYLE, 0) };
        if adjusted == 0 {
            return Err(GuiError::CreateWindow(last_error(
                "AdjustWindowRect reported failure without an error code",
            )));
        }

        // SAFETY: Both strings are NUL-terminated and outlive the call. A null
        // parent creates an unowned top-level window; the null menu and creation
        // parameter are documented as valid.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                window_title.as_ptr(),
                WINDOW_STYLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rect.right - rect.left,
                rect.bottom - rect.top,
                ptr::null_mut(),
                ptr::null_mut(),
                module_handle(),
                ptr::null(),
            )
        };

        if hwnd.is_null() {
            return Err(GuiError::CreateWindow(last_error(
                "CreateWindowExW reported failure without an error code",
            )));
        }

        create_controls(hwnd);
        if CONTROL_IDS.iter().any(|id| control(hwnd, *id).is_null()) {
            // SAFETY: `hwnd` was created above and remains owned by this
            // function until all child controls have been verified.
            unsafe {
                DestroyWindow(hwnd);
            }
            return Err(GuiError::CreateWindow(io::Error::other(
                "could not create all settings controls",
            )));
        }

        Ok(Self { hwnd })
    }

    /// Fill every control from `config` and show the window.
    pub fn open(&self, config: &Config) {
        set_text(
            self.hwnd,
            ID_INTERVAL,
            &config.interval.as_millis().to_string(),
        );
        set_text(
            self.hwnd,
            ID_INTERVAL_JITTER,
            &config.interval_jitter_percent.to_string(),
        );
        set_text(
            self.hwnd,
            ID_CURSOR_JITTER,
            &config.cursor_jitter_px.to_string(),
        );
        set_text(self.hwnd, ID_LIMIT, &config.click_limit.to_string());
        set_text(
            self.hwnd,
            ID_DELAY,
            &config.start_delay.as_secs().to_string(),
        );

        select_combo(
            self.hwnd,
            ID_BUTTON,
            Button::ALL
                .iter()
                .position(|button| *button == config.button)
                .unwrap_or(0),
        );
        select_combo(
            self.hwnd,
            ID_CLICK_TYPE,
            ClickType::ALL
                .iter()
                .position(|click_type| *click_type == config.click_type)
                .unwrap_or(0),
        );
        select_combo(self.hwnd, ID_TOGGLE_KEY, hotkey_index(config.toggle_hotkey));
        select_combo(self.hwnd, ID_QUIT_KEY, hotkey_index(config.quit_hotkey));
        select_combo(
            self.hwnd,
            ID_PROFILE_KEY,
            hotkey_index(config.profile_hotkey),
        );

        // SAFETY: `hwnd` is the live window created in `install`. Showing and
        // focusing it are the documented ways to bring it to the user.
        unsafe {
            ShowWindow(self.hwnd, SW_SHOW);
            SetForegroundWindow(self.hwnd);
        }
    }

    /// Hide the window. The window object is reused by the next open.
    pub fn hide(&self) {
        hide(self.hwnd);
    }

    /// Pop the next event produced by the window.
    pub fn try_recv(&self) -> Option<SettingsEvent> {
        pending_events().pop_front()
    }

    /// Show a modal error box owned by this settings window.
    pub fn show_error(&self, message: &str) {
        show_error_on(self.hwnd, message);
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        // SAFETY: `self.hwnd` was created by `install` and is owned by this
        // struct. Drop runs at most once, so it is destroyed exactly once.
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}

fn show_error_on(owner: HWND, message: &str) {
    let text = wide(message);
    let caption = wide(WINDOW_TITLE);

    // SAFETY: Both strings are NUL-terminated and outlive the call. `owner` is
    // either null or the live settings window, which stays enabled while the
    // message box runs.
    unsafe {
        MessageBoxW(
            owner,
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

fn create_controls(hwnd: HWND) {
    label(hwnd, "Interval (ms)", row(0));
    label(hwnd, "Interval jitter (%)", row(1));
    label(hwnd, "Mouse button", row(2));
    label(hwnd, "Click type", row(3));
    label(hwnd, "Cursor jitter (px)", row(4));
    label(hwnd, "Click limit (0 = unlimited)", row(5));
    label(hwnd, "Start delay (s)", row(6));
    label(hwnd, "Toggle hotkey", row(7));
    label(hwnd, "Quit hotkey", row(8));
    label(hwnd, "Profile hotkey", row(9));

    edit(hwnd, ID_INTERVAL, row(0), true);
    edit(hwnd, ID_INTERVAL_JITTER, row(1), true);
    combo(hwnd, ID_BUTTON, row(2));
    combo(hwnd, ID_CLICK_TYPE, row(3));
    edit(hwnd, ID_CURSOR_JITTER, row(4), true);
    edit(hwnd, ID_LIMIT, row(5), true);
    edit(hwnd, ID_DELAY, row(6), true);
    combo(hwnd, ID_TOGGLE_KEY, row(7));
    combo(hwnd, ID_QUIT_KEY, row(8));
    combo(hwnd, ID_PROFILE_KEY, row(9));

    let hint = "Saving writes config.toml and rebinds hotkeys immediately.\n\
                A named profile keeps its own click settings.";
    create_control(
        hwnd,
        "STATIC",
        hint,
        0,
        ID_STATUS,
        Bounds {
            x: LABEL_X,
            y: row(9) + 44,
            width: CLIENT_WIDTH - 2 * LABEL_X,
            height: 34,
        },
    );

    button(hwnd, ID_SAVE, "Save", CONTROL_X, row(9) + 96);
    button(hwnd, ID_CANCEL, "Cancel", CONTROL_X + 135, row(9) + 96);

    let buttons: Vec<String> = Button::ALL
        .iter()
        .map(|button| button.as_str().to_owned())
        .collect();
    fill_combo(hwnd, ID_BUTTON, &buttons);

    let click_types: Vec<String> = ClickType::ALL
        .iter()
        .map(|click_type| click_type.as_str().to_owned())
        .collect();
    fill_combo(hwnd, ID_CLICK_TYPE, &click_types);

    let hotkeys = HotkeyKey::ALL.map(|key| key.as_str().to_owned());
    for id in [ID_TOGGLE_KEY, ID_QUIT_KEY, ID_PROFILE_KEY] {
        fill_combo(hwnd, id, &hotkeys);
    }

    set_font(hwnd);
}

fn row(index: i32) -> i32 {
    FIRST_ROW + index * ROW_STEP
}

fn hotkey_index(key: HotkeyKey) -> usize {
    HotkeyKey::ALL
        .iter()
        .position(|candidate| *candidate == key)
        .unwrap_or(0)
}

fn create_control(
    parent: HWND,
    class: &str,
    text: &str,
    extra_style: u32,
    id: i32,
    bounds: Bounds,
) -> HWND {
    let class = wide(class);
    let text = wide(text);

    // SAFETY: Both strings are NUL-terminated and outlive the call, `parent` is
    // the live settings window, and Win32 accepts the numeric id cast to HMENU
    // for a child control. The returned handle is checked by the callers that
    // need it.
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | extra_style,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            parent,
            id as isize as HMENU,
            module_handle(),
            ptr::null(),
        )
    }
}

fn label(parent: HWND, text: &str, y: i32) {
    create_control(
        parent,
        "STATIC",
        text,
        0,
        0,
        Bounds {
            x: LABEL_X,
            y: y + 4,
            width: LABEL_WIDTH,
            height: CONTROL_HEIGHT,
        },
    );
}

fn edit(parent: HWND, id: i32, y: i32, numeric: bool) {
    let style =
        WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32 | if numeric { ES_NUMBER as u32 } else { 0 };
    create_control(
        parent,
        "EDIT",
        "",
        style,
        id,
        Bounds {
            x: CONTROL_X,
            y,
            width: CONTROL_WIDTH,
            height: CONTROL_HEIGHT,
        },
    );
}

fn button(parent: HWND, id: i32, text: &str, x: i32, y: i32) {
    create_control(
        parent,
        "BUTTON",
        text,
        BS_PUSHBUTTON as u32 | WS_TABSTOP,
        id,
        Bounds {
            x,
            y,
            width: BUTTON_WIDTH,
            height: BUTTON_HEIGHT,
        },
    );
}

fn combo(parent: HWND, id: i32, y: i32) {
    create_control(
        parent,
        "COMBOBOX",
        "",
        CBS_DROPDOWNLIST as u32 | WS_VSCROLL | WS_TABSTOP,
        id,
        Bounds {
            x: CONTROL_X,
            y,
            width: CONTROL_WIDTH,
            height: COMBO_HEIGHT,
        },
    );
}

fn fill_combo(parent: HWND, id: i32, items: &[String]) {
    let control = control(parent, id);
    if control.is_null() {
        return;
    }

    for item in items {
        let text = wide(item);
        // SAFETY: `control` is a live combo box and `text` is NUL-terminated and
        // outlives the call, as CB_ADDSTRING requires.
        unsafe {
            SendMessageW(control, CB_ADDSTRING, 0, text.as_ptr() as isize);
        }
    }
}

fn select_combo(parent: HWND, id: i32, index: usize) {
    let control = control(parent, id);
    if control.is_null() {
        return;
    }

    // SAFETY: `control` is a live combo box; CB_SETCURSEL expects a zero-based
    // index and simply fails for out-of-range values.
    unsafe {
        SendMessageW(control, CB_SETCURSEL, index, 0);
    }
}

fn selected_combo(parent: HWND, id: i32) -> usize {
    let control = control(parent, id);
    if control.is_null() {
        return 0;
    }

    // SAFETY: `control` is a live combo box; CB_GETCURSEL returns the index or
    // CB_ERR, which is negative.
    let selected = unsafe { SendMessageW(control, CB_GETCURSEL, 0, 0) };
    usize::try_from(selected).unwrap_or(0)
}

fn set_text(parent: HWND, id: i32, text: &str) {
    let control = control(parent, id);
    if control.is_null() {
        return;
    }

    let text = wide(text);
    // SAFETY: `control` is a live edit control and `text` is NUL-terminated and
    // outlives the call.
    unsafe {
        SetWindowTextW(control, text.as_ptr());
    }
}

fn read_text(parent: HWND, id: i32) -> String {
    let control = control(parent, id);
    if control.is_null() {
        return String::new();
    }

    let mut buffer = [0u16; 64];
    // SAFETY: `buffer` is a valid writable array of `buffer.len()` UTF-16 units
    // and the count passed to Windows matches its length.
    let length = unsafe { GetWindowTextW(control, buffer.as_mut_ptr(), buffer.len() as i32) };

    let length = usize::try_from(length).unwrap_or(0).min(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

fn control(parent: HWND, id: i32) -> HWND {
    // SAFETY: `parent` is the live settings window; GetDlgItem simply returns a
    // null handle for an unknown id.
    unsafe { GetDlgItem(parent, id) }
}

fn set_font(parent: HWND) {
    // SAFETY: `DEFAULT_GUI_FONT` is a shared stock object owned by the system,
    // so it must never be deleted by us.
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };

    for id in CONTROL_IDS {
        let child = control(parent, id);
        if child.is_null() {
            continue;
        }

        // SAFETY: `child` is a live child window and `font` is a valid stock
        // font handle. A non-zero lParam asks the control to repaint.
        unsafe {
            SendMessageW(child, WM_SETFONT, font as usize, 1);
        }
    }
}

/// Read every control and turn the text into config fields. Ranges and unique
/// hotkeys are checked by [`Config::from_fields`].
fn collect_fields(hwnd: HWND) -> Result<ConfigFields, ConfigError> {
    let button = selected_combo(hwnd, ID_BUTTON);
    let click_type = selected_combo(hwnd, ID_CLICK_TYPE);

    Ok(ConfigFields {
        interval_ms: read_number(hwnd, ID_INTERVAL, "interval_ms")?,
        interval_jitter_percent: read_number(hwnd, ID_INTERVAL_JITTER, "interval_jitter_percent")?,
        button: Button::ALL[button.min(Button::ALL.len() - 1)],
        click_type: ClickType::ALL[click_type.min(ClickType::ALL.len() - 1)],
        cursor_jitter_px: read_number(hwnd, ID_CURSOR_JITTER, "cursor_jitter_px")?,
        click_limit: read_number(hwnd, ID_LIMIT, "click_limit")?,
        start_delay_seconds: read_number(hwnd, ID_DELAY, "start_delay_seconds")?,
        toggle_hotkey: selected_hotkey(hwnd, ID_TOGGLE_KEY),
        quit_hotkey: selected_hotkey(hwnd, ID_QUIT_KEY),
        profile_hotkey: selected_hotkey(hwnd, ID_PROFILE_KEY),
    })
}

fn selected_hotkey(hwnd: HWND, id: i32) -> HotkeyKey {
    let index = selected_combo(hwnd, id);
    HotkeyKey::ALL[index.min(HotkeyKey::ALL.len() - 1)]
}

fn read_number(hwnd: HWND, id: i32, field: &'static str) -> Result<i64, ConfigError> {
    let text = read_text(hwnd, id);
    let trimmed = text.trim();

    trimmed
        .parse::<i64>()
        .map_err(|_| ConfigError::InvalidField {
            field,
            value: if trimmed.is_empty() {
                "<empty>".to_owned()
            } else {
                trimmed.to_owned()
            },
            expected: "a whole number",
        })
}

fn save(hwnd: HWND) {
    match collect_fields(hwnd).and_then(|fields| Config::from_fields(&fields)) {
        // The main loop decides whether the window closes: it still has to
        // rebind hotkeys and write the file, either of which can fail.
        Ok(config) => push_event(SettingsEvent::Saved(config)),
        Err(error) => show_error_on(hwnd, &error.to_string()),
    }
}

fn hide(hwnd: HWND) {
    // SAFETY: `hwnd` is the live settings window; hiding it keeps the window
    // object alive for the next open.
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
    push_event(SettingsEvent::Closed);
}

// SAFETY: The window procedure is only invoked by Windows for this module's
// window. It never unwinds and only touches the controls it owns and the shared
// event queue, which tolerates poisoned locks.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam & 0xffff) as i32;
            let notification = ((wparam >> 16) & 0xffff) as u32;

            if notification == BN_CLICKED {
                match id {
                    ID_SAVE => save(hwnd),
                    ID_CANCEL => hide(hwnd),
                    _ => {}
                }
            }
            0
        }
        // Closing the window only hides it, so the next open is instant.
        WM_CLOSE => {
            hide(hwnd);
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
    fn hotkeys_are_listed_in_function_key_order() {
        for (index, key) in HotkeyKey::ALL.iter().enumerate() {
            assert_eq!(hotkey_index(*key), index);
        }
        assert_eq!(hotkey_index(HotkeyKey::F6), 5);
        assert_eq!(hotkey_index(HotkeyKey::F12), 11);
    }

    #[test]
    fn rows_are_evenly_spaced() {
        assert_eq!(row(0), FIRST_ROW);
        assert_eq!(row(1) - row(0), ROW_STEP);
        assert!(row(9) + 96 + BUTTON_HEIGHT <= CLIENT_HEIGHT);
    }
}
