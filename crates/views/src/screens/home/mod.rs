// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

mod curated;

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, FontWeight, MouseDownEvent, Pixels, Point, Render,
    ScrollHandle, SharedString, WeakEntity, Window, div, px, svg,
};
use i18n::t;
use music::GenreItem;
use state::{Home, Network, Playback};
use ui::{ActiveTheme as _, Mode, Popup, Scrollbar, Scroller, Text};

use crate::chrome::Chrome;
use crate::shared::album_grid::CardGrid;
use crate::shared::cards;
use crate::shared::cells;
use crate::shared::menus::{Item, ItemMenu};
use crate::shared::shelves::{Rail, RailSpec, Shelves};
use crate::shared::tracks::{PlaybackStatus, playback_status};
use crate::shared::trouble;
use curated::Curated;

const STEADY: Pixels = px(0.5);

pub(crate) struct HomeView {
    home: Entity<Home>,
    playback: Entity<Playback>,
    playback_status: PlaybackStatus,
    shelves: Entity<Shelves>,
    width: Pixels,
    rail: Rail,
    scrollbar: Entity<Scrollbar>,
    menus: ItemMenu,
    context_menu: Option<(Item, Point<Pixels>)>,
}

impl HomeView {
    pub(crate) fn new(
        home: Entity<Home>,
        playback: Entity<Playback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let me = cx.entity_id();
        let playlist_scrollbar = cx.new(|_| Scrollbar::inset().watching(me));
        let menus = ItemMenu::new(playlist_scrollbar, cx);

        cx.observe(&home, |this, _, cx| {
            this.menus.reset(cx);
            this.context_menu = None;
            cx.notify();
        })
        .detach();
        let chrome = Chrome::entity(cx);
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();

        let library = state::Sonora::global(cx).library.clone();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();

        let shelves = cx.new(|_| Shelves::new("home-shelf", me, playback.clone()));
        cx.observe(&shelves, |_, _, cx| cx.notify()).detach();

        let current_playback = playback_status(&playback, cx);
        cx.observe(&playback, |this, playback, cx| {
            let current = playback_status(&playback, cx);
            if this.playback_status != current {
                this.playback_status = current;
                cx.notify();
            }
        })
        .detach();

        Self {
            home,
            playback,
            playback_status: current_playback,
            shelves,
            width: Pixels::ZERO,
            rail: Rail::new(me),
            scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(me)),
            menus,
            context_menu: None,
        }
    }

    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.home
            .update(cx, |home, cx| home.set_visible(visible, cx));
    }

    fn recommendations(
        &self,
        width: Pixels,
        window: &Window,
        notify: &Rc<dyn Fn(&mut App)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let items = self.home.read(cx).quick_picks();
        let items = match items.is_empty() {
            true => Curated::recommendations(),
            false => items.as_ref().clone(),
        };
        let layout = CardGrid::layout(width);
        let opened = Rc::new(items.clone());
        let card = layout.card;
        let home = cx.entity().downgrade();
        let playback = self.playback.clone();

        let icon = svg()
            .path(icons::path("icons/sparkles.svg"))
            .size_5()
            .flex_none()
            .text_color(gpui::hsla(48.0 / 360.0, 0.96, 0.53, 1.0))
            .into_any_element();

        let badge = div()
            .flex()
            .items_center()
            .rounded_full()
            .bg(theme.primary.opacity(0.15))
            .px_2p5()
            .py_0p5()
            .text_size(theme.text(Text::Tiny))
            .text_color(theme.primary)
            .font_weight(FontWeight::SEMIBOLD)
            .child(t!("home-live-picks"))
            .into_any_element();

        self.rail.render(
            RailSpec {
                tag: "home-picks",
                place: 0,
                title: t!("home-recommendations"),
                icon: Some(icon),
                badge: Some(badge),
                count: items.len(),
                tile: card,
                columns: layout.columns,
                tabs: None,
                trailing: None,
            },
            window,
            cx,
            notify,
            move |place, _, cx| {
                let Some(item) = opened.get(place) else {
                    return div().into_any_element();
                };
                let home_ref = home.clone();
                let opened_items = opened.clone();
                cards::item_card(
                    ElementId::NamedInteger(SharedString::new_static("picks-item"), place as u64),
                    item,
                    &playback,
                    cx,
                )
                .tile(card)
                .flat()
                .menu(move |event, _, cx| {
                    open_item_menu(&home_ref, &opened_items, place, event, cx);
                })
                .into_any_element()
            },
        )
    }

    fn failure(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let reason = match Network::lost(cx) {
            true => None,
            false => Some(self.home.read(cx).error()?.to_owned()),
        };
        let home = self.home.clone();

        Some(
            trouble::lost(
                "home-lost",
                t!("trouble-not-loaded"),
                reason.as_deref(),
                move |_, _, cx| {
                    home.update(cx, |home, cx| home.retry(cx));
                },
            )
            .size_full()
            .into_any_element(),
        )
    }
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(failure) = self.failure(cx) {
            return div().size_full().child(failure).into_any_element();
        }

        self.rail.sync();
        let theme = *cx.theme();
        let available = cells::content_width(window, theme.metrics.inset * 2., cx);
        if (available - self.width).abs() >= STEADY {
            self.width = available;
        }
        let context_menu = self.context_menu.clone().map(|(item, position)| {
            let menu = item.menu(&self.menus, self.playback.clone(), false, cx);
            Popup::new(position, menu).on_close(cx.listener(|this, _, _, cx| {
                this.context_menu = None;
                cx.notify();
            }))
        });

        let weak = cx.entity().downgrade();
        let notify: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
            weak.update(cx, |_, cx| cx.notify()).ok();
        });

        let picks = self.recommendations(available, window, &notify, cx);
        let sections = self.home.read(cx).sections();
        let feeding = self.home.read(cx).is_feeding();
        let width = self.width;
        let shelves = self
            .shelves
            .update(cx, |shelves, cx| match (sections.is_empty(), feeding) {
                (true, true) => shelves.pending(width, cx),
                (false, _) => vec![shelves.render(sections, Mode::Grid, width, window, cx)],
                (true, false) => {
                    let curated = Curated::sections();
                    vec![shelves.render(Rc::new(curated), Mode::Grid, width, window, cx)]
                }
            });

        Scroller::new("home-page", &self.scrollbar)
            .p(theme.metrics.inset)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_10()
                    .pb(px(48.))
                    .child(picks)
                    .children(shelves),
            )
            .when_some(context_menu, |this, menu| this.child(menu))
            .into_any_element()
    }
}

fn open_item_menu(
    home: &WeakEntity<HomeView>,
    items: &Rc<Vec<GenreItem>>,
    place: usize,
    event: &MouseDownEvent,
    cx: &mut gpui::App,
) {
    let Some(item) = items.get(place).and_then(Item::of) else {
        return;
    };
    let Some(home) = home.upgrade() else {
        return;
    };
    home.update(cx, |this, cx| {
        this.menus.reset(cx);
        this.context_menu = Some((item, event.position));
        cx.notify();
    });
}
