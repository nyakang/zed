//! Disposable native IME acceptance probe. Only aggregate counters are printed.
//! Drive with synthetic input on an isolated display; never use real credentials.
use gpui::{
    App, AppContext, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    IntoElement, Pixels, Point, Render, UTF16Selection, Window, WindowBounds, WindowOptions,
    canvas, div, point, prelude::*, px, size,
};
use std::{ops::Range, time::Duration};

struct ProbeLogger;
static LOGGER: ProbeLogger = ProbeLogger;
impl log::Log for ProbeLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Debug
            && (metadata.target().contains("xim") || metadata.target().contains("gpui_linux"))
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

struct Probe {
    focus: FocusHandle,
    id: usize,
    marked: String,
    committed: usize,
    chinese: usize,
    preedits: usize,
    anchor: Point<Pixels>,
}

impl EntityInputHandler for Probe {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        *adjusted = Some(0..self.marked.encode_utf16().count());
        Some(self.marked.clone())
    }
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }
    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked.clear();
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked.clear();
        self.committed += text.chars().count();
        self.chinese += text
            .chars()
            .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
            .count();
        println!(
            "IME_PROBE id={} committed={} chinese={} preedits={}",
            self.id, self.committed, self.chinese, self.preedits
        );
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = text.to_owned();
        self.preedits += 1;
        window.invalidate_character_coordinates();
        cx.notify();
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds {
            origin: bounds.origin + self.anchor,
            size: size(px(8.), px(24.)),
        })
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let focus = self.focus.clone();
        div()
            .size_full()
            .bg(gpui::rgb(0x202020))
            .text_color(gpui::white())
            .track_focus(&self.focus)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.anchor = event.position;
                    window.focus(&this.focus, cx);
                    window.invalidate_character_coordinates();
                    cx.notify();
                }),
            )
            .child(format!(
                "Synthetic IME probe {}: committed={}, CJK={}",
                self.id, self.committed, self.chinese
            ))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
    }
}

fn main() {
    log::set_logger(&LOGGER).expect("probe logger");
    log::set_max_level(log::LevelFilter::Debug);
    let seconds = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse::<u64>().ok())
        .unwrap_or(180)
        .clamp(1, 600);
    gpui_platform::application().run(move |cx: &mut App| {
        for id in 0..2 {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(60. + id as f32 * 520.), px(100.)),
                        size: size(px(480.), px(320.)),
                    })),
                    show: true,
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title(&format!("GPUI XIM probe {id}"));
                    let entity = cx.new(|cx| Probe {
                        focus: cx.focus_handle(),
                        id,
                        marked: String::new(),
                        committed: 0,
                        chinese: 0,
                        preedits: 0,
                        anchor: point(px(80.), px(80.)),
                    });
                    let focus = entity.read(cx).focus.clone();
                    window.focus(&focus, cx);
                    entity
                },
            )
            .expect("native probe window");
        }
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_secs(seconds))
                .await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
