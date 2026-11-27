use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::{
    RegistrationToken,
    timer::{TimeoutAction, Timer},
};
use gpui::{Bounds, ScaledPixels};
use x11rb::{
    protocol::{Event, xproto},
    xcb_ffi::XCBConnection,
};
use xim::{ClientError, x11rb::X11rbClient};

use super::{X11Client, X11ClientState};
use crate::linux::x11::xim_handler::XimHandler;

pub(super) struct XimRuntime {
    pub(super) client: Option<X11rbClient<Rc<XCBConnection>>>,
    pub(super) handler: XimHandler,
    generation: u64,
    configured: bool,
    failures: u32,
    retry_at: Instant,
    dirty: bool,
    refresh_position: bool,
    timer: Option<(RegistrationToken, Instant)>,
}

impl XimRuntime {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            client: None,
            handler: XimHandler::new(),
            generation: 0,
            configured: std::env::var("XMODIFIERS").ok().is_some_and(|value| {
                value
                    .strip_prefix("@im=")
                    .is_some_and(|name| !name.is_empty() && name != "none")
            }),
            failures: 0,
            retry_at: now,
            dirty: false,
            refresh_position: false,
            timer: None,
        }
    }

    fn fail(&mut self, category: &'static str, now: Instant) {
        self.client = None;
        self.handler.clear_connection();
        let delay = retry_delay(self.failures);
        self.failures = self.failures.saturating_add(1);
        self.retry_at = now + delay;
        self.dirty = false;
        log::warn!(
            "XIM connection invalidated: category={category}, generation={}, retry_ms={}",
            self.generation,
            delay.as_millis()
        );
    }

    pub(super) fn focus(&mut self, window: Option<xproto::Window>) {
        self.handler.set_focus(window);
        self.dirty = true;
    }

    pub(super) fn remove_window(&mut self, window: xproto::Window) {
        self.handler.remove_window(window);
        self.dirty = true;
    }

    pub(super) fn set_spot(&mut self, window: xproto::Window, bounds: Bounds<ScaledPixels>) {
        self.handler.set_spot(window, spot_from_bounds(bounds));
        self.dirty = true;
    }

    pub(super) fn request_position(&mut self) {
        self.refresh_position = true;
        self.dirty = true;
    }

    pub(super) fn reset(&mut self) {
        self.handler.reset();
        self.dirty = true;
    }

    pub(super) fn filter_event(&mut self, event: &Event, now: Instant) -> Result<bool, ()> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        match client.filter_event(event, &mut self.handler) {
            Ok(handled) if !self.handler.disconnected => Ok(handled),
            Ok(_) => {
                self.fail("disconnected", now);
                Err(())
            }
            Err(error) => {
                self.fail(error_category(&error), now);
                Err(())
            }
        }
    }

    pub(super) fn forward(
        &mut self,
        event: &xproto::KeyPressEvent,
        now: Instant,
    ) -> Result<bool, ()> {
        let Some(client) = self.client.as_mut() else {
            return Ok(false);
        };
        match self.handler.forward(client, event) {
            Ok(forwarded) => Ok(forwarded),
            Err(error) => {
                self.fail(error_category(&error), now);
                Err(())
            }
        }
    }

    fn next_wakeup(&self, now: Instant) -> Option<Instant> {
        if !self.configured {
            return None;
        }
        if self.client.is_some() {
            if self.dirty {
                Some(now)
            } else {
                self.handler.deadline()
            }
        } else if self.refresh_position {
            Some(now)
        } else if self.handler.focused_window().is_some() {
            Some(self.retry_at.max(now))
        } else {
            None
        }
    }
}

fn retry_delay(failures: u32) -> Duration {
    Duration::from_millis(match failures {
        0 => 500,
        1 => 1000,
        2 => 2000,
        _ => 5000,
    })
}

fn error_category(error: &ClientError) -> &'static str {
    match error {
        ClientError::ReadProtocol(_) => "protocol-decode",
        ClientError::XimError(_, _) => "protocol-error",
        ClientError::UnsupportedTransport => "unsupported-transport",
        ClientError::InvalidReply => "invalid-reply",
        ClientError::NoXimServer => "server-unavailable",
        _ => "transport-error",
    }
}

fn spot_from_bounds(bounds: Bounds<ScaledPixels>) -> xim::Point {
    let coordinate = |value: f32| value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
    xim::Point {
        x: coordinate((bounds.origin.x + bounds.size.width).0),
        y: coordinate((bounds.origin.y + bounds.size.height).0),
    }
}

impl X11ClientState {
    pub(super) fn stop_xim_timer(&mut self) {
        if let Some((token, _)) = self.xim.timer.take() {
            self.loop_handle.remove(token);
        }
    }
}

impl X11Client {
    // Called by rendering adapters too: only enqueue work here. XIM requests
    // and input-handler callbacks run from the event loop, outside GPUI borrows.
    pub(super) fn schedule_xim(&self) {
        let mut state = self.0.borrow_mut();
        let Some(when) = state.xim.next_wakeup(Instant::now()) else {
            state.stop_xim_timer();
            return;
        };
        if state
            .xim
            .timer
            .is_some_and(|(_, scheduled)| scheduled <= when)
        {
            return;
        }
        state.stop_xim_timer();
        let this = state.this.clone();
        match state
            .loop_handle
            .insert_source(Timer::from_deadline(when), move |_, _, _| {
                if let Some(client) = this.get_client() {
                    client.0.borrow_mut().xim.timer = None;
                    client.service_xim();
                }
                TimeoutAction::Drop
            }) {
            Ok(token) => state.xim.timer = Some((token, when)),
            Err(_) => log::error!("Failed to schedule XIM maintenance"),
        }
    }

    pub(super) fn clear_xim_composition(&self) {
        let window = {
            let mut state = self.0.borrow_mut();
            state.composing = false;
            state.pre_key_char_down = None;
            state
                .keyboard_focused_window
                .and_then(|id| state.windows.get(&id).map(|window| window.window.clone()))
        };
        if let Some(window) = window {
            window.handle_ime_delete();
        }
    }

    pub(super) fn service_xim(&self) {
        let refresh = {
            let mut state = self.0.borrow_mut();
            if std::mem::take(&mut state.xim.refresh_position) {
                state.keyboard_focused_window.and_then(|id| {
                    state
                        .windows
                        .get(&id)
                        .map(|window| (id, window.window.clone()))
                })
            } else {
                None
            }
        };
        if let Some((id, window)) = refresh {
            let bounds = window.get_ime_area();
            let mut state = self.0.borrow_mut();
            if state.keyboard_focused_window == Some(id) {
                state.xim.focus(bounds.map(|_| id));
                if let Some(bounds) = bounds {
                    state.xim.set_spot(id, bounds);
                }
            }
        }
        let now = Instant::now();
        let failed = {
            let mut state = self.0.borrow_mut();
            let connection = state.xcb_connection.clone();
            let screen = state.x_root_index;
            let runtime = &mut state.xim;
            let mut failed = false;
            if runtime.configured
                && runtime.client.is_none()
                && runtime.handler.focused_window().is_some()
                && now >= runtime.retry_at
            {
                runtime.generation = runtime.generation.wrapping_add(1);
                match X11rbClient::init(connection, screen, None) {
                    Ok(client) => {
                        runtime.handler.begin_connection(now);
                        runtime.client = Some(client);
                        log::debug!("XIM handshake started: generation={}", runtime.generation);
                    }
                    Err(error) => {
                        runtime.fail(error_category(&error), now);
                        failed = true;
                    }
                }
            }
            if runtime.client.is_some() && runtime.handler.timed_out(now) {
                runtime.fail("request-timeout", now);
                failed = true;
            }
            if let Some(client) = runtime.client.as_mut() {
                if let Err(error) = runtime.handler.drive(client, now) {
                    runtime.fail(error_category(&error), now);
                    failed = true;
                } else if runtime
                    .handler
                    .focused_window()
                    .is_some_and(|window| runtime.handler.ready_for(window))
                {
                    runtime.failures = 0;
                }
            }
            runtime.dirty = false;
            failed
        };
        if failed {
            self.clear_xim_composition();
        }
        self.schedule_xim();
    }
}

#[cfg(test)]
mod tests {
    use super::{XimRuntime, retry_delay, spot_from_bounds};
    use gpui::{Bounds, ScaledPixels, point, size};
    use std::time::Instant;

    #[test]
    fn unfocused_retries_pause_and_focus_churn_does_not_bypass_backoff() {
        let now = Instant::now();
        let mut runtime = XimRuntime::new(now);
        runtime.configured = true;
        assert!(runtime.next_wakeup(now).is_none());
        runtime.focus(Some(100));
        assert_eq!(runtime.next_wakeup(now), Some(now));
        runtime.fail("fixture", now);
        runtime.focus(None);
        assert!(runtime.next_wakeup(now).is_none());
        runtime.focus(Some(100));
        assert_eq!(
            runtime.next_wakeup(now),
            Some(now + Duration::from_millis(500))
        );
    }

    #[test]
    fn spot_coordinates_preserve_sign_and_saturate_instead_of_wrapping() {
        let bounds = Bounds {
            origin: point(ScaledPixels(-20.5), ScaledPixels(40000.)),
            size: size(ScaledPixels(1.), ScaledPixels(24.)),
        };
        let spot = spot_from_bounds(bounds);
        assert_eq!((spot.x, spot.y), (-20, i16::MAX));
    }

    use std::time::Duration;

    #[test]
    fn reconnect_backoff_remains_bounded() {
        let expected = [500, 1000, 2000, 5000, 5000];
        for (failures, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                retry_delay(failures as u32),
                Duration::from_millis(expected)
            );
        }
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(5));
    }
}
