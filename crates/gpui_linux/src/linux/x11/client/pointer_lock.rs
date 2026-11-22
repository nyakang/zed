use anyhow::Context as _;
use gpui::{Pixels, Point, point, px};
use gpui_util::ResultExt as _;
use x11rb::{
    connection::Connection,
    protocol::{
        xinput::{self, ConnectionExt as _},
        xproto::{self, ConnectionExt as _},
    },
    xcb_ffi::XCBConnection,
};

use super::{X11ClientState, X11ClientStatePtr};

pub(super) struct PointerLock {
    window: xproto::Window,
    root: xproto::Window,
    device: xinput::DeviceId,
    previous_mask: xinput::EventMask,
    position: Point<Pixels>,
}

impl X11ClientStatePtr {
    pub(crate) fn set_pointer_lock(
        &self,
        window: xproto::Window,
        enabled: bool,
    ) -> anyhow::Result<()> {
        let Some(client) = self.get_client() else {
            anyhow::ensure!(!enabled, "display connection is closed");
            return Ok(());
        };
        let mut state = client.0.borrow_mut();
        if !enabled {
            state.release_pointer_lock(window);
            return Ok(());
        }
        if let Some(lock) = &state.pointer_lock {
            anyhow::ensure!(
                lock.window == window,
                "another window owns the pointer lock"
            );
            return Ok(());
        }
        anyhow::ensure!(
            state.keyboard_focused_window == Some(window),
            "pointer lock requires keyboard focus"
        );
        let cursor = state
            .get_or_create_invisible_cursor()
            .context("cannot create invisible cursor")?;
        let connection = &state.xcb_connection;
        let pointer = connection.query_pointer(window)?.reply()?;
        let client_pointer = connection.xinput_xi_get_client_pointer(window)?.reply()?;
        anyhow::ensure!(client_pointer.set, "window has no pointer device");
        let device = client_pointer.deviceid;
        let previous_mask = connection
            .xinput_xi_get_selected_events(pointer.root)?
            .reply()?
            .masks
            .into_iter()
            .find(|mask| mask.deviceid == device)
            .unwrap_or(xinput::EventMask {
                deviceid: device,
                mask: vec![0u32.into()],
            });
        let mut mask = previous_mask.clone();
        if mask.mask.is_empty() {
            mask.mask.push(0u32.into());
        }
        mask.mask[0] |= xinput::XIEventMask::RAW_MOTION;
        connection
            .xinput_xi_select_events(pointer.root, &[mask])?
            .check()?;
        let grab = (|| -> anyhow::Result<()> {
            let mask = xinput::XIEventMask::MOTION
                | xinput::XIEventMask::BUTTON_PRESS
                | xinput::XIEventMask::BUTTON_RELEASE;
            let result = connection
                .xinput_xi_grab_device(
                    window,
                    x11rb::CURRENT_TIME,
                    cursor,
                    device,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                    xinput::GrabOwner::NO_OWNER,
                    &[u32::from(mask)],
                )?
                .reply()?;
            anyhow::ensure!(
                result.status == xproto::GrabStatus::SUCCESS,
                "pointer device is already grabbed"
            );
            Ok(())
        })();
        if let Err(error) = grab {
            restore_mask(connection, pointer.root, &previous_mask).log_err();
            return Err(error);
        }
        state.pointer_lock = Some(PointerLock {
            window,
            root: pointer.root,
            device,
            previous_mask,
            position: point(
                px(pointer.win_x as f32 / state.scale_factor),
                px(pointer.win_y as f32 / state.scale_factor),
            ),
        });
        if let Err(error) = state.xcb_connection.flush() {
            state.release_pointer_lock(window);
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) fn is_pointer_locked(&self, window: xproto::Window) -> bool {
        self.get_client()
            .is_some_and(|client| client.0.borrow().pointer_lock_position(window).is_some())
    }
}

fn restore_mask(
    connection: &XCBConnection,
    root: xproto::Window,
    previous: &xinput::EventMask,
) -> anyhow::Result<()> {
    let previously_selected = previous
        .mask
        .first()
        .is_some_and(|mask| mask.contains(xinput::XIEventMask::RAW_MOTION));
    if !previously_selected {
        let mut current = connection
            .xinput_xi_get_selected_events(root)?
            .reply()?
            .masks
            .into_iter()
            .find(|mask| mask.deviceid == previous.deviceid)
            .unwrap_or_else(|| previous.clone());
        if let Some(mask) = current.mask.first_mut() {
            *mask = (u32::from(*mask) & !u32::from(xinput::XIEventMask::RAW_MOTION)).into();
        }
        connection
            .xinput_xi_select_events(root, &[current])?
            .check()?;
    }
    Ok(())
}

impl X11ClientState {
    pub(super) fn cancel_pointer_lock(&mut self) -> Option<xproto::Window> {
        let window = self.pointer_lock.as_ref()?.window;
        self.release_pointer_lock(window);
        Some(window)
    }

    pub(super) fn release_pointer_lock(&mut self, window: xproto::Window) -> bool {
        if self
            .pointer_lock
            .as_ref()
            .is_none_or(|lock| lock.window != window)
        {
            return false;
        }
        let Some(lock) = self.pointer_lock.take() else {
            return false;
        };
        self.xcb_connection
            .xinput_xi_ungrab_device(x11rb::CURRENT_TIME, lock.device)
            .map_err(anyhow::Error::from)
            .and_then(|cookie| cookie.check().map_err(anyhow::Error::from))
            .log_err();
        restore_mask(&self.xcb_connection, lock.root, &lock.previous_mask).log_err();
        self.xcb_connection.flush().log_err();
        true
    }

    pub(super) fn pointer_lock_position(&self, window: xproto::Window) -> Option<Point<Pixels>> {
        self.pointer_lock
            .as_ref()
            .filter(|lock| lock.window == window)
            .map(|lock| lock.position)
    }

    pub(super) fn relative_pointer_event(
        &self,
        event: &xinput::RawMotionEvent,
    ) -> Option<(xproto::Window, Point<f32>)> {
        let lock = self
            .pointer_lock
            .as_ref()
            .filter(|lock| lock.device == event.deviceid)?;
        raw_delta(&event.valuator_mask, &event.axisvalues_raw).map(|delta| (lock.window, delta))
    }
}

fn raw_delta(mask: &[u32], values: &[xinput::Fp3232]) -> Option<Point<f32>> {
    let mask = *mask.first()?;
    let mut values = values.iter();
    let mut delta = point(0., 0.);
    for (axis, output) in [(0, &mut delta.x), (1, &mut delta.y)] {
        if mask & (1 << axis) != 0 {
            let value = values.next()?;
            *output = (f64::from(value.integral) + f64::from(value.frac) / 4294967296.) as f32;
        }
    }
    (delta.x != 0. || delta.y != 0.).then_some(delta)
}

#[cfg(test)]
mod tests {
    use super::raw_delta;
    use gpui::point;
    use x11rb::protocol::xinput::Fp3232;

    #[test]
    fn raw_axes_preserve_negative_fractions_and_ignore_scroll_only_events() {
        let half_negative = Fp3232 {
            integral: -1,
            frac: 1 << 31,
        };
        assert_eq!(raw_delta(&[2], &[half_negative]), Some(point(0., -0.5)));
        assert_eq!(raw_delta(&[4], &[half_negative]), None);
        assert_eq!(raw_delta(&[3], &[half_negative]), None);
        assert_eq!(raw_delta(&[], &[]), None);
    }
}
