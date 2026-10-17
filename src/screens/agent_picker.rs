// Picking one of the user's agents from a menu: who answers Ask in Feed settings and in the Feed's Ask panel, and who
// watches a Feed topic. A button with the agent's picture and name opens a menu of the
// agents (`agents:list`), the one picked checked and the main agent marked.

use gpui::{
    AnyElement, App, Corner, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button as MenuButton, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};

use crate::screens::agents::AgentRow;
use crate::theme::{ActiveTheme as _, Typography as _, space};

/// The menu of `agents`, `current` checked. `main_choice` puts "Main agent (<name>)" on top, picked as `None`, for a
/// setting that follows the main agent unless another is picked.
pub fn menu(
    mut menu: PopupMenu,
    agents: &[AgentRow],
    current: Option<&str>,
    main_choice: bool,
    pick: impl Fn(Option<String>, &mut Window, &mut App) + Clone + 'static,
) -> PopupMenu {
    menu = menu.min_w(px(220.)).max_h(px(420.)).scrollable(true);
    if main_choice {
        let label = agents
            .iter()
            .find(|agent| agent.main)
            .map_or_else(|| "Main agent".to_string(), |agent| format!("Main agent ({})", agent.name));
        let pick = pick.clone();
        menu = menu.item(
            PopupMenuItem::new(label).checked(current.is_none()).on_click(move |_, window, cx| pick(None, window, cx)),
        );
        menu = menu.separator();
    }
    for agent in agents {
        let (pick, id) = (pick.clone(), agent.id.clone());
        let row = agent.clone();
        let item = PopupMenuItem::element(move |_, cx| {
            let colors = *cx.colors();
            div()
                .flex()
                .items_center()
                .gap(px(space::SM))
                .child(row.tile(20., cx))
                .child(SharedString::from(row.name.clone()))
                .when(row.main, |this| this.child(div().caption().text_color(colors.text_secondary).child("Main")))
        });
        menu = menu.item(
            item.checked(current == Some(agent.id.as_str()))
                .on_click(move |_, window, cx| pick(Some(id.clone()), window, cx)),
        );
    }
    menu
}

/// A button showing `agent` (or `fallback` when it's none of them) that opens `menu` of `agents`.
pub fn button(
    id: impl Into<SharedString>,
    agents: Vec<AgentRow>,
    shown: Option<&AgentRow>,
    fallback: &str,
    current: Option<String>,
    main_choice: bool,
    pick: impl Fn(Option<String>, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> AnyElement {
    let colors = *cx.colors();
    let name = SharedString::from(shown.map(|agent| agent.name.clone()).unwrap_or_else(|| fallback.to_string()));
    MenuButton::new(id.into())
        .ghost()
        .small()
        .children(shown.map(|agent| agent.tile(18., cx)))
        .child(div().footnote().semibold().text_color(colors.text).child(name))
        .dropdown_caret(true)
        .dropdown_menu_with_anchor(Corner::TopLeft, move |popup, _, _| {
            menu(popup, &agents, current.as_deref(), main_choice, pick.clone())
        })
        .into_any_element()
}
