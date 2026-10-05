// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server and profile management: add servers, sign in, pick a saved profile.

use std::time::Duration;

use gpui_icons::LucideIcon;
use gpui_kit::{
    AppContext as _, Context, Div, Focusable as _, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, ObjectFit, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window,
    base::input::{IndentInline, OutdentInline},
    div,
    prelude::FluentBuilder as _,
    px, rgb, rgba,
};

use crate::{
    app::Bloom,
    config::{Profile, Server, normalize_url},
    jellyfin::{Branding, Client, User},
    ui::{
        glass::glass,
        input::{Input, InputEvent, InputState},
        scroll_area::ScrollArea,
    },
    views::cards::icon,
};
use crate::ui::tip::tip;

/// A Quick Connect sign-in that waits for approval on another device.
pub struct QuickSignIn {
    /// Code the user types on the other device.
    pub code: String,
    /// Stops the wait when dropped.
    _wait: Task<()>,
}

/// The dialog that lets another device sign in with a code.
#[derive(Default)]
pub struct Authorize {
    pub busy: bool,
    pub error: Option<String>,
}

pub struct ConnectState {
    pub url: gpui_kit::Entity<InputState>,
    pub username: gpui_kit::Entity<InputState>,
    pub password: gpui_kit::Entity<InputState>,
    /// Code field of the Quick Connect dialog.
    pub code: gpui_kit::Entity<InputState>,
    pub selected_server: Option<String>,
    /// Shows the server list although a server is selected.
    pub choosing_server: bool,
    pub public_users: Vec<User>,
    /// Sign-in text and splash image setting of the selected server.
    pub branding: Branding,
    /// The selected server accepts Quick Connect.
    pub quick_enabled: bool,
    pub quick: Option<QuickSignIn>,
    pub quick_starting: bool,
    pub authorize: Option<Authorize>,
    pub probing: bool,
    pub signing_in: bool,
    /// The remove button that waits for a second click, by its name. A
    /// removal ends sessions, so one click must not do it.
    pub confirm: Option<String>,
    /// Empties the password field at the next frame.
    pub clear_password: bool,
    pub error: Option<String>,
}

impl ConnectState {
    pub fn new(window: &mut Window, cx: &mut Context<Bloom>) -> Self {
        Self {
            url: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://jellyfin.example.com")),
            username: cx.new(|cx| InputState::new(window, cx).placeholder("Username")),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Password")
                    .masked(true)
            }),
            code: cx.new(|cx| InputState::new(window, cx).placeholder("6-digit code")),
            selected_server: None,
            choosing_server: false,
            public_users: Vec::new(),
            branding: Branding::default(),
            quick_enabled: false,
            quick: None,
            quick_starting: false,
            authorize: None,
            probing: false,
            signing_in: false,
            confirm: None,
            clear_password: false,
            error: None,
        }
    }

    /// Enter submits the relevant form.
    pub fn subscribe(&self, window: &mut Window, cx: &mut Context<Bloom>) -> Vec<Subscription> {
        vec![
            cx.subscribe_in(&self.url, window, |this, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add_server(cx);
                }
            }),
            cx.subscribe_in(&self.username, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let focus = this.connect.password.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
            }),
            cx.subscribe_in(&self.password, window, |this, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.sign_in(cx);
                }
            }),
            cx.subscribe_in(&self.code, window, |this, _, event, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.authorize_quick_connect(cx);
                }
            }),
        ]
    }
}

/// Says in plain words why a request to a server failed.
fn explain(err: &anyhow::Error, url: &str) -> String {
    use crate::connection::{Verdict, Why, classify, plain_words};
    let text = format!("{err:#}");
    let host = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    // Nothing of the server answered: the same rule as the offline state.
    match classify(err) {
        Verdict::Unreachable(Why::Gateway(code)) => {
            return format!(
                "{host} is offline behind its proxy ({code}): the proxy answers, but the server \
                 behind it does not. Start the server, or try again later."
            );
        }
        Verdict::Unreachable(why) => {
            return format!(
                "{host} does not answer. {} Check the address and your network connection.",
                plain_words(why)
            );
        }
        _ => {}
    }
    if text.contains("decode") {
        format!(
            "{host} answered, but not as a Jellyfin server. Check the address; a proxy or a \
             firewall page can also cause this."
        )
    } else if text.contains("HTTP 403") {
        format!("{host} refused the request (403). A firewall in front of the server can cause this.")
    } else if text.contains("HTTP 404") {
        format!("No Jellyfin server was found at {host}. Check the address and the path.")
    } else if text.contains("HTTP 5") {
        format!("{host} has a problem at the moment ({}). Try again later.", status(&text))
    } else if text.contains("HTTP ") {
        format!("{host} refused the request ({}).", status(&text))
    } else {
        format!("Could not reach {host}. Check the address and your network connection.")
    }
}

/// "HTTP 502" out of an error text.
fn status(text: &str) -> String {
    text.find("HTTP ")
        .map(|i| text[i..].chars().take(8).collect())
        .unwrap_or_default()
}

impl Bloom {
    pub fn select_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        self.connect.selected_server = Some(server_id.clone());
        self.connect.choosing_server = false;
        self.connect.confirm = None;
        self.connect.public_users.clear();
        self.connect.branding = Branding::default();
        self.connect.quick_enabled = false;
        self.connect.quick = None;
        self.connect.error = None;
        let Some(server) = self.config.server(&server_id) else {
            return;
        };
        let client = Client::new(&server.url, &self.config.device_id);
        self.fetch_with(
            client,
            cx,
            |client| {
                // Each part is optional: a server that hides one of them
                // still gets a sign-in form.
                Ok(std::thread::scope(|scope| {
                    let branding = scope.spawn(|| client.branding().unwrap_or_default());
                    let quick = scope.spawn(|| client.quick_connect_enabled().unwrap_or(false));
                    let users = client.public_users().unwrap_or_default();
                    (
                        users,
                        branding.join().unwrap_or_default(),
                        quick.join().unwrap_or(false),
                    )
                }))
            },
            move |this, result, cx| {
                if this.connect.selected_server.as_deref() != Some(server_id.as_str()) {
                    return;
                }
                if let Ok((users, branding, quick)) = result {
                    this.connect.public_users = users;
                    this.connect.branding = branding;
                    this.connect.quick_enabled = quick;
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    pub fn add_server(&mut self, cx: &mut Context<Self>) {
        let raw = self.connect.url.read(cx).value().to_string();
        if raw.trim().is_empty() || self.connect.probing {
            return;
        }
        let url = normalize_url(&raw);
        self.connect.probing = true;
        self.connect.error = None;
        let client = Client::new(&url, &self.config.device_id);
        let probe_url = url.clone();
        self.fetch_with(
            client,
            cx,
            |client| client.public_info(),
            move |this, result, cx| {
                this.connect.probing = false;
                match result {
                    Ok(info) => {
                        let name = if info.server_name.is_empty() {
                            probe_url.clone()
                        } else {
                            info.server_name.clone()
                        };
                        this.config.upsert_server(Server {
                            id: info.id.clone(),
                            name,
                            url: probe_url.clone(),
                            profiles: Vec::new(),
                        });
                        this.save_config(cx);
                        this.rebuild_menu(cx);
                        this.select_server(info.id, cx);
                    }
                    Err(err) => {
                        log::warn!("probe of {probe_url} failed: {err:#}");
                        this.connect.error = Some(explain(&err, &probe_url));
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Ends a saved session on the server, so its token stops working.
    /// An instance that must not write the config does not own the saved
    /// sessions, so it leaves them alone.
    pub fn revoke_profile(&self, url: &str, profile: &Profile, cx: &mut Context<Self>) {
        if std::env::var_os("BLOOM_CONFIG_READONLY").is_some() {
            return;
        }
        let client = Client::new(url, &self.config.device_id)
            .with_session(&profile.token, &profile.user_id);
        self.fetch_with(
            client,
            cx,
            |client| client.logout(),
            |_, result, _| {
                if let Err(err) = result {
                    log::warn!("sign out on the server failed: {err:#}");
                }
            },
        );
    }

    /// Removes a saved profile and ends its session on the server.
    pub fn forget_profile(&mut self, server_id: &str, user_id: &str, cx: &mut Context<Self>) {
        if let Some(server) = self.config.server(server_id)
            && let Some(profile) = server.profiles.iter().find(|p| p.user_id == user_id)
        {
            self.revoke_profile(&server.url, profile, cx);
        }
        self.config.remove_profile(server_id, user_id);
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.server_id == server_id && s.user_id == user_id)
        {
            self.session = None;
        }
        self.save_config(cx);
        self.rebuild_menu(cx);
        cx.notify();
    }

    pub fn remove_server(&mut self, server_id: String, cx: &mut Context<Self>) {
        if let Some(server) = self.config.server(&server_id) {
            for profile in &server.profiles {
                self.revoke_profile(&server.url, profile, cx);
            }
        }
        self.config.remove_server(&server_id);
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.server_id == server_id)
        {
            self.session = None;
        }
        self.connect.public_users.clear();
        self.connect.quick = None;
        self.connect.error = None;
        self.save_config(cx);
        self.rebuild_menu(cx);
        match self.config.servers.first().map(|s| s.id.clone()) {
            Some(next) => {
                self.select_server(next, cx);
                self.connect.choosing_server = true;
            }
            None => self.connect.selected_server = None,
        }
        cx.notify();
    }

    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.connect.signing_in {
            return;
        }
        let Some(server_id) = self.connect.selected_server.clone() else {
            return;
        };
        let Some(server) = self.config.server(&server_id) else {
            return;
        };
        let username = self.connect.username.read(cx).value().to_string();
        let password = self.connect.password.read(cx).value().to_string();
        if username.trim().is_empty() {
            self.connect.error = Some("Enter a username.".into());
            cx.notify();
            return;
        }
        self.connect.signing_in = true;
        self.connect.quick = None;
        self.connect.error = None;
        let url = server.url.clone();
        let client = Client::new(&server.url, &self.config.device_id);
        self.fetch_with(
            client,
            cx,
            move |client| client.authenticate(username.trim(), &password),
            move |this, result, cx| {
                this.connect.signing_in = false;
                match result {
                    Ok(auth) => {
                        let profile = Profile {
                            user_id: auth.user.id.clone(),
                            name: auth.user.name.clone(),
                            token: auth.access_token.clone(),
                            image_tag: auth.user.primary_image_tag.clone(),
                        };
                        this.connect.clear_password = true;
                        this.add_profile(&server_id, profile, cx);
                    }
                    Err(err) => {
                        log::warn!("sign-in failed: {err:#}");
                        this.connect.error = Some(if format!("{err:#}").contains("401") {
                            "Wrong username or password.".to_string()
                        } else {
                            explain(&err, &url)
                        });
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    // ----- Quick Connect ------------------------------------------------------

    /// Starts a sign-in with a code: the server gives the code, the user
    /// approves it on a device that is signed in, and this app gets a session.
    pub fn start_quick_connect(&mut self, cx: &mut Context<Self>) {
        if self.connect.quick_starting || self.connect.quick.is_some() {
            return;
        }
        let Some(server_id) = self.connect.selected_server.clone() else {
            return;
        };
        let Some(server) = self.config.server(&server_id) else {
            return;
        };
        self.connect.quick_starting = true;
        self.connect.error = None;
        let url = server.url.clone();
        let client = Client::new(&server.url, &self.config.device_id);
        let waiter = client.clone();
        self.fetch_with(
            client,
            cx,
            |client| client.quick_connect_initiate(),
            move |this, result, cx| {
                this.connect.quick_starting = false;
                if this.connect.selected_server.as_deref() != Some(server_id.as_str()) {
                    return;
                }
                match result {
                    Ok(started) => {
                        let wait = this.wait_quick_connect(waiter, server_id, started.secret, cx);
                        this.connect.quick = Some(QuickSignIn {
                            code: started.code,
                            _wait: wait,
                        });
                    }
                    Err(err) => {
                        log::warn!("quick connect did not start: {err:#}");
                        this.connect.error = Some(if format!("{err:#}").contains("401") {
                            "Quick Connect is off on this server.".to_string()
                        } else {
                            explain(&err, &url)
                        });
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Asks the server every few seconds whether the code was approved, and
    /// signs in when it was.
    fn wait_quick_connect(
        &self,
        client: Client,
        server_id: String,
        secret: String,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            let outcome = loop {
                cx.background_executor().timer(QUICK_POLL).await;
                if started.elapsed() > QUICK_TIMEOUT {
                    break Err("The code expired. Start Quick Connect again.".to_string());
                }
                let (asker, key) = (client.clone(), secret.clone());
                let state = cx
                    .background_executor()
                    .spawn(async move { asker.quick_connect_state(&key) })
                    .await;
                match state {
                    Ok(state) if state.authenticated => {
                        let (asker, key) = (client.clone(), secret.clone());
                        let auth = cx
                            .background_executor()
                            .spawn(async move { asker.authenticate_quick_connect(&key) })
                            .await;
                        break auth.map_err(|err| {
                            log::warn!("quick connect sign-in failed: {err:#}");
                            "The server approved the code but did not sign in. Try again."
                                .to_string()
                        });
                    }
                    Ok(_) => {}
                    // The server forgets a code that was not used.
                    Err(err) if format!("{err:#}").contains("HTTP 404") => {
                        break Err("The code expired. Start Quick Connect again.".to_string());
                    }
                    // A network error can pass; the next question tells.
                    Err(err) => log::debug!("quick connect state: {err:#}"),
                }
            };
            this.update(cx, |this, cx| {
                this.connect.quick = None;
                match outcome {
                    Ok(auth) => {
                        let profile = Profile {
                            user_id: auth.user.id.clone(),
                            name: auth.user.name.clone(),
                            token: auth.access_token.clone(),
                            image_tag: auth.user.primary_image_tag.clone(),
                        };
                        this.add_profile(&server_id, profile, cx);
                    }
                    Err(text) => this.connect.error = Some(text),
                }
                cx.notify();
            })
            .ok();
        })
    }

    pub fn cancel_quick_connect(&mut self, cx: &mut Context<Self>) {
        self.connect.quick = None;
        cx.notify();
    }

    /// Opens the dialog that signs in another device with its code.
    pub fn open_authorize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        self.connect.authorize = Some(Authorize::default());
        self.connect.code.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub fn close_authorize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connect.authorize = None;
        window.focus(&self.app_focus, cx);
        cx.notify();
    }

    /// Approves the code in the dialog for the signed-in user.
    pub fn authorize_quick_connect(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.connect.authorize else {
            return;
        };
        if dialog.busy {
            return;
        }
        let code: String = self
            .connect
            .code
            .read(cx)
            .value()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect();
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            dialog.error = Some("The code has 6 digits.".to_string());
            cx.notify();
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        self.fetch(
            cx,
            move |client| client.quick_connect_authorize(&code),
            |this, result, cx| {
                match result {
                    Ok(()) => {
                        this.connect.authorize = None;
                        this.toast(
                            "Device signed in",
                            "The other device can continue now.",
                            cx,
                        );
                    }
                    Err(err) => {
                        log::warn!("quick connect authorize failed: {err:#}");
                        if let Some(dialog) = &mut this.connect.authorize {
                            dialog.busy = false;
                            let text = format!("{err:#}");
                            dialog.error = Some(if text.contains("HTTP 404") {
                                "The server does not know this code. Check it and try again."
                                    .to_string()
                            } else if text.contains("HTTP 403") || text.contains("401") {
                                "The server did not accept the code.".to_string()
                            } else {
                                "The server could not be reached.".to_string()
                            });
                        }
                    }
                }
                cx.notify();
            },
        );
        cx.notify();
    }

    /// True at the second click on the remove button `key`. The first
    /// click only arms the button.
    fn confirmed(&mut self, key: String, cx: &mut Context<Self>) -> bool {
        if self.connect.confirm.as_deref() == Some(key.as_str()) {
            self.connect.confirm = None;
            return true;
        }
        self.connect.confirm = Some(key);
        cx.notify();
        false
    }

    /// Tab and Shift+Tab move between the fields of the sign-in form.
    fn focus_next_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let username = self.connect.username.read(cx).focus_handle(cx);
        let password = self.connect.password.read(cx).focus_handle(cx);
        if username.contains_focused(window, cx) {
            window.focus(&password, cx);
        } else if password.contains_focused(window, cx) {
            window.focus(&username, cx);
        }
    }

    pub fn render_connect(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self
            .connect
            .selected_server
            .as_ref()
            .filter(|_| !self.connect.choosing_server)
            .and_then(|id| self.config.server(id))
            .cloned();

        // The first field takes the focus, so the user can type at once.
        if window.focused(cx).is_none() && self.connect.authorize.is_none() {
            let field = match &server {
                Some(_) => &self.connect.username,
                None => &self.connect.url,
            };
            let focus = field.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }

        // The server makes an image of its library art for this page.
        let splash = server
            .as_ref()
            .filter(|_| self.connect.branding.splashscreen_enabled)
            .map(|s| Client::new(&s.url, &self.config.device_id).splashscreen_url());

        let content = match &server {
            Some(server) if self.connect.quick.is_some() => self.render_quick(server, cx),
            Some(server) => self.render_sign_in(server, cx),
            None => self.render_servers(cx),
        };
        let card = div()
            .relative()
            .w(px(CARD_W.min(self.viewport_w - 32.)))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(rgba(0xf5f5f726))
            .child(glass(px(CARD_RADIUS), rgba(0x1a1a1acc)))
            .child(content.relative().p(px(32.)).flex().flex_col().gap(px(22.)));

        div()
            .relative()
            .size_full()
            .bg(rgb(0x101010))
            .text_color(rgb(0xf5f5f7))
            .when_some(splash, |el, url| {
                el.child(
                    crate::images::remote_with(url, px(0.), ObjectFit::Cover)
                        .absolute()
                        .inset_0(),
                )
            })
            .child(div().absolute().inset_0().bg(rgba(0x0000008c)))
            .child(
                ScrollArea::new("connect.scroll").size_full().child(
                    div()
                        .w_full()
                        .min_h(px(self.viewport_h))
                        .py(px(64.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(18.))
                        .child(card)
                        // The name of the app and its line.
                        .child(
                            div()
                                .relative()
                                .text_size(px(13.))
                                .text_color(rgba(0xf5f5f799))
                                .child(format!("{} · {}", crate::brand::NAME, crate::brand::TAGLINE)),
                        ),
                ),
            )
            // The window moves when the user drags its top edge.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(48.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()),
            )
            .when_some(self.session.as_ref(), |el, session| {
                el.child(
                    div()
                        .id("connect.back")
                        .absolute()
                        .top(px(14.))
                        .left(px(92.))
                        .h(px(36.))
                        .px(px(14.))
                        .rounded(px(18.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .text_size(px(14.))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(glass(px(18.), rgba(0x2a2a2a99)))
                        .child(icon(LucideIcon::ArrowLeft, 16., rgb(0xf5f5f7)).relative())
                        .child(
                            div()
                                .relative()
                                .child(format!("Back to {}", session.user_name)),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.connect.quick = None;
                            this.connect.choosing_server = false;
                            this.screen = crate::app::Screen::Main;
                            window.focus(&this.app_focus, cx);
                            cx.notify();
                        })),
                )
            })
    }

    /// Mark, title and a line under it, at the top of the card.
    fn card_header(&self, title: impl Into<SharedString>, note: impl Into<SharedString>) -> Div {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .child(crate::icons::logo(44., rgb(0xf5f5f7)))
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(24.))
                    .line_height(px(30.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_center()
                    .child(title.into()),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(rgba(0xf5f5f799))
                    .text_center()
                    .child(note.into()),
            )
    }

    fn error_note(&self) -> Option<Div> {
        let error = self.connect.error.clone()?;
        Some(
            div()
                .px(px(14.))
                .py(px(10.))
                .rounded(px(12.))
                .bg(rgba(0xef535026))
                .border_1()
                .border_color(rgba(0xef535066))
                .flex()
                .items_start()
                .gap(px(10.))
                .text_size(px(13.))
                .text_color(rgb(0xffb4ab))
                .child(
                    div()
                        .mt(px(2.))
                        .flex_shrink_0()
                        .child(icon(LucideIcon::CircleAlert, 15., rgb(0xffb4ab))),
                )
                .child(div().min_w_0().child(error)),
        )
    }

    /// The server step: saved servers and the address field.
    fn render_servers(&self, cx: &mut Context<Self>) -> Div {
        let mut rows: Vec<Stateful<Div>> = Vec::new();
        for server in &self.config.servers {
            let id = server.id.clone();
            let remove = server.id.clone();
            let armed = self.connect.confirm.as_deref() == Some(&format!("server.{remove}"));
            rows.push(
                div()
                    .id(SharedString::from(format!("server.{}", server.id)))
                    .h(px(60.))
                    .px(px(14.))
                    .rounded(px(14.))
                    .bg(rgba(0xffffff0f))
                    .hover(|s| s.bg(rgba(0xffffff1f)))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(icon(LucideIcon::Server, 18., rgba(0xf5f5f7cc)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_size(px(15.))
                                    .truncate()
                                    .child(server.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgba(0xf5f5f780))
                                    .truncate()
                                    .child(format!(
                                        "{} · {} profile{}",
                                        host(&server.url),
                                        server.profiles.len(),
                                        if server.profiles.len() == 1 { "" } else { "s" }
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("server.remove.{}", server.id)))
                            .size(px(30.))
                            .rounded(px(10.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|s| s.bg(rgba(0xef535040)))
                            .when(armed, |el| el.bg(rgb(0xc62828)))
                            .child(icon(
                                if armed { LucideIcon::Check } else { LucideIcon::Trash },
                                15.,
                                if armed { rgb(0xffffff) } else { rgba(0xf5f5f799) },
                            ))
                            .tooltip(tip(if armed {
                                "Click again to remove"
                            } else {
                                "Remove this server"
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if this.confirmed(format!("server.{remove}"), cx) {
                                    this.remove_server(remove.clone(), cx);
                                }
                            })),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_server(id.clone(), cx)),
                    ),
            );
        }

        let probing = self.connect.probing;
        div()
            .child(self.card_header(
                "Connect to a server",
                "Enter the address of your Jellyfin server.",
            ))
            .when(!rows.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(label("Your servers"))
                        .children(rows),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(label("Server address"))
                    .child(
                        Input::new(&self.connect.url)
                            .aria_label("Server address")
                            .disabled(probing)
                            .w_full()
                            .h(px(FIELD_H)),
                    )
                    .children(self.error_note())
                    .child(
                        action(
                            "connect.add",
                            if probing { "Connecting…" } else { "Connect" },
                            true,
                            probing,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.add_server(cx))),
                    ),
            )
            .when(self.connect.selected_server.is_some(), |el| {
                el.child(div().flex().justify_center().child(
                    link("connect.servers.back", "Back to sign in").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.connect.choosing_server = false;
                            this.connect.error = None;
                            cx.notify();
                        },
                    )),
                ))
            })
    }

    /// The user step: saved profiles, the users the server shows, the form.
    fn render_sign_in(&self, server: &Server, cx: &mut Context<Self>) -> Div {
        let client = Client::new(&server.url, &self.config.device_id);

        // Saved profiles: one click opens the session.
        let mut saved: Vec<Stateful<Div>> = Vec::new();
        for profile in &server.profiles {
            let image = profile
                .image_tag
                .as_ref()
                .map(|tag| client.user_image_url(&profile.user_id, tag));
            let (sid, uid) = (server.id.clone(), profile.user_id.clone());
            let (sid2, uid2) = (server.id.clone(), profile.user_id.clone());
            let armed = self.connect.confirm.as_deref() == Some(&format!("profile.{uid2}"));
            saved.push(
                person(
                    SharedString::from(format!("profile.{}.{}", server.id, profile.user_id)),
                    &profile.name,
                    image,
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "forget.{}.{}",
                            server.id, profile.user_id
                        )))
                        .absolute()
                        .top(px(4.))
                        .right(px(4.))
                        .size(px(22.))
                        .rounded(px(11.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if armed { rgb(0xc62828) } else { rgba(0x00000066) })
                        .hover(|s| s.bg(rgba(0xef5350cc)))
                        .child(icon(
                            if armed { LucideIcon::Check } else { LucideIcon::X },
                            12.,
                            rgb(0xf5f5f7),
                        ))
                        .tooltip(tip(if armed {
                            "Click again to forget"
                        } else {
                            "Forget this profile"
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if this.confirmed(format!("profile.{uid2}"), cx) {
                                this.forget_profile(&sid2, &uid2, cx);
                            }
                        })),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    window.focus(&this.app_focus, cx);
                    this.open_session(&sid, &uid, cx);
                })),
            );
        }

        // Users the server shows that are not saved: a click fills the form.
        let mut public: Vec<Stateful<Div>> = Vec::new();
        for user in &self.connect.public_users {
            if server.profiles.iter().any(|p| p.user_id == user.id) {
                continue;
            }
            let image = user
                .primary_image_tag
                .as_ref()
                .map(|tag| client.user_image_url(&user.id, tag));
            let name = user.name.clone();
            let needs_password = user.has_password;
            public.push(
                person(
                    SharedString::from(format!("public.{}", user.id)),
                    &user.name,
                    image,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.connect
                        .username
                        .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                    this.connect
                        .password
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    if needs_password {
                        let focus = this.connect.password.read(cx).focus_handle(cx);
                        window.focus(&focus, cx);
                    } else {
                        this.sign_in(cx);
                    }
                })),
            );
        }

        let busy = self.connect.signing_in;
        let form = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            // Tab is the indent key of the text field; here it moves on.
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                this.focus_next_field(window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, window, cx| {
                this.focus_next_field(window, cx);
                cx.stop_propagation();
            }))
            .when(!saved.is_empty() || !public.is_empty(), |el| {
                el.child(label("Sign in as another user"))
            })
            .child(
                Input::new(&self.connect.username)
                    .aria_label("Username")
                    .disabled(busy)
                    .w_full()
                    .h(px(FIELD_H)),
            )
            .child(
                Input::new(&self.connect.password)
                    .aria_label("Password")
                    .disabled(busy)
                    .w_full()
                    .h(px(FIELD_H)),
            )
            .children(self.error_note())
            .child(
                action(
                    "connect.signin",
                    if busy { "Signing in…" } else { "Sign In" },
                    true,
                    busy,
                )
                .on_click(cx.listener(|this, _, _, cx| this.sign_in(cx))),
            )
            .when(self.connect.quick_enabled, |el| {
                let starting = self.connect.quick_starting;
                el.child(
                    action(
                        "connect.quick",
                        if starting {
                            "Getting a code…"
                        } else {
                            "Use Quick Connect"
                        },
                        false,
                        starting || busy,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.start_quick_connect(cx))),
                )
            });

        let remove = server.id.clone();
        let armed = self.connect.confirm.as_deref() == Some(&format!("server.{remove}"));
        let disclaimer = self
            .connect
            .branding
            .login_disclaimer
            .clone()
            .filter(|text| !text.trim().is_empty());
        div()
            .child(self.card_header(
                if saved.is_empty() {
                    "Please sign in"
                } else {
                    "Who is watching?"
                },
                server_line(server),
            ))
            .when(!saved.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap(px(8.))
                        .children(saved),
                )
            })
            .when(!public.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(label("Users on this server"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_center()
                                .gap(px(8.))
                                .children(public),
                        ),
                )
            })
            .child(form)
            .when_some(disclaimer, |el, text| {
                el.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgba(0xf5f5f780))
                        .text_center()
                        .child(text),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_center()
                    .gap(px(20.))
                    .child(link("connect.change", "Change server").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.connect.choosing_server = true;
                            this.connect.quick = None;
                            this.connect.error = None;
                            cx.notify();
                        },
                    )))
                    .child(
                        link(
                            "connect.remove-server",
                            if armed {
                                "Click again to remove"
                            } else {
                                "Remove server"
                            },
                        )
                        .when(armed, |el| el.text_color(rgb(0xff8a80)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.confirmed(format!("server.{remove}"), cx) {
                                this.remove_server(remove.clone(), cx);
                            }
                        })),
                    ),
            )
    }

    /// The Quick Connect step: the code, and a wait for the approval.
    fn render_quick(&self, server: &Server, cx: &mut Context<Self>) -> Div {
        let code = self
            .connect
            .quick
            .as_ref()
            .map(|quick| quick.code.clone())
            .unwrap_or_default();
        let mut digits = div().flex().justify_center().gap(px(8.));
        for (i, digit) in code.chars().enumerate() {
            digits = digits.child(
                div()
                    .w(px(48.))
                    .h(px(62.))
                    // A wider gap in the middle makes the code easy to read.
                    .when(i == 3, |el| el.ml(px(10.)))
                    .rounded(px(14.))
                    .bg(rgba(0xffffff14))
                    .border_1()
                    .border_color(rgba(0xf5f5f733))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(30.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(digit.to_string()),
            );
        }
        div()
            .child(self.card_header(
                "Quick Connect",
                server_line(server),
            ))
            .child(digits)
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(rgba(0xf5f5f7cc))
                    .text_center()
                    .child(
                        "On a device where you are signed in, open Quick Connect in the user \
                         menu and enter this code.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .text_color(rgba(0xf5f5f780))
                    .child(icon(LucideIcon::Clock, 14., rgba(0xf5f5f780)))
                    .child("Waiting for the approval…"),
            )
            .child(
                action("connect.quick.cancel", "Cancel", false, false)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_quick_connect(cx))),
            )
    }

    /// The dialog that signs in another device with the code it shows.
    pub fn render_authorize(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.connect.authorize.as_ref()?;
        let name = self
            .session
            .as_ref()
            .map(|s| s.user_name.clone())
            .unwrap_or_default();
        let busy = dialog.busy;
        Some(
            div()
                .id("quick.authorize")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000099))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_authorize(window, cx);
                    }
                    // The keys of the dialog are not shortcuts of the page.
                    cx.stop_propagation();
                }))
                // A click beside the dialog closes it.
                .on_click(cx.listener(|this, _, window, cx| this.close_authorize(window, cx)))
                .child(
                    div()
                        .id("quick.authorize.card")
                        .relative()
                        .w(px(420.))
                        .rounded(px(24.))
                        .border_1()
                        .border_color(rgba(0xf5f5f733))
                        .child(glass(px(24.), crate::ui::glass::POPUP_TINT))
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .relative()
                                .p(px(24.))
                                .flex()
                                .flex_col()
                                .gap(px(14.))
                                .text_color(rgb(0xf5f5f7))
                                .child(
                                    div()
                                        .text_size(px(20.))
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .child("Quick Connect"),
                                )
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .text_color(rgba(0xf5f5f7cc))
                                        .child(format!(
                                            "Enter the code that the other device shows. The \
                                             device then signs in as {name}."
                                        )),
                                )
                                .child(
                                    Input::new(&self.connect.code)
                                        .aria_label("Quick Connect code")
                                        .disabled(busy)
                                        .invalid(dialog.error.is_some())
                                        .w_full()
                                        .h(px(FIELD_H)),
                                )
                                .when_some(dialog.error.clone(), |el, error| {
                                    el.child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(rgb(0xffb4ab))
                                            .child(error),
                                    )
                                })
                                .child(
                                    div()
                                        .mt(px(4.))
                                        .flex()
                                        .gap(px(10.))
                                        .child(
                                            action("quick.authorize.cancel", "Cancel", false, false)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.close_authorize(window, cx)
                                                })),
                                        )
                                        .child(
                                            action(
                                                "quick.authorize.run",
                                                if busy { "Authorizing…" } else { "Authorize" },
                                                true,
                                                busy,
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.authorize_quick_connect(cx)
                                            })),
                                        ),
                                ),
                        ),
                ),
        )
    }
}

/// Width of the sign-in card.
const CARD_W: f32 = 440.;
const CARD_RADIUS: f32 = 28.;
/// Height of the text fields and the main buttons.
const FIELD_H: f32 = 44.;
/// Time between two questions about a Quick Connect code.
const QUICK_POLL: Duration = Duration::from_secs(3);
/// The app stops the wait for an approval after this time.
const QUICK_TIMEOUT: Duration = Duration::from_secs(600);

/// Line under the title: the server name, and its address when that says more.
fn server_line(server: &Server) -> String {
    let host = host(&server.url);
    if server.name == host {
        host.to_string()
    } else {
        format!("{} · {host}", server.name)
    }
}

/// "media.example.com" out of a server address.
fn host(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
}

/// Small heading over a group of the card.
fn label(text: &'static str) -> Div {
    div()
        .text_size(px(12.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(rgba(0xf5f5f780))
        .child(text)
}

/// A button across the card. `primary` gives it the accent fill; `busy`
/// greys it and takes the clicks away.
fn action(
    id: &'static str,
    text: impl Into<SharedString>,
    primary: bool,
    busy: bool,
) -> Stateful<Div> {
    let (bg, fg) = if primary {
        (rgb(0xf5f5f7), rgb(0x121212))
    } else {
        (rgba(0xffffff1f), rgb(0xf5f5f7))
    };
    div()
        .id(id)
        .flex_1()
        .min_h(px(FIELD_H))
        .h(px(FIELD_H))
        .px(px(16.))
        .rounded(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .bg(bg)
        .text_color(fg)
        .text_size(px(15.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .when(busy, |el| el.opacity(0.6).occlude())
        .when(!busy, |el| el.cursor_pointer().hover(|s| s.opacity(0.88)))
        .child(text.into())
}

/// A quiet text button.
fn link(id: &'static str, text: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .text_size(px(13.))
        .text_color(rgba(0xf5f5f799))
        .hover(|s| s.text_color(rgb(0xf5f5f7)))
        .child(text)
}

/// A user as a round picture with the name under it.
fn person(id: SharedString, name: &str, image: Option<String>) -> Stateful<Div> {
    // A colour of the name, so users without a picture differ.
    let hue = name.bytes().fold(0u32, |sum, b| sum.wrapping_mul(31) + b as u32) % 360;
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default();
    div()
        .id(id)
        .relative()
        .w(px(104.))
        .py(px(12.))
        .rounded(px(16.))
        .cursor_pointer()
        .hover(|s| s.bg(rgba(0xffffff14)))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .relative()
                .size(px(72.))
                .rounded(px(36.))
                .bg(gpui_kit::hsla(hue as f32 / 360., 0.45, 0.42, 1.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(28.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(initial)
                .when_some(image, |el, url| {
                    el.child(
                        crate::images::remote_with(url, px(36.), ObjectFit::Cover)
                            .absolute()
                            .inset_0(),
                    )
                }),
        )
        .child(
            div()
                .w_full()
                .px(px(6.))
                .text_size(px(14.))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_center()
                .truncate()
                .child(name.to_string()),
        )
}
