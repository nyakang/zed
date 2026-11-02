use gpui_util::ResultExt as _;
use windows::Win32::{
    Foundation::{HWND, POINT},
    UI::{
        Input::KeyboardAndMouse::GetFocus,
        WindowsAndMessaging::{GetCursorPos, HCURSOR, IsChild, SetCursor, WindowFromPoint},
    },
};

pub(crate) fn native_child_owns_cursor(owner: HWND, target: HWND) -> bool {
    !target.is_invalid() && unsafe { IsChild(owner, target).as_bool() }
}

pub(crate) fn set_cursor(owner: HWND, target: HWND, cursor: Option<HCURSOR>) -> bool {
    let native = native_child_owns_cursor(owner, target);
    log::trace!(target: "gpui::native_cursor", "cursor owner={} target={} native={} focus={} cursor={}", owner.0 as usize, target.0 as usize, native, unsafe { GetFocus() }.0 as usize, cursor.map_or(0, |cursor| cursor.0 as usize));
    if native {
        return false;
    }
    unsafe { SetCursor(cursor) };
    true
}

pub(crate) fn set_cursor_at_pointer(owner: HWND, cursor: Option<HCURSOR>) {
    let mut position = POINT::default();
    if unsafe { GetCursorPos(&mut position) }.log_err().is_some() {
        // A GPUI redraw can arrive while mstscax's input child owns the pointer.
        // Cache changes in GPUI, but leave the system cursor to that child.
        set_cursor(owner, unsafe { WindowFromPoint(position) }, cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::{native_child_owns_cursor, set_cursor};
    use windows::{
        Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, GetCursor, IDC_ARROW, IDC_IBEAM, LoadCursorW,
                SetCursor, WINDOW_EX_STYLE, WS_CHILD, WS_OVERLAPPED, WS_POPUP,
            },
        },
        core::w,
    };

    struct Window(HWND);
    impl Drop for Window {
        fn drop(&mut self) {
            unsafe { DestroyWindow(self.0) }.expect("destroy cursor test window");
        }
    }

    fn window(parent: Option<HWND>) -> Window {
        Window(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!(""),
                    if parent.is_some() {
                        WS_CHILD
                    } else {
                        WS_OVERLAPPED
                    },
                    0,
                    0,
                    100,
                    100,
                    parent,
                    None,
                    None,
                    None,
                )
            }
            .expect("create cursor test window"),
        )
    }

    #[test]
    fn native_children_keep_their_cursor_while_owner_and_owned_popups_use_gpui_cursor() {
        let owner = window(None);
        let child = window(Some(owner.0));
        let nested = window(Some(child.0));
        let popup = Window(
            unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    100,
                    100,
                    Some(owner.0),
                    None,
                    None,
                    None,
                )
            }
            .expect("create owned overlay"),
        );
        let local = unsafe { LoadCursorW(None, IDC_ARROW) }.expect("local cursor");
        let remote = unsafe { LoadCursorW(None, IDC_IBEAM) }.expect("remote cursor");
        let previous = unsafe { SetCursor(Some(remote)) };
        for target in [child.0, nested.0] {
            assert!(native_child_owns_cursor(owner.0, target));
            assert!(!set_cursor(owner.0, target, Some(local)));
            assert_eq!(unsafe { GetCursor() }, remote);
            assert!(!set_cursor(owner.0, target, None));
            assert_eq!(unsafe { GetCursor() }, remote);
        }
        for target in [owner.0, popup.0] {
            assert!(!native_child_owns_cursor(owner.0, target));
            assert!(set_cursor(owner.0, target, Some(local)));
            assert_eq!(unsafe { GetCursor() }, local);
            assert!(set_cursor(owner.0, target, None));
            assert!(unsafe { GetCursor() }.is_invalid());
        }
        unsafe { SetCursor(Some(previous)) };
    }
}
