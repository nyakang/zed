use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use x11rb::protocol::{Event, xproto};
use xim::{
    AHashMap, AttributeName, Client, ClientCore, ClientError, ClientHandler, InputStyle, Request,
};

pub(super) const XIM_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub enum XimCallbackEvent {
    XimXEvent(Event),
    XimPreeditEvent(xproto::Window, String),
    XimCommitEvent(xproto::Window, String),
}

#[derive(Default)]
struct InputContext {
    id: Option<u16>,
    spot: Option<(i16, i16)>,
    sent_spot: Option<(i16, i16)>,
    reset: bool,
    preedit: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingRequest {
    Create(xproto::Window),
    Destroy(u16),
    Reset(xproto::Window, u16),
    Position(xproto::Window, u16, (i16, i16)),
}

pub struct XimHandler {
    im_id: Option<u16>,
    style: Option<InputStyle>,
    windows: BTreeMap<xproto::Window, InputContext>,
    focused: Option<xproto::Window>,
    server_focus: Option<(xproto::Window, u16)>,
    retired: VecDeque<u16>,
    pending: Option<(PendingRequest, Instant)>,
    handshake_deadline: Option<Instant>,
    pub(super) disconnected: bool,
    pub(super) callbacks: VecDeque<XimCallbackEvent>,
}

impl XimHandler {
    pub fn new() -> Self {
        Self {
            im_id: None,
            style: None,
            windows: BTreeMap::new(),
            focused: None,
            server_focus: None,
            retired: VecDeque::new(),
            pending: None,
            handshake_deadline: None,
            disconnected: false,
            callbacks: VecDeque::new(),
        }
    }

    pub(super) fn begin_connection(&mut self, now: Instant) {
        self.clear_connection();
        self.handshake_deadline = Some(now + XIM_REQUEST_TIMEOUT);
    }

    pub(super) fn clear_connection(&mut self) {
        self.im_id = None;
        self.style = None;
        self.server_focus = None;
        self.retired.clear();
        self.pending = None;
        self.handshake_deadline = None;
        self.disconnected = false;
        self.callbacks.clear();
        for context in self.windows.values_mut() {
            context.id = None;
            context.sent_spot = None;
            context.reset = false;
            context.preedit.clear();
        }
    }

    pub(super) fn set_focus(&mut self, window: Option<xproto::Window>) {
        if self.focused == window {
            return;
        }
        if let Some(context) = self
            .focused
            .and_then(|window| self.windows.get_mut(&window))
        {
            context.reset = context.id.is_some();
            context.preedit.clear();
            context.sent_spot = None;
        }
        self.focused = window;
        if let Some(window) = window {
            self.windows.entry(window).or_default().sent_spot = None;
        }
    }

    pub(super) fn remove_window(&mut self, window: xproto::Window) {
        if self.focused == Some(window) {
            self.set_focus(None);
        }
        if let Some(id) = self.windows.remove(&window).and_then(|context| context.id) {
            self.retired.push_back(id);
        }
    }

    pub(super) fn set_spot(&mut self, window: xproto::Window, spot: xim::Point) {
        self.windows.entry(window).or_default().spot = Some((spot.x, spot.y));
    }

    pub(super) fn reset(&mut self) {
        if let Some(context) = self
            .focused
            .and_then(|window| self.windows.get_mut(&window))
        {
            context.reset = context.id.is_some();
            context.preedit.clear();
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending
            .map(|(_, sent)| sent + XIM_REQUEST_TIMEOUT)
            .or(self.handshake_deadline)
    }

    pub(super) fn focused_window(&self) -> Option<xproto::Window> {
        self.focused
    }

    pub(super) fn timed_out(&self, now: Instant) -> bool {
        self.handshake_deadline
            .is_some_and(|deadline| now >= deadline)
            || self
                .pending
                .is_some_and(|(_, sent)| now.saturating_duration_since(sent) >= XIM_REQUEST_TIMEOUT)
    }

    pub(super) fn ready_for(&self, window: xproto::Window) -> bool {
        self.focused == Some(window)
            && self
                .server_focus
                .is_some_and(|(focused, _)| focused == window)
            && self
                .windows
                .get(&window)
                .is_some_and(|context| !context.reset)
    }

    fn callback_window(&self, input_method: u16, input_context: u16) -> Option<xproto::Window> {
        let (window, context) = self.server_focus?;
        (self.im_id == Some(input_method) && context == input_context && self.ready_for(window))
            .then_some(window)
    }

    // CreateIcReply has no originating window. Keep at most one management
    // request in flight, including creation for windows closed before the reply.
    pub(super) fn drive<C: Client>(
        &mut self,
        client: &mut C,
        now: Instant,
    ) -> Result<(), ClientError> {
        let (Some(input_method), Some(style)) = (self.im_id, self.style) else {
            return Ok(());
        };
        if let Some((window, context)) = self.server_focus
            && self.focused != Some(window)
        {
            client.unset_focus(input_method, context)?;
            self.server_focus = None;
        }
        if self.pending.is_some() {
            return Ok(());
        }
        if let Some(context) = self.retired.pop_front() {
            client.destroy_ic(input_method, context)?;
            self.pending = Some((PendingRequest::Destroy(context), now));
            return Ok(());
        }
        if let Some((&window, context)) = self.windows.iter_mut().find(|(_, context)| context.reset)
            && let Some(id) = context.id
        {
            client.reset_ic(input_method, id)?;
            self.pending = Some((PendingRequest::Reset(window, id), now));
            return Ok(());
        }
        let Some(window) = self.focused else {
            return Ok(());
        };
        let Some(context) = self.windows.get_mut(&window) else {
            return Ok(());
        };
        let Some(id) = context.id else {
            let mut attributes = client
                .build_ic_attributes()
                .push(AttributeName::InputStyle, style)
                .push(AttributeName::ClientWindow, window)
                .push(AttributeName::FocusWindow, window);
            if let Some(spot) = context.spot
                && style.intersects(InputStyle::PREEDIT_CALLBACKS | InputStyle::PREEDIT_POSITION)
            {
                attributes = attributes.nested_list(AttributeName::PreeditAttributes, |builder| {
                    builder.push(
                        AttributeName::SpotLocation,
                        xim::Point {
                            x: spot.0,
                            y: spot.1,
                        },
                    );
                });
            }
            client.create_ic(input_method, attributes.build())?;
            self.pending = Some((PendingRequest::Create(window), now));
            return Ok(());
        };
        if self.server_focus != Some((window, id)) {
            client.set_focus(input_method, id)?;
            self.server_focus = Some((window, id));
        }
        if let Some(spot) = context.spot
            && style.intersects(InputStyle::PREEDIT_CALLBACKS | InputStyle::PREEDIT_POSITION)
            && context.sent_spot != Some(spot)
        {
            let attributes = client
                .build_ic_attributes()
                .nested_list(AttributeName::PreeditAttributes, |builder| {
                    builder.push(
                        AttributeName::SpotLocation,
                        xim::Point {
                            x: spot.0,
                            y: spot.1,
                        },
                    );
                })
                .build();
            client.set_ic_values(input_method, id, attributes)?;
            self.pending = Some((PendingRequest::Position(window, id, spot), now));
        }
        Ok(())
    }

    pub(super) fn forward<C: Client<XEvent = xproto::KeyPressEvent>>(
        &self,
        client: &mut C,
        event: &xproto::KeyPressEvent,
    ) -> Result<bool, ClientError> {
        if !self.ready_for(event.event) {
            return Ok(false);
        }
        if let (Some(input_method), Some((_, context))) = (self.im_id, self.server_focus) {
            client.forward_event(input_method, context, xim::ForwardEventFlag::empty(), event)?;
            return Ok(true);
        }
        Ok(false)
    }
}

impl<C: Client<XEvent = xproto::KeyPressEvent> + ClientCore<XEvent = xproto::KeyPressEvent>>
    ClientHandler<C> for XimHandler
{
    fn handle_connect(&mut self, client: &mut C) -> Result<(), ClientError> {
        client.open("C")
    }

    fn handle_open(&mut self, client: &mut C, input_method_id: u16) -> Result<(), ClientError> {
        if input_method_id == 0 {
            return Err(ClientError::InvalidReply);
        }
        self.im_id = Some(input_method_id);
        client.get_im_values(input_method_id, &[AttributeName::QueryInputStyle])
    }

    fn handle_get_im_values(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        attributes: AHashMap<AttributeName, Vec<u8>>,
    ) -> Result<(), ClientError> {
        if self.im_id != Some(input_method_id) {
            return Err(ClientError::InvalidReply);
        }
        let values = attributes
            .get(&AttributeName::QueryInputStyle)
            .ok_or(ClientError::InvalidReply)?;
        let styles: xim::InputStyleList = xim::read(values)?;
        self.style = [
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NONE,
            // Fcitx's default XIM configuration advertises server-rendered
            // preedit only. Requesting callbacks anyway is a protocol error.
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NONE,
            InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NOTHING,
            InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NONE,
        ]
        .into_iter()
        .find(|style| styles.styles.contains(style));
        self.handshake_deadline = None;
        if self.style.is_none() {
            log::warn!("XIM server does not support a usable preedit style with passive status");
        }
        Ok(())
    }

    fn handle_create_ic(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        if self.im_id != Some(input_method_id) || input_context_id == 0 {
            return Err(ClientError::InvalidReply);
        }
        let Some((PendingRequest::Create(window), _)) = self.pending else {
            return Err(ClientError::InvalidReply);
        };
        self.pending = None;
        if let Some(context) = self.windows.get_mut(&window) {
            context.id = Some(input_context_id);
        } else {
            self.retired.push_back(input_context_id);
        }
        Ok(())
    }

    fn handle_destroy_ic(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        if self.im_id == Some(input_method_id)
            && self
                .pending
                .is_some_and(|(request, _)| request == PendingRequest::Destroy(input_context_id))
        {
            self.pending = None;
            Ok(())
        } else {
            Err(ClientError::InvalidReply)
        }
    }

    fn handle_set_ic_values(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        if let Some((PendingRequest::Position(window, id, spot), _)) = self.pending
            && self.im_id == Some(input_method_id)
            && input_context_id == id
        {
            if let Some(context) = self.windows.get_mut(&window) {
                context.sent_spot = Some(spot);
            }
            self.pending = None;
            return Ok(());
        }
        Err(ClientError::InvalidReply)
    }

    fn handle_reset_ic(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
        _text: &str,
    ) -> Result<(), ClientError> {
        if let Some((PendingRequest::Reset(window, id), _)) = self.pending
            && self.im_id == Some(input_method_id)
            && input_context_id == id
        {
            if let Some(context) = self.windows.get_mut(&window) {
                context.reset = false;
            }
            self.pending = None;
        }
        // Reset cancels composition: never submit the server's returned text.
        Ok(())
    }

    fn handle_commit(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
        text: &str,
    ) -> Result<(), ClientError> {
        if let Some(window) = self.callback_window(input_method_id, input_context_id) {
            if let Some(context) = self.windows.get_mut(&window) {
                context.preedit.clear();
            }
            self.callbacks
                .push_back(XimCallbackEvent::XimCommitEvent(window, text.to_owned()));
        }
        Ok(())
    }

    fn handle_forward_event(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
        _flag: xim::ForwardEventFlag,
        event: xproto::KeyPressEvent,
    ) -> Result<(), ClientError> {
        if self.callback_window(input_method_id, input_context_id) != Some(event.event) {
            return Ok(());
        }
        let event = match event.response_type & 0x7f {
            xproto::KEY_PRESS_EVENT => Event::KeyPress(event),
            xproto::KEY_RELEASE_EVENT => Event::KeyRelease(event),
            _ => return Ok(()),
        };
        self.callbacks.push_back(XimCallbackEvent::XimXEvent(event));
        Ok(())
    }

    fn handle_close(&mut self, client: &mut C, _input_method_id: u16) -> Result<(), ClientError> {
        self.disconnected = true;
        client.disconnect()
    }

    fn handle_disconnect(&mut self) {
        self.disconnected = true;
    }

    fn handle_preedit_start(
        &mut self,
        client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        client.send_req(Request::PreeditStartReply {
            input_method_id,
            input_context_id,
            return_value: -1,
        })
    }

    fn handle_preedit_done(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
    ) -> Result<(), ClientError> {
        if let Some(window) = self.callback_window(input_method_id, input_context_id) {
            if let Some(context) = self.windows.get_mut(&window) {
                context.preedit.clear();
            }
            self.callbacks
                .push_back(XimCallbackEvent::XimPreeditEvent(window, String::new()));
        }
        Ok(())
    }

    fn handle_preedit_draw(
        &mut self,
        _client: &mut C,
        input_method_id: u16,
        input_context_id: u16,
        _caret: i32,
        first: i32,
        length: i32,
        status: xim::PreeditDrawStatus,
        text: &str,
        _feedbacks: Vec<xim::Feedback>,
    ) -> Result<(), ClientError> {
        if let Some(window) = self.callback_window(input_method_id, input_context_id)
            && let Some(context) = self.windows.get_mut(&window)
        {
            // XIM updates a range of characters, not bytes or the entire string.
            if !status.contains(xim::PreeditDrawStatus::NO_STRING) {
                let start = context
                    .preedit
                    .char_indices()
                    .nth(first.max(0) as usize)
                    .map_or(context.preedit.len(), |(index, _)| index);
                let end = context.preedit[start..]
                    .char_indices()
                    .nth(length.max(0) as usize)
                    .map_or(context.preedit.len(), |(index, _)| start + index);
                context.preedit.replace_range(start..end, text);
            }
            self.callbacks.push_back(XimCallbackEvent::XimPreeditEvent(
                window,
                context.preedit.clone(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
