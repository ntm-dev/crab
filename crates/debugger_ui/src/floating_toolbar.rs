use gpui::{
    Context, CursorStyle, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Render, WeakEntity, Window, point, px,
};
use ui::{ElevationIndex, prelude::*};
use util::ResultExt as _;

use crate::debugger_panel::DebugPanel;

const DEFAULT_TOP: Pixels = px(40.);
const DEFAULT_WIDTH_ESTIMATE: Pixels = px(300.);
const EDGE_MARGIN: Pixels = px(60.);
const POSITION_KEY: &str = "debug_toolbar_position";

pub(crate) struct DebugToolbar {
    panel: WeakEntity<DebugPanel>,
    position: Option<Point<Pixels>>,
    grab_offset: Option<Point<Pixels>>,
}

impl DebugToolbar {
    pub(crate) fn new(panel: WeakEntity<DebugPanel>, cx: &mut Context<Self>) -> Self {
        if let Some(panel) = panel.upgrade() {
            cx.observe(&panel, |_, _, cx| cx.notify()).detach();
        }
        let position = db::kvp::KeyValueStore::global(cx)
            .read_kvp(POSITION_KEY)
            .ok()
            .flatten()
            .and_then(|value| {
                let (x, y) = value.split_once(',')?;
                Some(point(px(x.parse().ok()?), px(y.parse().ok()?)))
            });
        Self {
            panel,
            position,
            grab_offset: None,
        }
    }

    fn save_position(&self, cx: &mut Context<Self>) {
        let Some(position) = self.position else {
            return;
        };
        let kvp = db::kvp::KeyValueStore::global(cx);
        let value = format!("{},{}", f32::from(position.x), f32::from(position.y));
        cx.background_spawn(async move {
            kvp.write_kvp(POSITION_KEY.to_string(), value).await.log_err();
        })
        .detach();
    }

    fn current_position(&self, window: &Window) -> Point<Pixels> {
        self.position.unwrap_or_else(|| {
            let viewport = window.viewport_size();
            point(
                ((viewport.width - DEFAULT_WIDTH_ESTIMATE) / 2.).max(px(0.)),
                DEFAULT_TOP,
            )
        })
    }
}

impl Render for DebugToolbar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(panel) = self.panel.upgrade() else {
            return div().into_any_element();
        };
        if panel.read(cx).active_session().is_none() {
            return div().into_any_element();
        }

        let controls = panel.update(cx, |panel, cx| panel.render_execution_controls(window, cx));
        let viewport = window.viewport_size();
        let position = self.current_position(window);
        let dragging = self.grab_offset.is_some();
        let colors = cx.theme().colors();

        let grip = h_flex()
            .id("debug-toolbar-grip")
            .h_full()
            .px_1()
            .gap_0p5()
            .cursor(if dragging {
                CursorStyle::ClosedHand
            } else {
                CursorStyle::OpenHand
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    let position = this.current_position(window);
                    this.position = Some(position);
                    this.grab_offset = Some(event.position - position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .children((0..2).map(|_| {
                v_flex().gap_0p5().children((0..3).map(|_| {
                    div()
                        .size(px(3.))
                        .rounded_full()
                        .bg(colors.text_muted.opacity(0.7))
                }))
            }));

        let toolbar = h_flex()
            .id("debug-toolbar")
            .absolute()
            .left(position.x)
            .top(position.y)
            .occlude()
            .py_0p5()
            .pr_1()
            .gap_0p5()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(ElevationIndex::ElevatedSurface.bg(cx))
            .shadow_md()
            .child(grip)
            .child(controls);

        let drag_surface = dragging.then(|| {
            div()
                .id("debug-toolbar-drag-surface")
                .absolute()
                .top_0()
                .left_0()
                .w(viewport.width)
                .h(viewport.height)
                .occlude()
                .cursor(CursorStyle::ClosedHand)
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    let Some(grab_offset) = this.grab_offset else {
                        return;
                    };
                    let viewport = window.viewport_size();
                    let target = event.position - grab_offset;
                    this.position = Some(point(
                        target.x.clamp(px(0.), (viewport.width - EDGE_MARGIN).max(px(0.))),
                        target.y.clamp(px(0.), (viewport.height - EDGE_MARGIN).max(px(0.))),
                    ));
                    cx.notify();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.grab_offset = None;
                        this.save_position(cx);
                        cx.notify();
                    }),
                )
        });

        div()
            .absolute()
            .top_0()
            .left_0()
            .child(toolbar)
            .children(drag_surface)
            .into_any_element()
    }
}
