use super::{Page, SharedState};
use crate::{
    cloud::{Client, User, UserManager},
    get_data, get_data_mut, save_data,
    task::Task,
    Rect, Ui,
};
use anyhow::{Context, Result};
use image::imageops::FilterType;
use macroquad::prelude::Touch;
use once_cell::sync::Lazy;
use prpr::{
    scene::{request_file, request_input, return_file, return_input, show_error, show_message, take_file, take_input},
    ui::RectButton,
};
use regex::Regex;
use serde_json::json;
use std::{future::Future, io::Cursor};

fn validate_username(username: &str) -> Option<&'static str> {
    if !(4..=20).contains(&username.len()) {
        return Some("Username length should be between 4 and 20");
    }
    if username.chars().any(|it| it != '_' && it != '-' && !it.is_alphanumeric()) {
        return Some("Username contains illegal characters");
    }
    None
}

pub struct AccountPage {
    register: bool,
    task: Option<Task<Result<Option<User>>>>,
    task_desc: String,
    email_input: String,
    username_input: String,
    password_input: String,
    avatar_button: RectButton,
}

impl AccountPage {
    pub fn new() -> Self {
        let logged_in = get_data().me.is_some();
        Self {
            register: false,
            task: if logged_in {
                Some(Task::new(async { Ok(Some(Client::get_me().await?)) }))
            } else {
                None
            },
            task_desc: if logged_in { "Update data".to_owned() } else { String::new() },
            email_input: String::new(),
            username_input: String::new(),
            password_input: String::new(),
            avatar_button: RectButton::new(),
        }
    }

    pub fn start(&mut self, desc: impl Into<String>, future: impl Future<Output = Result<Option<User>>> + Send + 'static) {
        self.task_desc = desc.into();
        self.task = Some(Task::new(future));
    }
}

impl Page for AccountPage {
    fn label(&self) -> &'static str {
        "账户"
    }

    fn update(&mut self, _focus: bool, _state: &mut SharedState) -> Result<()> {
        if let Some(task) = self.task.as_mut() {
            if let Some(result) = task.take() {
                let desc = &self.task_desc;
                match result {
                    Err(err) => {
                        show_error(err.context(format!("{desc}failed")));
                    }
                    Ok(user) => {
                        if let Some(user) = user {
                            UserManager::request(&user.id);
                            get_data_mut().me = Some(user);
                            save_data()?;
                        }
                        show_message(format!("{desc}successful"));
                        if desc == "Register" {
                            show_message("The verification information has been sent to the email, please verify and log in");
                        }
                        self.register = false;
                    }
                }
                self.task = None;
            }
        }

        if let Some((id, text)) = take_input() {
            if id == "edit_username" {
                if let Some(error) = validate_username(&text) {
                    show_message(error);
                } else {
                    let user = get_data().me.clone().unwrap();
                    self.start("Update the name", async move {
                        Client::update_user(json!({ "username": text })).await?;
                        Ok(Some(User { name: text, ..user }))
                    });
                }
            } else {
                return_input(id, text);
            }
        }
        if let Some((id, file)) = take_file() {
            if id == "avatar" {
                let mut load = |path: String| -> Result<()> {
                    let image = image::load_from_memory(&std::fs::read(path).context("Unable to read the picture")?)
                        .context("Unable to load image")?
                        .resize_exact(512, 512, FilterType::CatmullRom);
                    let mut bytes: Vec<u8> = Vec::new();
                    image.write_to(&mut Cursor::new(&mut bytes), image::ImageOutputFormat::Png)?;
                    let old_avatar = get_data().me.as_ref().unwrap().avatar.clone();
                    let user = get_data().me.clone().unwrap();
                    self.start("Upload an avatar", async move {
                        let file = Client::upload_file("avatar.png", &bytes).await.context("Failed to upload avatar")?;
                        if let Some(old) = old_avatar {
                            Client::delete_file(&old.id).await.context("Failed to delete the original avatar")?;
                        }
                        Client::update_user(json!({ "avatar": {
                                "id": file.id,
                                "__type": "File"
                            } }))
                        .await
                        .context("Failed to update avatar")?;
                        UserManager::clear_cache(&user.id);
                        Ok(Some(User { avatar: Some(file), ..user }))
                    });
                    Ok(())
                };
                if let Err(err) = load(file) {
                    show_error(err.context("Failed to import avatar"));
                }
            } else {
                return_file(id, file);
            }
        }
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, _state: &mut SharedState) -> Result<bool> {
        if self.task.is_none() && get_data().me.is_some() && self.avatar_button.touch(touch) {
            request_file("avatar");
            return Ok(true);
        }
        Ok(false)
    }

    fn render(&mut self, ui: &mut Ui, _state: &mut SharedState) -> Result<()> {
        ui.dx(0.02);
        let r = Rect::new(0., 0., 0.22, 0.22);
        self.avatar_button.set(ui, r);
        if let Some(avatar) = get_data().me.as_ref().and_then(|it| UserManager::get_avatar(&it.id)) {
            let ct = r.center();
            ui.fill_circle(ct.x, ct.y, r.w / 2., (*avatar, r));
        }
        ui.text(get_data().me.as_ref().map(|it| it.name.as_str()).unwrap_or("[Not logged in]"))
            .pos(r.right() + 0.02, r.center().y)
            .anchor(0., 0.5)
            .size(0.8)
            .draw();
        ui.dy(r.h + 0.03);
        if get_data().me.is_none() {
            let r = ui.text("User name").size(0.4).measure();
            ui.dx(r.w);
            if self.register {
                let r = ui.input("Email", &mut self.email_input, ());
                ui.dy(r.h + 0.02);
            }
            let r = ui.input("User name", &mut self.username_input, ());
            ui.dy(r.h + 0.02);
            let r = ui.input("Password", &mut self.password_input, true);
            ui.dy(r.h + 0.02);
            let labels = if self.register {
                ["Back", if self.task.is_none() { "Register" } else { "Register in…" }]
            } else {
                ["Register", if self.task.is_none() { "Login" } else { "Register in…" }]
            };
            let cx = r.right() / 2.;
            let mut r = Rect::new(0., 0., cx - 0.01, r.h);
            if ui.button("left", r, labels[0]) {
                self.register ^= true;
            }
            r.x = cx + 0.01;
            if ui.button("right", r, labels[1]) {
                let mut login = || -> Option<&'static str> {
                    let username = self.username_input.clone();
                    let password = self.password_input.clone();
                    if let Some(error) = validate_username(&username) {
                        return Some(error);
                    }
                    if !(6..=26).contains(&password.len()) {
                        return Some("Password length should be between 6 and 26");
                    }
                    if self.register {
                        let email = self.email_input.clone();
                        static EMAIL_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[\w\-\.]+@([\w\-]+\.)+[\w\-]{2,4}$").unwrap());
                        if !EMAIL_REGEX.is_match(&email) {
                            return Some("Email is not legal");
                        }
                        self.start("Register", async move {
                            Client::register(&email, &username, &password).await?;
                            Ok(None)
                        });
                    } else {
                        self.start("Login", async move {
                            let user = Client::login(&username, &password).await?;
                            Ok(Some(user))
                        });
                    }
                    None
                };
                if let Some(err) = login() {
                    show_message(err);
                }
            }
        } else {
            let cx = 0.2;
            let mut r = Rect::new(0., 0., cx - 0.01, ui.text("呃").size(0.42).measure().h + 0.02);
            if ui.button("logout", r, "Logout") && self.task.is_none() {
                get_data_mut().me = None;
                let _ = save_data();
                show_message("Logout successful");
            }
            r.x = cx + 0.01;
            if ui.button("edit_name", r, "Modify name") && self.task.is_none() {
                request_input("edit_username", &get_data().me.as_ref().unwrap().name);
            }
        }
        Ok(())
    }
}
