// SPDX-License-Identifier: GPL-3.0-or-later
// Portions Copyright (C) Sonora Contributors
// Modifications Copyright (C) 2026 Poly Project Contributors

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyView, App, Context, Entity, FocusHandle, Pixels, Render, StyleRefinement};
use gpui::{Window, deferred, div, px};
use input::WORKSPACE_CONTEXT;
use state::{Playback, Queue, SideTab};
use ui::{
    Activate, Deselect, Entrance, Remove, SelectNext, SelectPrevious, shown_listing,
};

use crate::chrome::{
    AppNotice, Chrome, PlayerBar, SidebarLeft, SidebarRight, TitleBarOptions, ToastStack,
};
use crate::shared::confirm::Confirm;
use crate::shared::menus::CardMenu;
use crate::shared::playlist_editor::PlaylistEditor;
use crate::shared::tag_editor::TagEditor;
use crate::shared::widevine::WidevinePrompt;
use crate::shells::Shell;

pub(crate) struct Workspace {
    sidebar: Entity<SidebarLeft>,
    player_bar: Entity<PlayerBar>,
    sidebar_right: Entity<SidebarRight>,
    playlist_editor: Entity<PlaylistEditor>,
    tag_editor: Entity<TagEditor>,
    confirm: Entity<Confirm>,
    card_menu: Entity<CardMenu>,
    widevine: Entity<WidevinePrompt>,
    toasts: Entity<ToastStack>,
    notice: Entity<AppNotice>,
    content: AnyView,
    /// A screen's own header, floated over the top of the page and outside its transition.
    /// The page pads itself to start beneath it.
    header: Option<AnyView>,
    transition: Option<Entrance>,
    focus: FocusHandle,
}

impl Workspace {
    pub fn new(
        playback: Entity<Playback>,
        queue: Entity<Queue>,
        content: AnyView,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar = cx.new(SidebarLeft::new);
        let sidebar_right = cx.new(|cx| SidebarRight::new(queue.clone(), playback.clone(), cx));
        let player_bar = cx.new(|cx| PlayerBar::new(playback, queue, cx));
        cx.observe(&sidebar_right, |_, _, cx| cx.notify()).detach();

        Self {
            sidebar,
            player_bar,
            sidebar_right,
            playlist_editor: PlaylistEditor::entity(cx),
            tag_editor: TagEditor::entity(cx),
            confirm: Confirm::entity(cx),
            card_menu: CardMenu::entity(cx),
            widevine: cx.new(WidevinePrompt::new),
            toasts: cx.new(ToastStack::new),
            notice: cx.new(AppNotice::new),
            content,
            header: None,
            transition: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn toggle_sidebar(&self, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| sidebar.toggle(cx));
    }

    pub fn toggle_sidebar_right(&self, cx: &mut Context<Self>) {
        self.sidebar_right.update(cx, |panel, cx| panel.toggle(cx));
    }

    pub fn show_side(&self, tab: SideTab, cx: &mut Context<Self>) {
        self.sidebar_right
            .update(cx, |panel, cx| panel.show(tab, cx));
    }

    #[allow(dead_code)]
    pub fn content(&self) -> &AnyView {
        &self.content
    }

    /// Shows a page, with the header it wants above it or none. The two arrive together so
    /// a header can never outlive its screen.
    pub fn set_content(
        &mut self,
        content: AnyView,
        header: Option<AnyView>,
        cx: &mut Context<Self>,
    ) {
        self.content = content;
        self.header = header;
        cx.notify();
    }

    pub fn reveal_content(&mut self, cx: &mut Context<Self>) -> Duration {
        if cx.reduce_motion() {
            self.transition = None;
            return Duration::ZERO;
        }

        let entrance = Entrance::start();
        self.transition = Some(entrance);
        cx.notify();
        entrance.span()
    }

    pub fn finish_transition(&mut self, cx: &mut Context<Self>) {
        if self.transition.take().is_some() {
            cx.notify();
        }
    }

    fn hidden(&mut self, window: &mut Window, cx: &Context<Self>) -> f32 {
        if cx.reduce_motion() {
            self.transition = None;
            return 0.;
        }
        let Some(transition) = self.transition else {
            return 0.;
        };
        if transition.running() {
            window.request_animation_frame();
        }
        transition.hidden()
    }

    #[allow(dead_code)]
    pub fn player_bar(&self) -> &Entity<PlayerBar> {
        &self.player_bar
    }
}

impl Shell for Workspace {
    fn title_bar(&self, content: Option<AnyView>, cx: &App) -> TitleBarOptions {
        let sidebar = self.sidebar.read(cx);

        TitleBarOptions {
            navigation: true,
            sidebar_open: sidebar.is_open(),
            sidebar_right: Some(self.sidebar_right.read(cx).is_open()),
            offset: sidebar.occupied_width(),
            border: true,
            content,
            transparent: false,
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _current = router::trail(cx).read(cx).current();
        let show_floating_controls = cfg!(target_os = "windows");
        let is_trans = self.sidebar_right.read(cx).is_transitioning();
        if is_trans {
            cx.on_next_frame(window, |_, _, cx| cx.notify());
        }
        let is_right_visible = self.sidebar_right.read(cx).is_visible() && SidebarRight::available(window);
        let target_w = self.sidebar_right.read(cx).width();
        let factor = self.sidebar_right.read(cx).factor();
        let reserve_w = (target_w + px(10.)) * factor;
        let slide_offset = (target_w + px(20.)) * (1.0 - factor);
        let stable_right = Pixels::ZERO;
        let reserved = self.sidebar_right.read(cx).reserved_width();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.adapt(reserved, window, cx));
        let left = self.sidebar.read(cx).occupied_width();
        let _overlay_width = self.sidebar.read(cx).overlay_width();
        Chrome::publish(left, stable_right, reserved, cx);
        let covered = self.sidebar_right.read(cx).covers_content(window);
        let overlay = self.sidebar.read(cx).overlays();
        let sidebar_width = self.sidebar.read(cx).occupied_width();
        let sidebar = match overlay || sidebar_width == gpui::Pixels::ZERO {
            true => self.sidebar.clone().into_any_element(),
            false => self
                .sidebar
                .clone()
                .cached(StyleRefinement::default().w(sidebar_width).h_full())
                .into_any_element(),
        };
        let hidden = self.hidden(window, cx);
        let content = match hidden > 0. || is_trans {
            true => self.content.clone().into_any_element(),
            false => self
                .content
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
        };

        div()
            .relative()
            .size_full()
            .key_context(super::shell_context(WORKSPACE_CONTEXT))
            .track_focus(&self.focus)
            .on_action(|_: &SelectNext, window, cx| {
                if let Some(table) = shown_listing(cx) {
                    table.select_next(window, cx);
                    cx.stop_propagation();
                }
            })
            .on_action(|_: &SelectPrevious, window, cx| {
                if let Some(table) = shown_listing(cx) {
                    table.select_previous(window, cx);
                    cx.stop_propagation();
                }
            })
            .on_action(|_: &Deselect, _, cx| {
                if let Some(table) = shown_listing(cx) {
                    table.deselect(cx);
                    cx.stop_propagation();
                }
            })
            .on_action(|_: &Activate, _, cx| {
                if let Some(table) = shown_listing(cx) {
                    table.activate(cx);
                    cx.stop_propagation();
                }
            })
            .on_action(|_: &Remove, _, cx| {
                if let Some(table) = shown_listing(cx) {
                    table.remove(cx);
                    cx.stop_propagation();
                }
            })
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .size_full()
                    .p(px(10.))
                    .gap(px(10.))
                    .child(sidebar)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .gap(px(10.))
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_1()
                                    .min_h_0()
                                    .child(
                                        div()
                                            .relative()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .overflow_hidden()
                                            .when(covered, |this| this.hidden())
                                            .child(
                                                div()
                                                    .relative()
                                                    .flex()
                                                    .flex_col()
                                                    .flex_1()
                                                    .min_h_0()
                                                    .overflow_hidden()
                                                    .fade_edges(px(0.), px(48.))
                                                    .child(
                                                        div()
                                                            .absolute()
                                                            .left_0()
                                                            .right_0()
                                                            .top(px(14.) * hidden)
                                                            .h_full()
                                                            .opacity(1. - hidden)
                                                            .flex()
                                                            .flex_col()
                                                            .child(content),
                                                    )
                                                    .when_some(self.header.clone(), |this, header| {
                                                        this.child(
                                                            div()
                                                                .absolute()
                                                                .inset_0()
                                                                .child(header),
                                                        )
                                                    }),
                                            ),
                                    )
                                    .when(reserve_w > Pixels::ZERO, |this| {
                                        this.child(
                                            div()
                                                .relative()
                                                .h_full()
                                                .w(reserve_w)
                                                .flex_none(),
                                        )
                                    })
                                    .when(is_right_visible, |this| {
                                        this.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .right(px(0.) - slide_offset)
                                                .w(target_w)
                                                .h_full()
                                                .child(self.sidebar_right.clone()),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .relative()
                                    .w_full()
                                    .flex_none()
                                    .child(self.player_bar.clone())
                                    .child(self.toasts.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(deferred(self.notice.clone()))
                    .child(self.card_menu.clone())
                    .child(self.playlist_editor.clone())
                    .child(self.tag_editor.clone())
                    .child(self.confirm.clone())
                    .child(self.widevine.clone()),
            )
            .when(show_floating_controls, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(22.))
                        .right(px(24.))
                        .child(ui::WindowControls::new(false)),
                )
            })
    }
}
