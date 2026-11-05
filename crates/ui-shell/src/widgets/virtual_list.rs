use iced::advanced::layout;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::operation::scrollable::{AbsoluteOffset, RelativeOffset, Scrollable};
use iced::advanced::widget::{Id, Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{
    mouse, touch, Background, Color, Element, Event, Length, Point, Rectangle, Size, Vector,
};
use std::collections::HashMap;

type Row<'a, Message, Theme, Renderer> = (u64, f32, Element<'a, Message, Theme, Renderer>);
type RowParts<'a, Message, Theme, Renderer> = (
    Vec<u64>,
    Vec<f32>,
    Vec<Element<'a, Message, Theme, Renderer>>,
);
type WindowBuilder<'a, Message, Theme, Renderer> =
    dyn Fn(f32, f32) -> Vec<Row<'a, Message, Theme, Renderer>> + 'a;
type ScrollHandler<'a, Message> = dyn Fn(f32, f32) -> Message + 'a;

/// A vertically virtualized container.
///
/// The lazy constructor owns the scroll state and asks its window builder for
/// only the rows intersecting the current viewport (plus caller-defined
/// overscan). This keeps scrolling and relayout in the widget tree instead of
/// waiting for an application message before newly visible rows exist.
pub struct VirtualList<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    offsets: Vec<f32>,
    keys: Vec<u64>,
    total_height: f32,
    id: Option<Id>,
    initial_offset_y: f32,
    initial_viewport_height: f32,
    window_builder: Option<Box<WindowBuilder<'a, Message, Theme, Renderer>>>,
    on_scroll: Option<Box<ScrollHandler<'a, Message>>>,
}

impl<'a, Message, Theme, Renderer> VirtualList<'a, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    // Test-only: product code always builds the list lazily through `lazy`.
    #[cfg(test)]
    pub fn new_keyed(
        children: impl IntoIterator<Item = Row<'a, Message, Theme, Renderer>>,
        total_height: f32,
    ) -> Self {
        let (keys, offsets, elements) = unzip_rows(children);
        Self::eager(elements, offsets, keys, total_height)
    }

    #[cfg(test)]
    fn eager(
        children: Vec<Element<'a, Message, Theme, Renderer>>,
        offsets: Vec<f32>,
        keys: Vec<u64>,
        total_height: f32,
    ) -> Self {
        validate_rows(&keys, &offsets);
        let maximum_offset = offsets.iter().copied().fold(0.0, f32::max);
        Self {
            children,
            offsets,
            keys,
            total_height: total_height.max(maximum_offset).max(0.0),
            id: None,
            initial_offset_y: 0.0,
            initial_viewport_height: 0.0,
            window_builder: None,
            on_scroll: None,
        }
    }

    pub fn lazy(
        id: Id,
        initial_offset_y: f32,
        initial_viewport_height: f32,
        total_height: f32,
        window_builder: impl Fn(f32, f32) -> Vec<Row<'a, Message, Theme, Renderer>> + 'a,
    ) -> Self {
        let initial_offset_y = finite_non_negative(initial_offset_y);
        let initial_viewport_height = finite_non_negative(initial_viewport_height);
        let total_height = finite_non_negative(total_height);
        let initial_rows = window_builder(initial_offset_y, initial_viewport_height);
        let (keys, offsets, children) = unzip_rows(initial_rows);
        validate_rows(&keys, &offsets);

        Self {
            children,
            offsets,
            keys,
            total_height,
            id: Some(id),
            initial_offset_y,
            initial_viewport_height,
            window_builder: Some(Box::new(window_builder)),
            on_scroll: None,
        }
    }

    pub fn on_scroll(mut self, handler: impl Fn(f32, f32) -> Message + 'a) -> Self {
        self.on_scroll = Some(Box::new(handler));
        self
    }

    fn rebuild_window(&mut self, tree: &mut Tree, offset_y: f32, viewport_height: f32) {
        let Some(builder) = self.window_builder.as_ref() else {
            return;
        };
        let (keys, offsets, children) = unzip_rows(builder(offset_y, viewport_height));
        validate_rows(&keys, &offsets);
        self.keys = keys;
        self.offsets = offsets;
        self.children = children;
        reconcile_children(&self.keys, &self.children, tree);

        let state = tree.state.downcast_mut::<VirtualTreeState>();
        state.built_offset_y = offset_y;
        state.built_viewport_height = viewport_height;
    }

    fn is_lazy(&self) -> bool {
        self.window_builder.is_some()
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn unzip_rows<'a, Message, Theme, Renderer>(
    rows: impl IntoIterator<Item = Row<'a, Message, Theme, Renderer>>,
) -> RowParts<'a, Message, Theme, Renderer> {
    let mut keys = Vec::new();
    let mut offsets = Vec::new();
    let mut elements = Vec::new();
    for (key, offset, element) in rows {
        keys.push(key);
        offsets.push(finite_non_negative(offset));
        elements.push(element);
    }
    (keys, offsets, elements)
}

fn validate_rows(keys: &[u64], offsets: &[f32]) {
    debug_assert_eq!(keys.len(), offsets.len());
    debug_assert!(offsets.windows(2).all(|pair| pair[1] >= pair[0]));
    debug_assert!({
        let mut unique = keys.to_vec();
        unique.sort_unstable();
        unique.dedup();
        unique.len() == keys.len()
    });
}

#[derive(Debug)]
struct VirtualTreeState {
    keys: Vec<u64>,
    offset_y: f32,
    viewport_height: f32,
    total_height: f32,
    built_offset_y: f32,
    built_viewport_height: f32,
    last_touch_y: Option<f32>,
    scrollbar_drag_offset: Option<f32>,
}

impl VirtualTreeState {
    fn max_offset(&self) -> f32 {
        (self.total_height - self.viewport_height).max(0.0)
    }

    fn clamp_offset(&mut self) {
        self.offset_y = finite_non_negative(self.offset_y).min(self.max_offset());
    }

    fn set_offset(&mut self, offset_y: f32) -> bool {
        let previous = self.offset_y;
        self.offset_y = finite_non_negative(offset_y).min(self.max_offset());
        (previous - self.offset_y).abs() > f32::EPSILON
    }
}

impl Scrollable for VirtualTreeState {
    fn snap_to(&mut self, offset: RelativeOffset<Option<f32>>) {
        if let Some(y) = offset.y {
            self.offset_y = y.clamp(0.0, 1.0) * self.max_offset();
        }
    }

    fn scroll_to(&mut self, offset: AbsoluteOffset<Option<f32>>) {
        if let Some(y) = offset.y {
            self.set_offset(y);
        }
    }

    fn scroll_by(
        &mut self,
        offset: AbsoluteOffset,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
    ) {
        self.set_offset(self.offset_y + offset.y);
    }
}

fn reconcile_children<Message, Theme, Renderer: iced::advanced::Renderer>(
    keys: &[u64],
    children: &[Element<'_, Message, Theme, Renderer>],
    tree: &mut Tree,
) {
    let state = tree.state.downcast_mut::<VirtualTreeState>();
    let old_keys = std::mem::take(&mut state.keys);
    let old_trees = std::mem::take(&mut tree.children);
    let mut by_key = old_keys
        .into_iter()
        .zip(old_trees)
        .collect::<HashMap<_, _>>();
    tree.children = keys
        .iter()
        .zip(children)
        .map(|(key, child)| by_key.remove(key).unwrap_or_else(|| Tree::new(child)))
        .collect();
    for (child, child_tree) in children.iter().zip(&mut tree.children) {
        child_tree.diff(child.as_widget());
    }
    state.keys = keys.to_vec();
}

fn scrollbar_geometry(
    bounds: Rectangle,
    state: &VirtualTreeState,
) -> Option<(Rectangle, Rectangle)> {
    if state.total_height <= state.viewport_height || bounds.height <= 0.0 {
        return None;
    }
    const WIDTH: f32 = 8.0;
    const MARGIN: f32 = 1.0;
    const MIN_THUMB: f32 = 24.0;
    let track = Rectangle {
        x: bounds.x + bounds.width - WIDTH - MARGIN,
        y: bounds.y + MARGIN,
        width: WIDTH,
        height: (bounds.height - MARGIN * 2.0).max(0.0),
    };
    let thumb_height = (track.height * state.viewport_height / state.total_height)
        .clamp(MIN_THUMB.min(track.height), track.height);
    let travel = (track.height - thumb_height).max(0.0);
    let thumb_y = if state.max_offset() > 0.0 {
        track.y + travel * state.offset_y / state.max_offset()
    } else {
        track.y
    };
    Some((
        track,
        Rectangle {
            x: track.x,
            y: thumb_y,
            width: track.width,
            height: thumb_height,
        },
    ))
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for VirtualList<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<VirtualTreeState>()
    }

    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(VirtualTreeState {
            keys: self.keys.clone(),
            offset_y: self.initial_offset_y,
            viewport_height: self.initial_viewport_height,
            total_height: self.total_height,
            built_offset_y: self.initial_offset_y,
            built_viewport_height: self.initial_viewport_height,
            last_touch_y: None,
            scrollbar_drag_offset: None,
        })
    }

    fn diff(&self, tree: &mut Tree) {
        reconcile_children(&self.keys, &self.children, tree);
        let state = tree.state.downcast_mut::<VirtualTreeState>();
        state.total_height = self.total_height;
        state.clamp_offset();
    }

    fn size(&self) -> Size<Length> {
        if self.is_lazy() {
            Size::new(Length::Fill, Length::Fill)
        } else {
            Size::new(Length::Fill, Length::Fixed(self.total_height))
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let lazy = self.is_lazy();
        let size = if lazy {
            limits.resolve(Length::Fill, Length::Fill, Size::ZERO)
        } else {
            Size::new(limits.max().width, self.total_height)
        };

        let (offset_y, rebuild) = {
            let state = tree.state.downcast_mut::<VirtualTreeState>();
            state.viewport_height = size.height;
            state.total_height = self.total_height;
            state.clamp_offset();
            let rebuild = lazy
                && ((state.built_offset_y - state.offset_y).abs() > f32::EPSILON
                    || (state.built_viewport_height - size.height).abs() > f32::EPSILON);
            (state.offset_y, rebuild)
        };
        if rebuild {
            self.rebuild_window(tree, offset_y, size.height);
        }

        let width = size.width;
        let child_limits =
            layout::Limits::new(Size::new(width, 0.0), Size::new(width, f32::INFINITY));
        let children = self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(&self.offsets)
            .map(|((child, child_tree), offset)| {
                child
                    .as_widget_mut()
                    .layout(child_tree, renderer, &child_limits)
                    .move_to(Point::new(0.0, *offset - offset_y))
            })
            .collect();
        layout::Node::with_children(size, children)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<VirtualTreeState>();
        operation.scrollable(
            self.id.as_ref(),
            bounds,
            Rectangle {
                height: self.total_height,
                ..bounds
            },
            Vector::new(0.0, state.offset_y),
            state,
        );
        operation.traverse(&mut |operation| {
            for ((child, child_tree), child_layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(child_tree, child_layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let visible = bounds.intersection(viewport).unwrap_or(bounds);
        for ((child, child_tree), child_layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            if child_layout.bounds().intersects(&visible) {
                child.as_widget_mut().update(
                    child_tree,
                    event,
                    child_layout,
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    &visible,
                );
            }
        }
        if !self.is_lazy() || shell.is_event_captured() {
            return;
        }

        let cursor_position = cursor.position();
        let cursor_over = cursor_position.is_some_and(|position| bounds.contains(position));
        let mut changed = false;
        {
            let state = tree.state.downcast_mut::<VirtualTreeState>();
            let scrollbar = scrollbar_geometry(bounds, state);
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if cursor_over => {
                    if let (Some(position), Some((track, thumb))) = (cursor_position, scrollbar) {
                        if track.contains(position) {
                            if thumb.contains(position) {
                                state.scrollbar_drag_offset = Some(position.y - thumb.y);
                            } else {
                                let target = position.y - track.y - thumb.height / 2.0;
                                let travel = (track.height - thumb.height).max(1.0);
                                changed = state.set_offset(target / travel * state.max_offset());
                            }
                            shell.capture_event();
                        }
                    }
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) => {
                    if let (Some(grab), Some((track, thumb))) =
                        (state.scrollbar_drag_offset, scrollbar)
                    {
                        let travel = (track.height - thumb.height).max(1.0);
                        let target = position.y - track.y - grab;
                        changed = state.set_offset(target / travel * state.max_offset());
                        shell.capture_event();
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    if state.scrollbar_drag_offset.take().is_some() {
                        shell.capture_event();
                    }
                }
                Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor_over => {
                    let delta_y = match *delta {
                        mouse::ScrollDelta::Lines { y, .. } => -y * 60.0,
                        mouse::ScrollDelta::Pixels { y, .. } => -y,
                    };
                    changed = state.set_offset(state.offset_y + delta_y);
                    if changed {
                        shell.capture_event();
                    }
                }
                Event::Touch(touch::Event::FingerPressed { position, .. })
                    if bounds.contains(*position) =>
                {
                    state.last_touch_y = Some(position.y);
                }
                Event::Touch(touch::Event::FingerMoved { position, .. }) => {
                    if let Some(previous_y) = state.last_touch_y.replace(position.y) {
                        changed = state.set_offset(state.offset_y + previous_y - position.y);
                        if changed {
                            shell.capture_event();
                        }
                    }
                }
                Event::Touch(
                    touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. },
                ) => {
                    state.last_touch_y = None;
                }
                _ => {}
            }
        }

        if changed {
            let state = tree.state.downcast_ref::<VirtualTreeState>();
            if let Some(on_scroll) = self.on_scroll.as_ref() {
                shell.publish(on_scroll(state.offset_y, bounds.height));
            }
            shell.invalidate_layout();
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<VirtualTreeState>();
        if let (Some(position), Some((track, _))) = (
            cursor.position(),
            scrollbar_geometry(layout.bounds(), state),
        ) {
            if track.contains(position) {
                return mouse::Interaction::Pointer;
            }
        }
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, child_tree), child_layout)| {
                child.as_widget().mouse_interaction(
                    child_tree,
                    child_layout,
                    cursor,
                    viewport,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(visible) = layout.bounds().intersection(viewport) else {
            return;
        };
        renderer.with_layer(visible, |renderer| {
            for ((child, child_tree), child_layout) in self
                .children
                .iter()
                .zip(&tree.children)
                .zip(layout.children())
            {
                if child_layout.bounds().intersects(&visible) {
                    child.as_widget().draw(
                        child_tree,
                        renderer,
                        theme,
                        style,
                        child_layout,
                        cursor,
                        &visible,
                    );
                }
            }

            let state = tree.state.downcast_ref::<VirtualTreeState>();
            if let Some((track, thumb)) = scrollbar_geometry(layout.bounds(), state) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: track,
                        ..renderer::Quad::default()
                    },
                    Background::Color(Color::from_rgba(0.35, 0.35, 0.35, 0.18)),
                );
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: thumb,
                        ..renderer::Quad::default()
                    },
                    Background::Color(Color::from_rgba(0.55, 0.55, 0.55, 0.65)),
                );
            }
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<VirtualList<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(list: VirtualList<'a, Message, Theme, Renderer>) -> Self {
        Element::new(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_rows_normalize_offsets_and_preserve_virtual_height() {
        let list = VirtualList::<()>::new_keyed(
            [
                (1, f32::NAN, iced::widget::Space::new().into()),
                (2, 34.0, iced::widget::Space::new().into()),
            ],
            68.0,
        );

        assert_eq!(list.offsets, vec![0.0, 34.0]);
        assert_eq!(list.total_height, 68.0);
        assert_eq!(list.keys, vec![1, 2]);
    }

    #[test]
    fn virtual_height_never_clips_the_last_offset() {
        let list =
            VirtualList::<()>::new_keyed([(1, 128.0, iced::widget::Space::new().into())], 10.0);
        assert_eq!(list.total_height, 128.0);
    }

    #[test]
    fn scroll_state_clamps_to_virtual_extent() {
        let mut state = VirtualTreeState {
            keys: Vec::new(),
            offset_y: 0.0,
            viewport_height: 300.0,
            total_height: 1_000.0,
            built_offset_y: 0.0,
            built_viewport_height: 300.0,
            last_touch_y: None,
            scrollbar_drag_offset: None,
        };

        assert!(state.set_offset(2_000.0));
        assert_eq!(state.offset_y, 700.0);
        state.snap_to(RelativeOffset {
            x: None,
            y: Some(0.5),
        });
        assert_eq!(state.offset_y, 350.0);
    }
}
