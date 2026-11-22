//! Native pointer-lock acceptance probe. Click to acquire, Shift+F12 to release.
//! Run on a disposable X11/Wayland display for synthetic input automation.
use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, PointerLockEvent, Render, Window, WindowBounds, WindowOptions, canvas, div,
    prelude::*, px, size,
};
use std::{cell::RefCell, rc::Rc, time::Duration};

#[derive(Default)]
struct Results {
    acquired: usize,
    released: usize,
    motions: usize,
    x: f32,
    y: f32,
}

struct Probe {
    focus: FocusHandle,
    results: Rc<RefCell<Results>>,
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let results = self.results.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    match window.set_pointer_lock(true) {
                        Ok(()) => this.results.borrow_mut().acquired += 1,
                        Err(error) => eprintln!("pointer lock failed: {error}"),
                    }
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, _| {
                if event.keystroke.key == "f12" && event.keystroke.modifiers.shift {
                    match window.set_pointer_lock(false) {
                        Ok(()) => this.results.borrow_mut().released += 1,
                        Err(error) => eprintln!("pointer release failed: {error}"),
                    }
                }
            }))
            .child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        window.on_mouse_event(move |event: &PointerLockEvent, phase, _, _| {
                            if !phase.capture() {
                                return;
                            }
                            let mut results = results.borrow_mut();
                            match event {
                                PointerLockEvent::Motion { delta } => {
                                    results.motions += 1;
                                    results.x += delta.x;
                                    results.y += delta.y;
                                }
                                PointerLockEvent::Unlocked => results.released += 1,
                                PointerLockEvent::Locked => {}
                            }
                        });
                    },
                )
                .size_full(),
            )
    }
}

fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<u64>().ok())
        .unwrap_or(30)
        .clamp(1, 120);
    gpui_platform::application().run(move |cx: &mut App| {
        let results = Rc::new(RefCell::new(Results::default()));
        let observed = results.clone();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(320.), px(240.)),
                        cx,
                    ))),
                    show: true,
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("GPUI pointer lock probe");
                    cx.new(|cx| Probe {
                        focus: cx.focus_handle(),
                        results,
                    })
                },
            )
            .expect("probe window");
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_secs(seconds))
                .await;
            let results = observed.borrow();
            println!(
                "acquired={} released={} motions={} dx={} dy={}",
                results.acquired, results.released, results.motions, results.x, results.y
            );
            drop(results);
            if let Err(error) = window.update(cx, |_, window, _| {
                if let Err(error) = window.set_pointer_lock(false) {
                    eprintln!("pointer release failed: {error}");
                }
            }) {
                eprintln!("probe window closed: {error}");
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
