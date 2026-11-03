//! Rime in COSMIC's panel (docs/nixos/desktops.md §6, D10).
//!
//!   Gaming       a toggle: rimed `Mode.Set("gaming"|"daily")`; "Game running" while `GameMode.Active`.
//!   Update       shown while rimed's `Updates.State` is available (click: `Updates.Apply`, COSMIC's polkit agent asks
//!                for an administrator), downloading/installing (progress, not clickable) or restart (click: COSMIC's
//!                own restart dialog).
//!   Open Rime Settings   `rime-settings`, the app for sessions without Rime Shell.
//!
//! Everything is read by rime-cosmic-model, which follows PropertiesChanged; this file only draws.
//!
//! Test hook: when a process owns `org.rimeos.QuickSettingsTest` on the session bus (only the VM tests run one), every
//! redraw is reported to it as JSON. Nothing is sent otherwise.

use cosmic::{
    Element, Task,
    app::{self, Core},
    applet::{menu_button, padded_control},
    iced::{
        Length, Subscription,
        futures::SinkExt,
        platform_specific::shell::wayland::commands::popup::destroy_popup,
        stream,
        widget::{column, row},
        window::Id,
    },
    widget::{divider, text, toggler},
};
use rime_cosmic_model::View;

const ID: &str = "org.rimeos.CosmicAppletRime";
const TEST_NAME: &str = "org.rimeos.QuickSettingsTest";

struct Applet {
    core: Core,
    popup: Option<Id>,
    view: View,
}

#[derive(Clone, Debug)]
enum Message {
    TogglePopup,
    PopupClosed(Id),
    Rimed(View),
    ToggleGaming(bool),
    Update,
    OpenSettings(&'static str),
    Done,
}

fn spawn(program: &str, args: &[&str]) {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    tokio::spawn(cosmic::process::spawn(cmd));
}

/// Sends what is drawn to the VM tests' sink, if one runs.
async fn report(view: View) {
    let Ok(conn) = zbus::Connection::session().await else { return };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return };
    if !dbus.name_has_owner(TEST_NAME.try_into().expect("name")).await.unwrap_or(false) {
        return;
    }
    let json = format!(
        "{{\"gaming\":{{\"checked\":{},\"subtitle\":{:?},\"reactive\":{}}},\"update\":{{\"visible\":{},\"title\":{:?},\"subtitle\":{:?}}}}}",
        view.gaming_checked, view.gaming_subtitle, view.rimed, view.update_visible, view.update_title, view.update_subtitle,
    );
    let _ = conn
        .call_method(Some(TEST_NAME), "/org/rimeos/QuickSettingsTest", Some(TEST_NAME), "Report", &("cosmic", json))
        .await;
}

impl cosmic::Application for Applet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, app::Task<Message>) {
        (Self { core, popup: None, view: View::default() }, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        struct Follow;
        Subscription::run_with(std::any::TypeId::of::<Follow>(), |_| {
            stream::channel(16, async |mut out: cosmic::iced::futures::channel::mpsc::Sender<Message>| {
                match zbus::Connection::system().await {
                    Ok(conn) => {
                        let mut tx = out.clone();
                        if let Err(e) = rime_cosmic_model::follow(&conn, move |v| {
                            let _ = tx.try_send(Message::Rimed(v));
                        })
                        .await
                        {
                            tracing::warn!("rime: lost rimed: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("rime: no system bus: {e}"),
                }
                // The system bus is gone (the session is ending): nothing more to follow, and nothing to poll.
                let _ = out.send(Message::Done).await;
                std::future::pending::<()>().await;
            })
        })
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                        |_| Default::default(),
                        |app: &mut Self| {
                            let new_id = Id::unique();
                            app.popup = Some(new_id);
                            app.core.applet.get_popup_settings(app.core.main_window_id().unwrap(), new_id, None, None, None)
                        },
                        None,
                    ))
                };
            }
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Rimed(v) => {
                self.view = v.clone();
                tokio::spawn(report(v));
            }
            Message::ToggleGaming(on) => {
                // The toggle shows Mode.Active; rimed's answer comes back as a property.
                tokio::spawn(async move {
                    match zbus::Connection::system().await {
                        Ok(conn) => {
                            if let Err(e) = rime_cosmic_model::set_gaming(&conn, on).await {
                                tracing::warn!("rime: Mode.Set refused: {e}");
                            }
                        }
                        Err(e) => tracing::warn!("rime: no system bus: {e}"),
                    }
                });
            }
            Message::Update => match self.view.update_state.as_str() {
                "available" => {
                    tokio::spawn(async {
                        if let Ok(conn) = zbus::Connection::system().await {
                            if let Err(e) = rime_cosmic_model::apply_update(&conn).await {
                                tracing::warn!("rime: Updates.Apply refused: {e}");
                            }
                        }
                    });
                }
                // COSMIC's own restart dialog (it asks and counts down).
                "restart" => spawn("cosmic-osd", &["restart"]),
                _ => {}
            },
            Message::OpenSettings(page) => {
                if page.is_empty() {
                    spawn("rime-settings", &[]);
                } else {
                    spawn("rime-settings", &[page]);
                }
            }
            Message::Done => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let icon = if self.view.update_state == "restart" {
            "system-reboot-symbolic"
        } else if self.view.update_visible {
            "software-update-available-symbolic"
        } else {
            "input-gaming-symbolic"
        };
        self.core.applet.icon_button(icon).on_press_down(Message::TogglePopup).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        let v = &self.view;
        let mut gaming = toggler(v.gaming_checked).label(String::from("Gaming")).text_size(14).width(Length::Fill);
        if v.rimed {
            gaming = gaming.on_toggle(Message::ToggleGaming);
        }
        let mut content = column![padded_control(gaming)].padding([8, 0]);
        if !v.gaming_subtitle.is_empty() {
            content = content.push(padded_control(text::caption(v.gaming_subtitle.clone())));
        }
        if v.update_visible {
            let label = if v.update_subtitle.is_empty() {
                v.update_title.clone()
            } else {
                format!("{} — {}", v.update_title, v.update_subtitle)
            };
            let mut button = menu_button(text::body(label));
            if v.update_sensitive {
                button = button.on_press(Message::Update);
            }
            content = content.push(padded_control(divider::horizontal::default())).push(button);
        }
        content = content
            .push(padded_control(divider::horizontal::default()))
            .push(menu_button(row![text::body("Gaming settings")]).on_press(Message::OpenSettings("gaming")))
            .push(menu_button(text::body("Open Rime Settings")).on_press(Message::OpenSettings("")));
        self.core.applet.popup_container(content).into()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    cosmic::applet::run::<Applet>(())
}
