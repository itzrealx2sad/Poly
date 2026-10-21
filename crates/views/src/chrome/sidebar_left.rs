// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use gpui::prelude::*;
use gpui::{App, Context, Entity, Pixels, Render, ScrollHandle, SharedString, Window, div, px, svg};
use router::{Destination, LibraryTab, NavEntry, Navigation, NavigationEvent, SettingsTab, navigate};
use state::{AppSettings, Library, Pins, Playback, Session, Sonora};
use ui::{ActiveTheme as _, Scrollbar, Tipped as _};

#[allow(dead_code)]
pub(crate) struct SidebarLeft {
    settings: Entity<AppSettings>,
    session: Entity<Session>,
    trail: Entity<Navigation>,
    at: Destination,
    width: Pixels,
    open: bool,
    cramped: bool,
    forced: Option<bool>,
    library_open: bool,
    local_open: bool,
    pinned_open: bool,
    dropping: bool,
    drop_gap: Option<usize>,
    playback: Entity<Playback>,
    pins: Entity<Pins>,
    library: Entity<Library>,
    scrollbar: Entity<Scrollbar>,
}

impl SidebarLeft {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Sonora::global(cx).settings.clone();
        let session = Sonora::global(cx).session.clone();
        let playback = Sonora::global(cx).playback.clone();
        let pins = Sonora::global(cx).pins.clone();
        let library = Sonora::global(cx).library.clone();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        cx.observe(&pins, |_, _, cx| cx.notify()).detach();
        cx.observe(&playback, |_, _, cx| cx.notify()).detach();
        let me = cx.entity_id();
        let scrollbar = cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(me));
        let open = settings.read(cx).sidebar_open();
        let pinned_open = settings.read(cx).sidebar_pinned_open();
        let trail = router::trail(cx);

        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();
        cx.observe(&trail, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&trail, |this, _, _: &NavigationEvent, cx| {
            this.dismiss(cx);
            cx.notify();
        })
        .detach();

        let at = trail.read(cx).current();
        let library_open = matches!(at, Destination::Library(_));
        let local_open = matches!(at, Destination::Local(_));

        Self {
            settings,
            session,
            trail,
            at,
            width: px(64.),
            open,
            forced: None,
            cramped: false,
            library_open,
            local_open,
            pinned_open,
            dropping: false,
            drop_gap: None,
            playback,
            pins,
            library,
            scrollbar,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn overlays(&self) -> bool {
        false
    }

    pub fn overlay_width(&self) -> Pixels {
        Pixels::ZERO
    }

    fn dismiss(&mut self, _cx: &mut Context<Self>) {}

    pub fn occupied_width(&self) -> Pixels {
        match self.is_open() {
            true => px(64.),
            false => Pixels::ZERO,
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        self.persist(cx);
        cx.notify();
    }

    pub fn adapt(&mut self, _right: Pixels, _window: &Window, _cx: &mut App) {
        self.width = px(64.);
    }

    fn persist(&self, cx: &mut Context<Self>) {
        let width = self.width / px(1.);
        let open = self.open;
        self.settings
            .update(cx, |settings, cx| settings.set_sidebar(width, open, cx));
    }
}

impl Render for SidebarLeft {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        if !self.is_open() {
            return div().w(px(0.)).into_any_element();
        }

        let current = self.trail.read(cx).current();
        let is_settings = matches!(current, Destination::Settings(_));

        let nav_btn = |id: SharedString, icon: &'static str, tip: SharedString, active: bool, dest: Destination| {
            let tint = match active {
                true => theme.foreground,
                false => theme.muted_foreground,
            };
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(40.))
                .rounded(px(12.))
                .cursor_pointer()
                .when(active, |this| this.bg(theme.foreground.opacity(0.1)))
                .when(!active, |this| {
                    this.hover(|style| {
                        style
                            .bg(theme.foreground.opacity(0.05))
                            .text_color(theme.foreground)
                    })
                })
                .text_color(tint)
                .child(
                    svg()
                        .path(icons::path(icon))
                        .size(px(20.))
                        .text_color(tint),
                )
                .tip_label(tip, ui::Perch::Pointer)
                .on_click(move |_, _, cx| navigate(dest.clone(), cx))
        };

        let nav_items: Vec<_> = NavEntry::ALL
            .into_iter()
            .filter(|entry| self.settings.read(cx).nav_shown(entry.id()))
            .map(|entry| {
                let id = SharedString::from(entry.id());
                let tip = i18n::lookup(entry.key(), None);
                let active = entry.is_active(&current);
                nav_btn(id, entry.icon(), tip, active, entry.destination())
            })
            .collect();

        let ambient = crate::shared::ambient::shown(cx) && self.settings.read(cx).adaptive_theme();

        div()
            .id("sidebar-left")
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .w(px(64.))
            .h_full()
            .py(px(16.))
            .rounded(px(16.))
            .border_1()
            .border_color(theme.border)
            .when_else(
                ambient || ui::blurring(cx),
                |this| ui::glass(this, cx),
                |this| this.bg(theme.secondary),
            )
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(24.))
                    .child(
                        div()
                            .id("poly-logo-btn")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(40.))
                            .rounded(px(12.))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.foreground.opacity(0.08)))
                            .text_color(theme.foreground)
                            .child(
                                svg()
                                    .path(icons::path("icons/poly-logo.svg"))
                                    .size(px(24.))
                                    .text_color(theme.foreground),
                            )
                            .tip_label("Poly Music", ui::Perch::Pointer)
                            .on_click(|_, _, cx| navigate(Destination::Home, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(8.))
                            .children(nav_items),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(nav_btn(
                        SharedString::from("nav-settings"),
                        "icons/settings.svg",
                        i18n::lookup("nav-settings", None),
                        is_settings,
                        Destination::Settings(SettingsTab::General),
                    )),
            )
            .into_any_element()
    }
}
