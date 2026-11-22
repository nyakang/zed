use anyhow::Context as _;
use gpui::{CursorStyle, PlatformInput, PointerLockEvent, point};
use wayland_backend::client::ObjectId;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, protocol::wl_surface,
};
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{zwp_locked_pointer_v1, zwp_pointer_constraints_v1},
    relative_pointer::zv1::client::{zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1},
};

use super::{WaylandClientState, WaylandClientStatePtr};
use crate::linux::wayland::serial::SerialKind;

pub(super) struct PointerLock {
    surface: ObjectId,
    lock: zwp_locked_pointer_v1::ZwpLockedPointerV1,
    relative: zwp_relative_pointer_v1::ZwpRelativePointerV1,
    active: bool,
}

impl Drop for PointerLock {
    fn drop(&mut self) {
        self.relative.destroy();
        self.lock.destroy();
    }
}

impl WaylandClientState {
    pub(super) fn cancel_pointer_lock(&mut self) -> Option<super::WaylandWindowStatePtr> {
        let surface = self.pointer_lock.as_ref()?.surface.clone();
        self.release_pointer_lock(&surface);
        self.windows.get(&surface).cloned()
    }

    pub(super) fn pointer_lock_active(&self) -> bool {
        self.pointer_lock.as_ref().is_some_and(|lock| lock.active)
    }

    pub(super) fn release_pointer_lock(&mut self, surface: &ObjectId) -> bool {
        if !self
            .pointer_lock
            .as_ref()
            .is_some_and(|lock| &lock.surface == surface)
        {
            return false;
        }
        self.pointer_lock.take();
        let serial = self.serial_tracker.get(SerialKind::MouseEnter);
        self.apply_cursor_style(
            self.cursor_style.unwrap_or(CursorStyle::Arrow),
            serial.as_raw(),
        );
        true
    }
}

impl WaylandClientStatePtr {
    pub(crate) fn set_pointer_lock(
        &self,
        surface: &wl_surface::WlSurface,
        enabled: bool,
    ) -> anyhow::Result<()> {
        let Some(client) = self.try_get_client() else {
            anyhow::ensure!(!enabled, "display connection is closed");
            return Ok(());
        };
        let mut state = client.borrow_mut();
        let id = surface.id();
        if !enabled {
            state.release_pointer_lock(&id);
            return Ok(());
        }
        if let Some(lock) = &state.pointer_lock {
            anyhow::ensure!(lock.surface == id, "another window owns the pointer lock");
            return Ok(());
        }
        anyhow::ensure!(
            state
                .keyboard_focused_window
                .as_ref()
                .is_some_and(|window| window.surface().id() == id)
                && state
                    .mouse_focused_window
                    .as_ref()
                    .is_some_and(|window| window.surface().id() == id),
            "pointer lock requires keyboard and pointer focus"
        );
        let constraints = state
            .globals
            .pointer_constraints
            .as_ref()
            .context("compositor does not support pointer constraints")?;
        let relative = state
            .globals
            .relative_pointer_manager
            .as_ref()
            .context("compositor does not support relative pointers")?;
        let pointer = state.wl_pointer.as_ref().context("seat has no pointer")?;
        let lock = constraints.lock_pointer(
            surface,
            pointer,
            None,
            zwp_pointer_constraints_v1::Lifetime::Oneshot,
            &state.globals.qh,
            id.clone(),
        );
        let relative = relative.get_relative_pointer(pointer, &state.globals.qh, id.clone());
        state.pointer_lock = Some(PointerLock {
            surface: id,
            lock,
            relative,
            active: false,
        });
        // The lock must not persist and silently reactivate after focus returns.
        surface.commit();
        Ok(())
    }

    pub(crate) fn is_pointer_locked(&self, surface: &ObjectId) -> bool {
        self.try_get_client().is_some_and(|client| {
            client
                .borrow()
                .pointer_lock
                .as_ref()
                .is_some_and(|lock| lock.active && &lock.surface == surface)
        })
    }
}

delegate_noop!(WaylandClientStatePtr: ignore zwp_pointer_constraints_v1::ZwpPointerConstraintsV1);
delegate_noop!(WaylandClientStatePtr: ignore zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1);

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ObjectId> for WaylandClientStatePtr {
    fn event(
        this: &mut Self,
        proxy: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        surface: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let client = this.get_client();
        let mut state = client.borrow_mut();
        if !state
            .pointer_lock
            .as_ref()
            .is_some_and(|lock| lock.lock == *proxy && &lock.surface == surface)
        {
            return;
        }
        let event = match event {
            zwp_locked_pointer_v1::Event::Locked => {
                if let Some(lock) = state.pointer_lock.as_mut() {
                    lock.active = true;
                }
                let serial = state.serial_tracker.get(SerialKind::MouseEnter);
                state.apply_cursor_style(CursorStyle::Hidden, serial.as_raw());
                PointerLockEvent::Locked
            }
            zwp_locked_pointer_v1::Event::Unlocked => {
                state.release_pointer_lock(surface);
                PointerLockEvent::Unlocked
            }
            _ => return,
        };
        let window = state.windows.get(surface).cloned();
        drop(state);
        if let Some(window) = window {
            window.handle_input(PlatformInput::PointerLock(event));
        }
    }
}

impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ObjectId> for WaylandClientStatePtr {
    fn event(
        this: &mut Self,
        proxy: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        surface: &ObjectId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx_unaccel,
            dy_unaccel,
            ..
        } = event
        else {
            return;
        };
        let delta = point(dx_unaccel as f32, dy_unaccel as f32);
        if !delta.x.is_finite() || !delta.y.is_finite() {
            return;
        }
        let client = this.get_client();
        let state = client.borrow();
        if !state
            .pointer_lock
            .as_ref()
            .is_some_and(|lock| lock.active && lock.relative == *proxy && &lock.surface == surface)
        {
            return;
        }
        let window = state.windows.get(surface).cloned();
        drop(state);
        if let Some(window) = window {
            window.handle_input(PlatformInput::PointerLock(PointerLockEvent::Motion {
                delta,
            }));
        }
    }
}
