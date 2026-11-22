use anyhow::{Context as _, Result};
use gpui::Point;
use gpui_util::ResultExt as _;
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT},
    UI::{
        Input::{
            GetRawInputData, GetRegisteredRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
            RAWINPUTHEADER, RID_INPUT, RIDEV_REMOVE, RIM_TYPEMOUSE, RegisterRawInputDevices,
        },
        WindowsAndMessaging::{ClipCursor, GetClipCursor, GetCursorPos, GetForegroundWindow},
    },
};

#[derive(Default)]
pub(crate) struct PointerLock {
    owner: Option<HWND>,
    previous: Option<RAWINPUTDEVICE>,
    clip: Option<RECT>,
}

impl PointerLock {
    pub(crate) fn active(&self) -> bool {
        self.owner.is_some()
    }

    pub(crate) fn acquire(&mut self, window: HWND) -> Result<()> {
        if self.active() {
            return Ok(());
        }
        anyhow::ensure!(
            unsafe { GetForegroundWindow() } == window,
            "pointer lock requires the foreground window"
        );
        let previous = mouse_registration()?;
        let mut position = POINT::default();
        unsafe { GetCursorPos(&mut position) }.context("cannot read pointer position")?;
        let clip = RECT {
            left: position.x,
            top: position.y,
            right: position.x.saturating_add(1),
            bottom: position.y.saturating_add(1),
        };
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            hwndTarget: window,
            ..Default::default()
        };
        unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }
            .context("cannot register relative pointer input")?;
        self.owner = Some(window);
        self.previous = previous;
        if let Err(error) = unsafe { ClipCursor(Some(&clip)) } {
            self.release();
            return Err(error).context("cannot constrain pointer");
        }
        self.clip = Some(clip);
        Ok(())
    }

    pub(crate) fn release(&mut self) {
        let Some(owner) = self.owner.take() else {
            return;
        };
        if let Some(clip) = self.clip.take() {
            let mut current = RECT::default();
            // Do not undo a constraint installed by another native component.
            if unsafe { GetClipCursor(&mut current) }.is_ok() && current == clip {
                unsafe { ClipCursor(None) }.log_err();
            }
        }
        let previous = self.previous.take();
        match mouse_registration() {
            Ok(Some(current)) if current.hwndTarget == owner => {
                let device = previous.unwrap_or(RAWINPUTDEVICE {
                    usUsagePage: 1,
                    usUsage: 2,
                    dwFlags: RIDEV_REMOVE,
                    ..Default::default()
                });
                unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }
                    .log_err();
            }
            Ok(_) => {}
            Err(error) => log::warn!("cannot inspect relative pointer registration: {error}"),
        }
    }
}

impl Drop for PointerLock {
    fn drop(&mut self) {
        self.release();
    }
}

fn mouse_registration() -> Result<Option<RAWINPUTDEVICE>> {
    let mut count = 0;
    let size = size_of::<RAWINPUTDEVICE>() as u32;
    if unsafe { GetRegisteredRawInputDevices(None, &mut count, size) } == u32::MAX {
        return Err(std::io::Error::last_os_error()).context("cannot inspect raw input devices");
    }
    if count == 0 {
        return Ok(None);
    }
    anyhow::ensure!(count <= 1024, "too many raw input registrations");
    let mut devices = vec![RAWINPUTDEVICE::default(); count as usize];
    let received =
        unsafe { GetRegisteredRawInputDevices(Some(devices.as_mut_ptr()), &mut count, size) };
    if received == u32::MAX {
        return Err(std::io::Error::last_os_error()).context("cannot inspect raw input devices");
    }
    Ok(devices
        .into_iter()
        .take(received as usize)
        .find(|device| device.usUsagePage == 1 && device.usUsage == 2))
}

pub(crate) fn relative_motion(parameter: LPARAM) -> Option<Point<f32>> {
    let mut input = RAWINPUT::default();
    let mut length = size_of::<RAWINPUT>() as u32;
    let received = unsafe {
        GetRawInputData(
            HRAWINPUT(parameter.0 as _),
            RID_INPUT,
            Some((&mut input as *mut RAWINPUT).cast()),
            &mut length,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if received == u32::MAX
        || received < size_of::<RAWINPUTHEADER>() as u32
        || input.header.dwType != RIM_TYPEMOUSE.0
    {
        return None;
    }
    let mouse = unsafe { input.data.mouse };
    // Absolute devices (including some tablet/RDP sources) use normalized
    // coordinates, not motion deltas. Never reinterpret them as a large jump.
    motion_delta(mouse.usFlags.0, mouse.lLastX, mouse.lLastY)
}

fn motion_delta(flags: u16, x: i32, y: i32) -> Option<Point<f32>> {
    (flags & 1 == 0 && (x != 0 || y != 0)).then_some(Point {
        x: x as f32,
        y: y as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::motion_delta;
    use gpui::point;

    #[test]
    fn raw_motion_preserves_sign_and_rejects_absolute_devices() {
        assert_eq!(motion_delta(0, -12, 34), Some(point(-12., 34.)));
        assert_eq!(motion_delta(1, 30000, 50000), None);
        assert_eq!(motion_delta(0, 0, 0), None);
    }
}
