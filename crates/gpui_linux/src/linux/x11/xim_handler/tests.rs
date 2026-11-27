use std::time::{Duration, Instant};
use x11rb::protocol::xproto;
use xim::{
    AHashMap, Attr, AttributeName, ClientCore, ClientError, ClientHandler, InputStyle, Request,
};

use super::{XimCallbackEvent, XimHandler};

#[derive(Default)]
struct TestClient {
    attributes: AHashMap<AttributeName, u16>,
    requests: Vec<Request>,
}

impl ClientCore for TestClient {
    type XEvent = xproto::KeyPressEvent;
    fn set_attrs(&mut self, _: Vec<Attr>, _: Vec<Attr>) {}
    fn ic_attributes(&self) -> &AHashMap<AttributeName, u16> {
        &self.attributes
    }
    fn im_attributes(&self) -> &AHashMap<AttributeName, u16> {
        &self.attributes
    }
    fn serialize_event(&self, _: &Self::XEvent) -> xim::XEvent {
        panic!("fixture does not serialize keys")
    }
    fn deserialize_event(&self, _: &xim::XEvent) -> Self::XEvent {
        panic!("fixture does not deserialize keys")
    }
    fn send_req(&mut self, request: Request) -> Result<(), ClientError> {
        self.requests.push(request);
        Ok(())
    }
}

fn handshake(handler: &mut XimHandler, client: &mut TestClient, now: Instant) {
    handler.begin_connection(now);
    handler.handle_connect(client).unwrap();
    handler.handle_open(client, 7).unwrap();
    let mut attributes = AHashMap::default();
    attributes.insert(
        AttributeName::QueryInputStyle,
        xim::write_to_vec(xim::InputStyleList {
            styles: vec![InputStyle::PREEDIT_CALLBACKS | InputStyle::STATUS_NOTHING],
        }),
    );
    handler.handle_get_im_values(client, 7, attributes).unwrap();
    for (index, name) in [
        AttributeName::InputStyle,
        AttributeName::ClientWindow,
        AttributeName::FocusWindow,
        AttributeName::PreeditAttributes,
        AttributeName::SpotLocation,
    ]
    .into_iter()
    .enumerate()
    {
        client.attributes.insert(name, index as u16 + 1);
    }
}

fn focused() -> (XimHandler, TestClient, Instant) {
    let now = Instant::now();
    let mut handler = XimHandler::new();
    let mut client = TestClient::default();
    handler.set_focus(Some(100));
    handshake(&mut handler, &mut client, now);
    handler.drive(&mut client, now).unwrap();
    handler.handle_create_ic(&mut client, 7, 10).unwrap();
    handler.drive(&mut client, now).unwrap();
    (handler, client, now)
}

#[test]
fn focus_during_handshake_never_creates_an_uninitialized_context() {
    let mut handler = XimHandler::new();
    let mut client = TestClient::default();
    let now = Instant::now();
    handler.begin_connection(now);
    handler.set_focus(Some(100));
    handler.drive(&mut client, now).unwrap();
    assert!(client.requests.is_empty());
    assert!(!handler.ready_for(100));
    assert!(!handler.timed_out(now + Duration::from_secs(4)));
    assert!(handler.timed_out(now + Duration::from_secs(5)));
}

#[test]
fn repeated_focus_reuses_context_and_sends_balanced_focus_notifications() {
    let (mut handler, mut client, now) = focused();
    for _ in 0..100 {
        handler.set_focus(Some(100));
        handler.drive(&mut client, now).unwrap();
        handler.set_focus(None);
        handler.drive(&mut client, now).unwrap();
        assert!(!handler.ready_for(100));
        handler
            .handle_reset_ic(&mut client, 7, 10, "cancelled synthetic input")
            .unwrap();
        handler.set_focus(Some(100));
        handler.drive(&mut client, now).unwrap();
        assert!(handler.ready_for(100));
    }
    assert_eq!(
        client
            .requests
            .iter()
            .filter(|request| matches!(request, Request::CreateIc { .. }))
            .count(),
        1
    );
    assert_eq!(
        client
            .requests
            .iter()
            .filter(|request| matches!(request, Request::SetIcFocus { .. }))
            .count(),
        101
    );
    assert_eq!(
        client
            .requests
            .iter()
            .filter(|request| matches!(request, Request::UnsetIcFocus { .. }))
            .count(),
        100
    );
    assert!(handler.callbacks.is_empty());
}

#[test]
fn closing_window_during_creation_destroys_late_context_before_creating_another() {
    let mut handler = XimHandler::new();
    let mut client = TestClient::default();
    let now = Instant::now();
    handler.set_focus(Some(100));
    handshake(&mut handler, &mut client, now);
    handler.drive(&mut client, now).unwrap();
    handler.remove_window(100);
    handler.set_focus(Some(200));
    handler.drive(&mut client, now).unwrap();
    handler.handle_create_ic(&mut client, 7, 10).unwrap();
    handler.drive(&mut client, now).unwrap();
    assert!(matches!(
        client.requests.last(),
        Some(Request::DestroyIc {
            input_context_id: 10,
            ..
        })
    ));
    handler.handle_destroy_ic(&mut client, 7, 10).unwrap();
    handler.drive(&mut client, now).unwrap();
    handler.handle_create_ic(&mut client, 7, 20).unwrap();
    handler.drive(&mut client, now).unwrap();
    assert!(handler.ready_for(200));
    assert!(!handler.ready_for(100));
}

#[test]
fn old_and_unfocused_contexts_cannot_deliver_text() {
    let (mut handler, mut client, now) = focused();
    handler
        .handle_commit(&mut client, 99, 10, "wrong method")
        .unwrap();
    handler
        .handle_commit(&mut client, 7, 99, "wrong context")
        .unwrap();
    handler.set_focus(None);
    handler
        .handle_commit(&mut client, 7, 10, "late commit")
        .unwrap();
    assert!(handler.callbacks.is_empty());
    handler.begin_connection(now);
    handler
        .handle_commit(&mut client, 7, 10, "old connection")
        .unwrap();
    assert!(handler.callbacks.is_empty());
}

#[test]
fn position_acknowledgement_coalesces_updates_without_changing_input_style() {
    let (mut handler, mut client, now) = focused();
    handler.set_spot(100, xim::Point { x: -5, y: 80 });
    handler.drive(&mut client, now).unwrap();
    handler.set_spot(100, xim::Point { x: 30, y: 90 });
    handler.drive(&mut client, now).unwrap();
    let position_requests = |client: &TestClient| {
        client
            .requests
            .iter()
            .filter(|request| matches!(request, Request::SetIcValues { .. }))
            .count()
    };
    assert_eq!(position_requests(&client), 1);
    assert!(handler.timed_out(now + Duration::from_secs(5)));
    handler.handle_set_ic_values(&mut client, 7, 10).unwrap();
    handler.drive(&mut client, now).unwrap();
    assert_eq!(position_requests(&client), 2);
    if let Some(Request::SetIcValues { ic_attributes, .. }) = client.requests.last() {
        assert_eq!(ic_attributes.len(), 1);
        assert_eq!(
            ic_attributes[0].id,
            client.attributes[&AttributeName::PreeditAttributes]
        );
    } else {
        panic!("expected position request");
    }
    handler.handle_set_ic_values(&mut client, 7, 10).unwrap();
    handler.drive(&mut client, now).unwrap();
    assert_eq!(position_requests(&client), 2);
}

#[test]
fn reconnect_recreates_context_and_resends_latest_position() {
    let (mut handler, mut client, now) = focused();
    handler.set_spot(100, xim::Point { x: 12, y: 34 });
    handler.drive(&mut client, now).unwrap();
    handshake(&mut handler, &mut client, now);
    assert!(!handler.ready_for(100));
    handler.drive(&mut client, now).unwrap();
    handler.handle_create_ic(&mut client, 7, 11).unwrap();
    handler.drive(&mut client, now).unwrap();
    assert!(handler.ready_for(100));
    assert!(matches!(
        client.requests.last(),
        Some(Request::SetIcValues {
            input_context_id: 11,
            ..
        })
    ));
}

#[test]
fn preedit_start_is_acknowledged_and_unicode_edits_are_applied_by_character() {
    let (mut handler, mut client, _) = focused();
    handler.handle_preedit_start(&mut client, 7, 10).unwrap();
    assert!(matches!(
        client.requests.last(),
        Some(Request::PreeditStartReply {
            return_value: -1,
            ..
        })
    ));
    handler
        .handle_preedit_draw(
            &mut client,
            7,
            10,
            0,
            0,
            0,
            xim::PreeditDrawStatus::empty(),
            "中a",
            vec![],
        )
        .unwrap();
    handler
        .handle_preedit_draw(
            &mut client,
            7,
            10,
            0,
            1,
            1,
            xim::PreeditDrawStatus::empty(),
            "文",
            vec![],
        )
        .unwrap();
    assert!(
        matches!(handler.callbacks.back(), Some(XimCallbackEvent::XimPreeditEvent(100, text)) if text == "中文")
    );
    handler.handle_preedit_done(&mut client, 7, 10).unwrap();
    assert!(
        matches!(handler.callbacks.back(), Some(XimCallbackEvent::XimPreeditEvent(100, text)) if text.is_empty())
    );
}

#[test]
fn server_rendered_preedit_is_negotiated_when_callbacks_are_not_advertised() {
    let mut handler = XimHandler::new();
    let mut client = TestClient::default();
    let now = Instant::now();
    handler.set_focus(Some(100));
    handshake(&mut handler, &mut client, now);
    let style = InputStyle::PREEDIT_POSITION | InputStyle::STATUS_NOTHING;
    let mut attributes = AHashMap::default();
    attributes.insert(
        AttributeName::QueryInputStyle,
        xim::write_to_vec(xim::InputStyleList {
            styles: vec![style],
        }),
    );
    handler
        .handle_get_im_values(&mut client, 7, attributes)
        .unwrap();
    handler.drive(&mut client, now).unwrap();
    match client.requests.last() {
        Some(Request::CreateIc { ic_attributes, .. }) => {
            let attribute = ic_attributes
                .iter()
                .find(|attribute| attribute.id == client.attributes[&AttributeName::InputStyle])
                .unwrap();
            assert_eq!(xim::read::<InputStyle>(&attribute.value).unwrap(), style);
        }
        _ => panic!("expected a context using the advertised style"),
    }
}
