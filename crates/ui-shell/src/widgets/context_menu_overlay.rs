use iced::advanced::layout;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{mouse, Element, Length, Point, Rectangle, Size, Vector};

pub struct ContextMenuOverlay<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    menu: Element<'a, Message, Theme, Renderer>,
    position: Point,
    dismiss: Message,
}

impl<'a, Message, Theme, Renderer> ContextMenuOverlay<'a, Message, Theme, Renderer> {
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        menu: impl Into<Element<'a, Message, Theme, Renderer>>,
        position: Point,
        dismiss: Message,
    ) -> Self {
        Self {
            content: content.into(),
            menu: menu.into(),
            position,
            dismiss,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for ContextMenuOverlay<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: iced::advanced::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::new(&self.menu)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.content.as_widget(), self.menu.as_widget()]);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let mut children = tree.children.iter_mut();
        let content_state = children.next()?;
        let menu_state = children.next()?;
        let content_overlay = self.content.as_widget_mut().overlay(
            content_state,
            layout,
            renderer,
            viewport,
            translation,
        );
        let menu_overlay = overlay::Element::new(Box::new(MenuOverlay {
            menu: &mut self.menu,
            state: menu_state,
            position: self.position + translation,
            dismiss: self.dismiss.clone(),
        }));
        Some(
            overlay::Group::with_children(
                content_overlay.into_iter().chain([menu_overlay]).collect(),
            )
            .overlay(),
        )
    }
}

impl<'a, Message, Theme, Renderer> From<ContextMenuOverlay<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(value: ContextMenuOverlay<'a, Message, Theme, Renderer>) -> Self {
        Element::new(value)
    }
}

struct MenuOverlay<'a, 'b, Message, Theme, Renderer> {
    menu: &'b mut Element<'a, Message, Theme, Renderer>,
    state: &'b mut Tree,
    position: Point,
    dismiss: Message,
}

impl<Message, Theme, Renderer> overlay::Overlay<Message, Theme, Renderer>
    for MenuOverlay<'_, '_, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: iced::advanced::Renderer,
{
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        let menu = self.menu.as_widget_mut().layout(
            self.state,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        let size = menu.size();
        let position = Point::new(
            self.position
                .x
                .clamp(0.0, (bounds.width - size.width).max(0.0)),
            self.position
                .y
                .clamp(0.0, (bounds.height - size.height).max(0.0)),
        );
        menu.move_to(position)
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        self.menu
            .as_widget_mut()
            .operate(self.state, layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        self.menu.as_widget_mut().update(
            self.state,
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            &Rectangle::with_size(Size::INFINITE),
        );
        if matches!(
            event,
            iced::Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left | mouse::Button::Right
            ))
        ) && cursor
            .position()
            .is_some_and(|position| !layout.bounds().contains(position))
        {
            shell.publish(self.dismiss.clone());
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.menu.as_widget().mouse_interaction(
            self.state,
            layout,
            cursor,
            &Rectangle::with_size(Size::INFINITE),
            renderer,
        )
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.menu.as_widget().draw(
            self.state,
            renderer,
            theme,
            style,
            layout,
            cursor,
            &Rectangle::with_size(Size::INFINITE),
        );
    }

    fn overlay<'c>(
        &'c mut self,
        layout: Layout<'c>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, Renderer>> {
        self.menu.as_widget_mut().overlay(
            self.state,
            layout,
            renderer,
            &Rectangle::with_size(Size::INFINITE),
            Vector::ZERO,
        )
    }
}
