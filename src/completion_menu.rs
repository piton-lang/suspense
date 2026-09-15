//! The chat input's completion menu: asks a completion provider for items as
//! the user types, lists them in a popover at the cursor, and inserts the one
//! the user accepts.
//!
//! gpui-kit's own menu always opens below the cursor, so at the bottom of the
//! window it runs off-screen. This one opens below the cursor when it fits,
//! above it otherwise, and always stays inside the window. Its rows and styling
//! follow gpui-kit's menu.

use std::rc::Rc;

use gpui_kit::component::input::{CompletionProvider, EditorState, Rope};
use gpui_kit::component::label::Label;
use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::{ActiveTheme, IndexPath, Selectable, ThemeStyled as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTriggerKind, Documentation,
};

const MAX_MENU_HEIGHT: Pixels = px(240.);
const MAX_MENU_WIDTH: Pixels = px(320.);
const MIN_MENU_WIDTH: Pixels = px(120.);
const POPOVER_GAP: Pixels = px(4.);
/// The least room kept between the menu and the window's edges.
const WINDOW_MARGIN: Pixels = px(8.);

pub struct CompletionMenu {
    editor: Entity<EditorState>,
    provider: Option<Rc<dyn CompletionProvider>>,
    list: Entity<ListState<MenuDelegate>>,
    open: bool,
    /// Where the text being completed starts, while a completion is under way.
    trigger_start: Option<usize>,
    /// The editor's text and cursor as last seen, to tell an edit from a
    /// cursor move.
    text: SharedString,
    cursor: usize,
    /// The editor's text right after a completion was inserted, whose edit
    /// must not start another completion.
    inserted: Option<SharedString>,
    request: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl CompletionMenu {
    pub fn new(editor: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| {
            ListState::new(
                MenuDelegate {
                    query: SharedString::default(),
                    items: Vec::new(),
                    selected_ix: 0,
                },
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&list, window, |this, _, event: &ListEvent, window, cx| {
                if let ListEvent::Confirm(ix) = event {
                    this.accept(ix.row, window, cx);
                }
            }),
            // A cursor moved without an edit ends the completion.
            cx.observe(&editor, |this, editor, cx| {
                let editor = editor.read(cx);
                if this.open && editor.value() == this.text && editor.cursor() != this.cursor {
                    this.hide(cx);
                }
            }),
        ];
        let (text, cursor) = {
            let editor = editor.read(cx);
            (editor.value(), editor.cursor())
        };
        Self {
            editor,
            provider: None,
            list,
            open: false,
            trigger_start: None,
            text,
            cursor,
            inserted: None,
            request: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_provider(&mut self, provider: Rc<dyn CompletionProvider>, cx: &mut Context<Self>) {
        self.provider = Some(provider);
        self.hide(cx);
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The items the menu lists.
    #[cfg(test)]
    pub fn items<'a>(&self, cx: &'a App) -> &'a [Rc<CompletionItem>] {
        &self.list.read(cx).delegate().items
    }

    /// Called after every edit of the editor's text: starts, refreshes, or
    /// ends a completion.
    pub fn on_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let editor = self.editor.read(cx);
            (editor.value(), editor.cursor())
        };
        let previous = std::mem::replace(&mut self.text, text.clone());
        self.cursor = cursor;
        if self.inserted.take().is_some_and(|inserted| inserted == text) {
            return;
        }
        let Some(provider) = self.provider.clone() else {
            return;
        };

        // What the edit typed just before the cursor, if it typed anything.
        let typed = (text.len() > previous.len())
            .then(|| text.get(..cursor)?.chars().next_back())
            .flatten();
        let start = match (typed, self.trigger_start) {
            (Some(typed), start) => {
                if !provider.is_completion_trigger(cursor, &typed.to_string(), cx) {
                    self.hide(cx);
                    return;
                }
                start.unwrap_or(cursor - typed.len_utf8())
            }
            // A deletion refreshes a completion under way.
            (None, Some(start)) => start,
            (None, None) => return,
        };
        if cursor < start {
            self.hide(cx);
            return;
        }
        self.trigger_start = Some(start);

        let query = text
            .get(start..cursor)
            .unwrap_or_default()
            .trim()
            .to_string();
        let context = CompletionContext {
            trigger_kind: CompletionTriggerKind::TRIGGER_CHARACTER,
            trigger_character: Some(query.clone()),
        };
        let response =
            provider.completions(&Rope::from(text.as_ref()), cursor, context, window, cx);
        self.request = cx.spawn_in(window, async move |this, cx| {
            let items = match response.await {
                Ok(CompletionResponse::Array(items)) => items,
                Ok(CompletionResponse::List(list)) => list.items,
                Err(_) => Vec::new(),
            };
            this.update_in(cx, |this, window, cx| {
                // Items for text that has since changed are stale.
                if this.text != text || this.cursor != cursor {
                    return;
                }
                if items.is_empty() || !this.editor.read(cx).focus_handle(cx).is_focused(window) {
                    this.hide(cx);
                } else {
                    this.show(query, items, window, cx);
                }
            })
            .ok();
        });
    }

    fn show(
        &mut self,
        query: String,
        items: Vec<CompletionItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let longest_ix = items
            .iter()
            .enumerate()
            .max_by_key(|(_, item)| item.label.len() + item.detail.as_ref().map_or(0, String::len))
            .map_or(0, |(ix, _)| ix);
        self.list.update(cx, |list, cx| {
            let delegate = list.delegate_mut();
            delegate.query = query.into();
            delegate.items = items.into_iter().map(Rc::new).collect();
            list.set_selected_index(Some(IndexPath::new(0)), window, cx);
            list.scroll_to_item(IndexPath::new(0), ScrollStrategy::Top, window, cx);
            list.set_item_to_measure_index(IndexPath::new(longest_ix), window, cx);
        });
        self.open = true;
        cx.notify();
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.request = Task::ready(());
        self.trigger_start = None;
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    /// Moves the selection `step` items along, wrapping around.
    pub fn select_next(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |list, cx| {
            let count = list.delegate().items.len() as isize;
            if count == 0 {
                return;
            }
            let selected = list.delegate().selected_ix as isize;
            let ix = IndexPath::new((selected + step).rem_euclid(count) as usize);
            list.set_selected_index(Some(ix), window, cx);
            list.scroll_to_item(ix, ScrollStrategy::Nearest, window, cx);
        });
    }

    /// Inserts the selected item.
    pub fn accept_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.list.read(cx).delegate().selected_ix;
        self.accept(ix, window, cx);
    }

    fn accept(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.list.read(cx).delegate().items.get(ix).cloned() else {
            return;
        };
        // Without an edit of its own, the item replaces the text typed since
        // the completion started.
        let range = self.trigger_start.unwrap_or(self.cursor)..self.cursor;
        self.editor.update(cx, |editor, cx| {
            editor.insert_completion(&item, range, window, cx)
        });
        let editor = self.editor.read(cx);
        self.text = editor.value();
        self.cursor = editor.cursor();
        self.inserted = Some(self.text.clone());
        self.hide(cx);
    }

    /// The cursor's line, in window coordinates.
    fn cursor_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
        let editor = self.editor.read(cx);
        let (cursor, line_height) = editor.cursor_layout()?;
        Some(Bounds::new(
            cursor.origin + editor.scroll_offset(),
            size(cursor.size.width, line_height),
        ))
    }
}

impl Render for CompletionMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open || self.list.read(cx).delegate().items.is_empty() {
            return Empty.into_any_element();
        }
        let Some(cursor) = self.cursor_bounds(cx) else {
            return Empty.into_any_element();
        };

        let documentation = self
            .list
            .read(cx)
            .delegate()
            .selected_item()
            .and_then(|item| item.documentation.clone());
        let viewport = window.viewport_size();
        let max_width = MAX_MENU_WIDTH.min(viewport.width - WINDOW_MARGIN * 2);
        // Documentation goes beside the list when both fit across the window
        // (the popover shifts left to make room), otherwise it is stacked with
        // the list and cut to its first line.
        let stacked =
            WINDOW_MARGIN + max_width + POPOVER_GAP + max_width + WINDOW_MARGIN > viewport.width;

        let list = self.list.clone();
        let menu = cx.entity().downgrade();
        let build = move |above: bool, _: &mut Window, cx: &mut App| {
            div()
                .flex()
                .flex_row()
                .gap(POPOVER_GAP)
                .items_start()
                .when(stacked, |this| {
                    // Above the cursor, the list stays next to it.
                    if above {
                        this.flex_col_reverse()
                    } else {
                        this.flex_col()
                    }
                })
                .child(TestSupportExt::test_support(
                    popover("completion-menu", cx)
                        .max_w(max_width)
                        .min_w(MIN_MENU_WIDTH)
                        .child(List::new(&list).max_h(MAX_MENU_HEIGHT)),
                ))
                .when_some(documentation.clone(), |this, documentation| {
                    let mut doc = match documentation {
                        Documentation::String(doc) => doc,
                        Documentation::MarkupContent(content) => content.value,
                    };
                    if stacked {
                        doc = doc.lines().next().unwrap_or_default().to_string();
                    }
                    this.child(
                        div().child(
                            popover("completion-menu-doc", cx)
                                .w(max_width)
                                .px_2()
                                .child(render_markdown(doc, cx)),
                        ),
                    )
                })
                .on_mouse_down_out({
                    let menu = menu.clone();
                    move |_, _, cx| {
                        menu.update(cx, |menu, cx| menu.hide(cx)).ok();
                    }
                })
                .into_any_element()
        };

        deferred(PlacedPopover {
            cursor,
            build: Box::new(build),
        })
        .into_any_element()
    }
}

/// Where a popover of `popover` size goes for a cursor at `cursor`: below it
/// when it fits, otherwise above it if there is more room there, and always
/// inside the window. Returns the origin, and whether it went above.
fn place_popover(
    cursor: Bounds<Pixels>,
    popover: Size<Pixels>,
    viewport: Size<Pixels>,
) -> (Point<Pixels>, bool) {
    let space_below = viewport.height - WINDOW_MARGIN - (cursor.bottom() + POPOVER_GAP);
    let space_above = cursor.top() - POPOVER_GAP - WINDOW_MARGIN;
    let above = popover.height > space_below && space_above > space_below;

    let y = if above {
        cursor.top() - POPOVER_GAP - popover.height
    } else {
        cursor.bottom() + POPOVER_GAP
    };
    let y = y
        .min(viewport.height - WINDOW_MARGIN - popover.height)
        .max(WINDOW_MARGIN);
    let x = (cursor.left() - POPOVER_GAP)
        .min(viewport.width - WINDOW_MARGIN - popover.width)
        .max(WINDOW_MARGIN);
    (point(x, y), above)
}

/// Lays out the popover at its measured size and places it with
/// [`place_popover`], outside the layout of its parent.
struct PlacedPopover {
    cursor: Bounds<Pixels>,
    /// Builds the popover; told whether it sits above the cursor.
    build: Box<dyn Fn(bool, &mut Window, &mut App) -> AnyElement>,
}

impl IntoElement for PlacedPopover {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PlacedPopover {
    type RequestLayoutState = (Point<Pixels>, AnyElement);
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut popover = (self.build)(false, window, cx);
        let popover_size = popover.layout_as_root(AvailableSpace::min_size(), window, cx);
        let (origin, above) = place_popover(self.cursor, popover_size, window.viewport_size());
        if above {
            popover = (self.build)(true, window, cx);
            popover.layout_as_root(AvailableSpace::min_size(), window, cx);
        }

        let style = Style {
            position: Position::Absolute,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), (origin, popover))
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (origin, popover): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_absolute_element_offset(*origin, |window| popover.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (_, popover): &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        popover.paint(window, cx);
    }
}

fn popover(id: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .occlude()
        .popover_style(cx)
        .shadow_md()
        .text_xs()
        .p_1()
}

fn render_markdown(markdown: String, cx: &App) -> TextView {
    TextView::markdown("doc", markdown)
        .style(
            TextViewStyle::default()
                .paragraph_gap(rems(0.5))
                .heading_font_size(|level, rem_size| match level {
                    1..=3 => rem_size * 1,
                    4 => rem_size * 0.9,
                    _ => rem_size * 0.8,
                })
                .code_block(
                    StyleRefinement::default()
                        .bg(cx.theme().transparent)
                        .p_0()
                        .text_size(px(11.)),
                ),
        )
        .selectable(true)
}

struct MenuDelegate {
    /// The text typed since the completion started, highlighted in each label.
    query: SharedString,
    items: Vec<Rc<CompletionItem>>,
    selected_ix: usize,
}

impl MenuDelegate {
    fn selected_item(&self) -> Option<&Rc<CompletionItem>> {
        self.items.get(self.selected_ix)
    }
}

impl ListDelegate for MenuDelegate {
    type Item = MenuItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        Some(MenuItem {
            ix: ix.row,
            item: self.items.get(ix.row)?.clone(),
            query_len: self.query.len(),
            selected: false,
        })
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_ix = ix.map_or(0, |ix| ix.row);
        cx.notify();
    }
}

#[derive(IntoElement)]
struct MenuItem {
    ix: usize,
    item: Rc<CompletionItem>,
    query_len: usize,
    selected: bool,
}

impl Selectable for MenuItem {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for MenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let item = self.item;
        let deprecated = item.deprecated.unwrap_or(false);
        let matched_len = item
            .filter_text
            .as_ref()
            .map_or(self.query_len, String::len)
            .min(item.label.len());
        let highlights = vec![(
            0..matched_len,
            HighlightStyle {
                color: Some(cx.theme().blue),
                ..Default::default()
            },
        )];

        h_flex()
            .id(self.ix)
            .gap_2()
            .p_1()
            .text_xs()
            .line_height(relative(1.))
            .rounded(cx.theme().radius.half())
            .when(deprecated, |this| this.line_through())
            .hover(|this| this.bg(cx.theme().accent.opacity(0.8)))
            .when(self.selected, |this| {
                this.bg(cx.theme().tokens.accent)
                    .text_color(cx.theme().accent_foreground)
            })
            .child(div().child(StyledText::new(item.label.clone()).with_highlights(highlights)))
            .when_some(item.detail.clone(), |this, detail| {
                this.child(
                    Label::new(detail)
                        .text_color(cx.theme().muted_foreground)
                        .when(deprecated, |this| this.line_through())
                        .italic(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, point, px, size};

    use super::{WINDOW_MARGIN, place_popover};

    const VIEWPORT: gpui_kit::Size<gpui_kit::Pixels> = gpui_kit::Size {
        width: px(800.),
        height: px(600.),
    };

    fn cursor_at(x: f32, y: f32) -> Bounds<gpui_kit::Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(2.), px(20.)))
    }

    #[test]
    fn opens_below_the_cursor_when_it_fits() {
        let (origin, above) =
            place_popover(cursor_at(100., 100.), size(px(200.), px(150.)), VIEWPORT);
        assert!(!above);
        assert!(origin.y > px(120.));
    }

    #[test]
    fn opens_above_the_cursor_at_the_bottom_of_the_window() {
        let popover = size(px(200.), px(150.));
        let (origin, above) = place_popover(cursor_at(100., 560.), popover, VIEWPORT);
        assert!(above);
        assert!(origin.y + popover.height < px(560.));
    }

    #[test]
    fn stays_inside_the_window() {
        let popover = size(px(300.), px(500.));
        for cursor in [cursor_at(780., 300.), cursor_at(0., 0.), cursor_at(790., 590.)] {
            let (origin, _) = place_popover(cursor, popover, VIEWPORT);
            assert!(origin.x >= WINDOW_MARGIN && origin.y >= WINDOW_MARGIN, "{origin:?}");
            assert!(
                origin.x + popover.width <= VIEWPORT.width - WINDOW_MARGIN
                    && origin.y + popover.height <= VIEWPORT.height - WINDOW_MARGIN,
                "{origin:?}"
            );
        }
    }
}
