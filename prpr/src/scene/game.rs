use super::{draw_background, request_input, return_input, show_message, take_input, EndingScene, NextScene, Scene};
use crate::{
    config::Config,
    core::{copy_fbo, BadNote, Chart, ChartExtra, Effect, Point, Resource, UIElement, Vector, JUDGE_LINE_GOOD_COLOR, JUDGE_LINE_PERFECT_COLOR},
    ext::{screen_aspect, RectExt, SafeTexture},
    fs::FileSystem,
    info::{ChartFormat, ChartInfo},
    judge::Judge,
    parse::{parse_extra, parse_pec, parse_phigros, parse_rpe},
    time::TimeManager,
    ui::{RectButton, Ui},
};
use anyhow::{bail, Context, Result};
use concat_string::concat_string;
use macroquad::{prelude::*, window::InternalGlContext};
use sasa::{Music, MusicParams};
use std::{
    io::ErrorKind,
    ops::{DerefMut, Range},
    path::PathBuf,
    process::{Command, Stdio},
    rc::Rc,
    sync::Mutex,
};

pub static FFMPEG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

const WAIT_TIME: f32 = 0.5;
const AFTER_TIME: f32 = 0.7;

fn fmt_time(t: f32) -> String {
    let f = t < 0.;
    let t = t.abs();
    let secs = t % 60.;
    let mut t = (t / 60.) as u64;
    let mins = t % 60;
    t /= 60;
    let hrs = t % 100;
    format!("{}{hrs:02}:{mins:02}:{secs:05.2}", if f { "-" } else { "" })
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    fn on_game_start();
}

#[derive(PartialEq, Eq)]
pub enum GameMode {
    Normal,
    TweakOffset,
    Exercise,
}

#[derive(Clone)]
enum State {
    Starting,
    BeforeMusic,
    Playing,
    Ending,
}

pub struct GameScene {
    should_exit: bool,
    next_scene: Option<NextScene>,

    pub mode: GameMode,
    pub res: Resource,
    pub chart: Chart,
    pub judge: Judge,
    pub gl: InternalGlContext<'static>,
    info_offset: f32,
    compatible_mode: bool,
    effects: Vec<Effect>,

    first_in: bool,
    exercise_range: Range<f32>,
    exercise_press: Option<(i8, u64)>,
    exercise_btns: (RectButton, RectButton),

    pub music: Music,

    get_size_fn: Rc<dyn Fn() -> (u32, u32)>,

    state: State,
    last_update_time: f64,
    pause_rewind: Option<f64>,

    bad_notes: Vec<BadNote>,
}

macro_rules! reset {
    ($self:ident, $res:expr, $tm:ident) => {{
        $self.bad_notes.clear();
        $self.judge.reset();
        $self.chart.reset();
        $res.judge_line_color = JUDGE_LINE_PERFECT_COLOR;
        $self.music.pause()?;
        $self.music.seek_to(0.)?;
        $tm.reset();
        $self.last_update_time = $tm.now();
        $self.state = State::Starting;
    }};
}

impl GameScene {
    pub const BEFORE_TIME: f32 = 0.7;
    pub const FADEOUT_TIME: f32 = WAIT_TIME + AFTER_TIME + 0.3;

    pub async fn load_chart(fs: &mut Box<dyn FileSystem>, info: &ChartInfo) -> Result<Chart> {
        async fn load_chart_bytes(fs: &mut Box<dyn FileSystem>, info: &ChartInfo) -> Result<Vec<u8>> {
            if let Ok(bytes) = fs.load_file(&info.chart).await {
                return Ok(bytes);
            }
            if let Some(name) = info.chart.strip_suffix(".pec") {
                if let Ok(bytes) = fs.load_file(&concat_string!(name, ".json")).await {
                    return Ok(bytes);
                }
            }
            bail!("Cannot find chart file")
        }
        let extra = fs.load_file("extra.json").await.ok().map(|it| String::from_utf8(it)).transpose()?;
        let extra = if let Some(extra) = extra {
            let ffmpeg: PathBuf = FFMPEG_PATH.lock().unwrap().to_owned().unwrap_or_else(|| "ffmpeg".into());
            let ffmpeg = if match Command::new(&ffmpeg).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                Ok(_) => true,
                Err(err) => err.kind() != ErrorKind::NotFound,
            } {
                Some(ffmpeg.as_path())
            } else {
                warn!("ffmpeg not found at {}, disabling video", ffmpeg.display());
                None
            };
            parse_extra(&extra, fs.deref_mut(), ffmpeg).await.context("Failed to parse extra")?
        } else {
            ChartExtra::default()
        };
        let text = String::from_utf8(load_chart_bytes(fs, info).await.context("Failed to load chart")?)?;
        let format = info.format.clone().unwrap_or_else(|| {
            if text.starts_with('{') {
                if text.contains("\"META\"") {
                    ChartFormat::Rpe
                } else {
                    ChartFormat::Pgr
                }
            } else {
                ChartFormat::Pec
            }
        });
        let mut chart = match format {
            ChartFormat::Rpe => parse_rpe(&text, fs.deref_mut(), extra).await,
            ChartFormat::Pgr => parse_phigros(&text, extra),
            ChartFormat::Pec => parse_pec(&text, extra),
        }?;
        chart.settings.hold_partial_cover = info.hold_partial_cover;
        Ok(chart)
    }

    pub async fn new(
        mode: GameMode,
        info: ChartInfo,
        mut config: Config,
        mut fs: Box<dyn FileSystem>,
        player: Option<SafeTexture>,
        background: SafeTexture,
        illustration: SafeTexture,
        get_size_fn: Rc<dyn Fn() -> (u32, u32)>,
    ) -> Result<Self> {
        match mode {
            GameMode::TweakOffset => {
                config.autoplay = true;
            }
            GameMode::Exercise => {
                config.autoplay = false;
            }
            _ => {}
        }
        let mut chart = Self::load_chart(&mut fs, &info).await?;
        let effects = std::mem::take(&mut chart.extra.global_effects);
        if config.fxaa {
            chart
                .extra
                .effects
                .push(Effect::new(0.0..f32::INFINITY, include_str!("fxaa.glsl"), Vec::new(), false).unwrap());
        }

        let info_offset = info.offset;
        let mut res = Resource::new(config, info, fs, player, background, illustration, chart.extra.effects.is_empty() && effects.is_empty())
            .await
            .context("Failed to load resources")?;
        let exercise_range = (chart.offset + info_offset + res.config.offset)..res.track_length;

        let judge = Judge::new(&chart);

        let music = Self::new_music(&mut res)?;
        Ok(Self {
            should_exit: false,
            next_scene: None,

            mode,
            res,
            chart,
            judge,
            gl: unsafe { get_internal_gl() },
            compatible_mode: false,
            effects,
            info_offset,

            first_in: false,
            exercise_range,
            exercise_press: None,
            exercise_btns: (RectButton::new(), RectButton::new()),

            music,

            get_size_fn,

            state: State::Starting,
            last_update_time: 0.,
            pause_rewind: None,

            bad_notes: Vec::new(),
        })
    }

    fn new_music(res: &mut Resource) -> Result<Music> {
        res.audio.create_music(
            res.music.clone(),
            MusicParams {
                amplifier: res.config.volume_music as _,
                playback_rate: res.config.speed as _,
                ..Default::default()
            },
        )
    }

    fn ui(&mut self, ui: &mut Ui, tm: &mut TimeManager) -> Result<()> {
        let time = tm.now() as f32;
        let p = match self.state {
            State::Starting => {
                if time <= Self::BEFORE_TIME {
                    1. - (1. - time / Self::BEFORE_TIME).powi(3)
                } else {
                    1.
                }
            }
            State::BeforeMusic => 1.,
            State::Playing => 1.,
            State::Ending => {
                let t = time - self.res.track_length - WAIT_TIME;
                1. - (t / (AFTER_TIME + 0.3)).min(1.).powi(2)
            }
        };
        let c = Color::new(1., 1., 1., self.res.alpha);
        let res = &mut self.res;
        let eps = 2e-2 / res.aspect_ratio;
        let top = -1. / res.aspect_ratio;
        let pause_w = 0.015;
        let pause_h = pause_w * 3.2;
        let pause_center = Point::new(pause_w * 3.5 - 1., top + eps * 2.8 + pause_h / 2.);
        if res.config.interactive
            && !tm.paused()
            && self.pause_rewind.is_none()
            && Judge::get_touches().iter().any(|touch| {
                touch.phase == TouchPhase::Started && {
                    let p = touch.position;
                    let p = Point::new(p.x, p.y);
                    (pause_center - p).norm() < 0.05
                }
            })
        {
            if !self.music.paused() {
                self.music.pause()?;
            }
            tm.pause();
        }
        let margin = 0.03;

        self.chart.with_element(ui, res, UIElement::Score, |ui, color, scale| {
            ui.text(format!("{:07}", self.judge.score()))
                .pos(1. - margin, top + eps * 2.2 - (1. - p) * 0.4)
                .anchor(1., 0.)
                .size(0.8)
                .color(Color { a: color.a * c.a, ..color })
                .scale(scale)
                .draw();
        });
        self.chart.with_element(ui, res, UIElement::Pause, |ui, color, scale| {
            let mut r = Rect::new(pause_w * 3.0 - 1., top + eps * 3.5 - (1. - p) * 0.4, pause_w, pause_h);
            let ct = Vector::new(r.x + pause_w, r.y + r.h / 2.);
            let c = Color { a: color.a * c.a, ..color };
            ui.with(scale.prepend_translation(&-ct).append_translation(&ct), |ui| {
                ui.fill_rect(r, c);
                r.x += pause_w * 2.;
                ui.fill_rect(r, c);
            });
        });
        if self.judge.combo >= 3 {
            let btm = self.chart.with_element(ui, res, UIElement::ComboNumber, |ui, color, scale| {
                ui.text(self.judge.combo.to_string())
                    .pos(0., top + eps * 2. - (1. - p) * 0.4)
                    .anchor(0.5, 0.)
                    .color(Color { a: color.a * c.a, ..color })
                    .scale(scale)
                    .draw()
                    .bottom()
            });
            self.chart.with_element(ui, res, UIElement::Combo, |ui, color, scale| {
                ui.text(if res.config.autoplay { "AUTOPLAY" } else { "COMBO" })
                    .pos(0., btm + 0.01)
                    .anchor(0.5, 0.)
                    .size(0.4)
                    .color(Color { a: color.a * c.a, ..color })
                    .scale(scale)
                    .draw();
            });
        }
        let lf = -1. + margin;
        let bt = -top - eps * 2.8;
        self.chart.with_element(ui, res, UIElement::Name, |ui, color, scale| {
            ui.text(&res.info.name)
                .pos(lf, bt + (1. - p) * 0.4)
                .anchor(0., 1.)
                .size(0.5)
                .color(Color { a: color.a * c.a, ..color })
                .scale(scale)
                .draw();
        });
        self.chart.with_element(ui, res, UIElement::Level, |ui, color, scale| {
            ui.text(&res.info.level)
                .pos(-lf, bt + (1. - p) * 0.4)
                .anchor(1., 1.)
                .size(0.5)
                .color(Color { a: color.a * c.a, ..color })
                .scale(scale)
                .draw();
        });
        let hw = 0.003;
        let height = eps * 1.2;
        let dest = 2. * res.time / res.track_length;
        self.chart.with_element(ui, res, UIElement::Bar, |ui, color, scale| {
            let ct = Vector::new(0., top + height / 2.);
            ui.with(scale.prepend_translation(&-ct).append_translation(&ct), |ui| {
                ui.fill_rect(
                    Rect::new(-1., top, dest, height),
                    Color {
                        a: color.a * c.a * 0.6,
                        ..color
                    },
                );
                ui.fill_rect(Rect::new(-1. + dest - hw, top, hw * 2., height), Color { a: color.a * c.a, ..color });
            });
        });
        Ok(())
    }

    fn overlay_ui(&mut self, ui: &mut Ui, tm: &mut TimeManager) -> Result<()> {
        let c = Color::new(1., 1., 1., self.res.alpha);
        let res = &mut self.res;
        if tm.paused() {
            let h = 1. / res.aspect_ratio;
            draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., 0.6));
            let o = if self.mode == GameMode::Exercise { -0.3 } else { 0. };
            let s = 0.06;
            let w = 0.05;
            draw_texture_ex(
                *res.icon_back,
                -s * 3. - w,
                -s + o,
                c,
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            draw_texture_ex(
                *res.icon_retry,
                -s,
                -s + o,
                c,
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            draw_texture_ex(
                *res.icon_resume,
                s + w,
                -s + o,
                c,
                DrawTextureParams {
                    dest_size: Some(vec2(s * 2., s * 2.)),
                    ..Default::default()
                },
            );
            if res.config.interactive {
                let mut clicked = None;
                for touch in Judge::get_touches() {
                    if touch.phase != TouchPhase::Started {
                        continue;
                    }
                    let p = touch.position;
                    let p = Point::new(p.x, p.y);
                    for i in -1..=1 {
                        let ct = Point::new((s * 2. + w) * i as f32, o);
                        let d = p - ct;
                        if d.x.abs() <= s && d.y.abs() <= s {
                            clicked = Some(i);
                            break;
                        }
                    }
                }
                match clicked {
                    Some(-1) => {
                        self.should_exit = true;
                    }
                    Some(0) => {
                        reset!(self, res, tm);
                    }
                    Some(1) => {
                        let mut pos = self.music.position();
                        if self.mode == GameMode::Exercise && tm.now() > self.exercise_range.end as f64 {
                            tm.seek_to(self.exercise_range.start as f64);
                            self.music.seek_to(self.exercise_range.start)?;
                            pos = self.exercise_range.start;
                        }
                        self.music.play()?;
                        res.time -= 3.;
                        let dst = pos - 3.;
                        if dst < 0. {
                            self.music.pause()?;
                            self.state = State::BeforeMusic;
                        } else {
                            self.music.seek_to(dst)?;
                        }
                        tm.resume();
                        tm.seek_to(tm.now() - 3.);
                        self.pause_rewind = Some(tm.now() - 0.2);
                    }
                    _ => {}
                }
            }
            if self.mode == GameMode::Exercise {
                ui.dy(0.06);
                let hw = 0.7;
                let h = 0.06;
                let eh = 0.12;
                let rad = 0.03;
                let sp = self.offset().min(0.);
                ui.fill_rect(Rect::new(-hw, -h, hw * 2., h * 2.), GRAY);
                let st = -hw + (self.exercise_range.start - sp) / (self.res.track_length - sp) * hw * 2.;
                let en = -hw + (self.exercise_range.end - sp) / (self.res.track_length - sp) * hw * 2.;
                let t = tm.now() as f32;
                let cur = -hw + (t - sp) / (self.res.track_length - sp) * hw * 2.;
                ui.fill_rect(Rect::new(st, -h, en - st, h * 2.), WHITE);
                ui.fill_rect(Rect::new(st, -eh, 0., eh + h).feather(0.005), BLUE);
                ui.fill_circle(st, -eh, rad, BLUE);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(st, -eh, 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (-1, it.id));
                }
                ui.fill_rect(Rect::new(en, -h, 0., eh + h).feather(0.005), RED);
                ui.fill_circle(en, eh, rad, RED);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(en, eh, 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (1, it.id));
                }
                ui.fill_rect(Rect::new(cur, -h, 0., h * 2.).feather(0.005), GREEN);
                ui.fill_circle(cur, 0., rad, GREEN);
                if self.exercise_press.is_none() {
                    let r = ui.rect_to_global(Rect::new(cur, 0., 0., 0.).feather(rad));
                    self.exercise_press = Judge::get_touches()
                        .iter()
                        .find(|it| it.phase == TouchPhase::Started && r.contains(it.position))
                        .map(|it| (0, it.id));
                }
                ui.text(fmt_time(t)).pos(0., -0.23).anchor(0.5, 0.).size(0.8).draw();
                if let Some((ctrl, id)) = &self.exercise_press {
                    if let Some(touch) = Judge::get_touches().iter().rfind(|it| it.id == *id) {
                        let x = touch.position.x;
                        let p = (x + hw) / (hw * 2.) * (self.res.track_length - sp) + sp;
                        let p = if self.res.track_length - sp <= 3. || *ctrl == 0 {
                            p.clamp(sp, self.res.track_length)
                        } else {
                            p.clamp(
                                if *ctrl == -1 { sp } else { self.exercise_range.start + 3. },
                                if *ctrl == -1 {
                                    self.exercise_range.end - 3.
                                } else {
                                    self.res.track_length
                                },
                            )
                        };
                        if *ctrl == 0 {
                            tm.seek_to(p as f64);
                            self.music.seek_to(p)?;
                        } else {
                            *(if *ctrl == -1 {
                                &mut self.exercise_range.start
                            } else {
                                &mut self.exercise_range.end
                            }) = p;
                        }
                        if matches!(touch.phase, TouchPhase::Cancelled | TouchPhase::Ended) {
                            self.exercise_press = None;
                        }
                    }
                }
                ui.dy(0.2);
                let r = ui.text("至").size(0.8).anchor(0.5, 0.).draw();
                let mut tx = ui
                    .text(fmt_time(self.exercise_range.start))
                    .pos(r.x - 0.02, 0.)
                    .anchor(1., 0.)
                    .size(0.8)
                    .color(BLACK);
                let re = tx.measure();
                self.exercise_btns.0.set(tx.ui, re);
                tx.ui
                    .fill_rect(re.feather(0.01), Color::new(1., 1., 1., if self.exercise_btns.0.touching() { 0.5 } else { 1. }));
                tx.draw();

                let mut tx = ui
                    .text(fmt_time(self.exercise_range.end))
                    .pos(r.right() + 0.02, 0.)
                    .size(0.8)
                    .color(BLACK);
                let re = tx.measure();
                self.exercise_btns.1.set(tx.ui, re);
                tx.ui
                    .fill_rect(re.feather(0.01), Color::new(1., 1., 1., if self.exercise_btns.1.touching() { 0.5 } else { 1. }));
                tx.draw();
            }
        }
        if let Some(time) = self.pause_rewind {
            let dt = tm.now() - time;
            let t = 3 - dt.floor() as i32;
            if t <= 0 {
                self.pause_rewind = None;
            } else {
                let a = (1. - dt as f32 / 3.) * 1.;
                let h = 1. / self.res.aspect_ratio;
                draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., a));
                ui.text(t.to_string()).anchor(0.5, 0.5).size(1.).color(c).draw();
            }
        }
        Ok(())
    }

    fn interactive(res: &Resource, state: &State) -> bool {
        res.config.interactive && matches!(state, State::Playing)
    }

    fn offset(&self) -> f32 {
        self.chart.offset + self.res.config.offset + self.info_offset
    }

    fn tweak_offset(&mut self, ui: &mut Ui, ita: bool) {
        ui.scope(|ui| {
            let width = 0.55;
            let height = 0.4;
            ui.dx(1. - width - 0.02);
            ui.dy(ui.top - height - 0.02);
            ui.fill_rect(Rect::new(0., 0., width, height), GRAY);
            ui.dy(0.02);
            ui.text("调整延迟").pos(width / 2., 0.).anchor(0.5, 0.).size(0.7).draw();
            ui.dy(0.16);
            let r = ui
                .text(format!("{}ms", (self.info_offset * 1000.).round() as i32))
                .pos(width / 2., 0.)
                .anchor(0.5, 0.)
                .size(0.6)
                .no_baseline()
                .draw();
            let d = 0.14;
            if ui.button("lg_sub", Rect::new(d, r.center().y, 0., 0.).feather(0.026), "-") && ita {
                self.info_offset -= 0.05;
            }
            if ui.button("lg_add", Rect::new(width - d, r.center().y, 0., 0.).feather(0.026), "+") && ita {
                self.info_offset += 0.05;
            }
            let d = 0.08;
            if ui.button("sm_sub", Rect::new(d, r.center().y, 0., 0.).feather(0.022), "-") && ita {
                self.info_offset -= 0.005;
            }
            if ui.button("sm_add", Rect::new(width - d, r.center().y, 0., 0.).feather(0.022), "+") && ita {
                self.info_offset += 0.005;
            }
            let d = 0.03;
            if ui.button("ti_sub", Rect::new(d, r.center().y, 0., 0.).feather(0.017), "-") && ita {
                self.info_offset -= 0.001;
            }
            if ui.button("ti_add", Rect::new(width - d, r.center().y, 0., 0.).feather(0.017), "+") && ita {
                self.info_offset += 0.001;
            }
            ui.dy(0.14);
            let pad = 0.02;
            let spacing = 0.01;
            let mut r = Rect::new(pad, 0., (width - pad * 2. - spacing * 2.) / 3., 0.06);
            if ui.button("cancel", r, "取消") {
                self.next_scene = Some(NextScene::PopWithResult(Box::new(None::<f32>)));
            }
            r.x += r.w + spacing;
            if ui.button("reset", r, "重置") {
                self.info_offset = 0.;
            }
            r.x += r.w + spacing;
            if ui.button("save", r, "保存") {
                self.next_scene = Some(NextScene::PopWithResult(Box::new(Some(self.info_offset))));
            }
        });
    }
}

impl Scene for GameScene {
    fn enter(&mut self, tm: &mut TimeManager, target: Option<RenderTarget>) -> Result<()> {
        #[cfg(target_arch = "wasm32")]
        on_game_start();
        self.music = Self::new_music(&mut self.res)?;
        self.res.camera.render_target = target;
        tm.speed = self.res.config.speed as _;
        reset!(self, self.res, tm);
        set_camera(&self.res.camera);
        self.first_in = true;
        Ok(())
    }

    fn pause(&mut self, tm: &mut TimeManager) -> Result<()> {
        if !tm.paused() {
            self.pause_rewind = None;
            self.music.pause()?;
            tm.pause();
        }
        Ok(())
    }

    fn resume(&mut self, tm: &mut TimeManager) -> Result<()> {
        if !matches!(self.state, State::Playing) {
            tm.resume();
        }
        Ok(())
    }

    fn update(&mut self, tm: &mut TimeManager) -> Result<()> {
        self.res.audio.recover_if_needed()?;
        if matches!(self.state, State::Playing) {
            tm.update(self.music.position() as f64);
        }
        if self.mode == GameMode::Exercise && tm.now() > self.exercise_range.end as f64 && !tm.paused() {
            let state = self.state.clone();
            reset!(self, self.res, tm);
            self.state = state;
            tm.seek_to(self.exercise_range.start as f64);
            tm.pause();
            self.music.pause()?;
        }
        let offset = self.offset();
        let time = tm.now() as f32;
        let time = match self.state {
            State::Starting => {
                if time >= Self::BEFORE_TIME {
                    self.res.alpha = 1.;
                    self.state = State::BeforeMusic;
                    tm.reset();
                    tm.seek_to(if self.mode == GameMode::Exercise {
                        self.exercise_range.start as f64
                    } else {
                        offset.min(0.) as f64
                    });
                    self.last_update_time = tm.real_time();
                    if self.first_in && self.mode == GameMode::Exercise {
                        tm.pause();
                        self.first_in = false;
                    }
                    tm.now() as f32
                } else {
                    self.res.alpha = 1. - (1. - time / Self::BEFORE_TIME).powi(3);
                    if self.mode == GameMode::Exercise {
                        self.exercise_range.start
                    } else {
                        offset
                    }
                }
            }
            State::BeforeMusic => {
                if time >= 0.0 {
                    self.music.seek_to(time)?;
                    if !tm.paused() {
                        self.music.play()?;
                    }
                    self.state = State::Playing;
                }
                time
            }
            State::Playing => {
                if time > self.res.track_length + WAIT_TIME {
                    self.state = State::Ending;
                }
                time
            }
            State::Ending => {
                let t = time - self.res.track_length - WAIT_TIME;
                if t >= AFTER_TIME + 0.3 {
                    self.next_scene = match self.mode {
                        GameMode::Normal => Some(NextScene::Overlay(Box::new(EndingScene::new(
                            self.res.background.clone(),
                            self.res.illustration.clone(),
                            self.res.player.clone(),
                            self.res.icons.clone(),
                            self.res.icon_retry.clone(),
                            self.res.icon_proceed.clone(),
                            self.res.info.clone(),
                            self.judge.result(),
                            self.res.challenge_icons[self.res.config.challenge_color.clone() as usize].clone(),
                            &self.res.config,
                            self.res.res_pack.ending.clone(),
                        )?))),
                        GameMode::TweakOffset => Some(NextScene::PopWithResult(Box::new(None::<f32>))),
                        GameMode::Exercise => None,
                    };
                }
                self.res.alpha = 1. - (t / AFTER_TIME).min(1.).powi(2);
                self.res.track_length
            }
        };
        let time = (time - offset).max(0.);
        self.res.time = time;
        if !tm.paused() && self.pause_rewind.is_none() {
            self.gl.quad_gl.viewport(self.res.camera.viewport);
            self.judge.update(&mut self.res, &mut self.chart, &mut self.bad_notes);
            self.gl.quad_gl.viewport(None);
        }
        self.res.judge_line_color = if self.judge.counts[2] + self.judge.counts[3] == 0 {
            if self.judge.counts[1] == 0 {
                JUDGE_LINE_PERFECT_COLOR
            } else {
                JUDGE_LINE_GOOD_COLOR
            }
        } else {
            WHITE
        };
        self.res.judge_line_color.a *= self.res.alpha;
        self.chart.update(&mut self.res);
        let res = &mut self.res;
        if res.config.interactive && is_key_pressed(KeyCode::Space) {
            if tm.paused() {
                if matches!(self.state, State::Playing) {
                    self.music.play()?;
                    tm.resume();
                }
            } else if matches!(self.state, State::Playing | State::BeforeMusic) {
                if !self.music.paused() {
                    self.music.pause()?;
                }
                tm.pause();
            }
        }
        if Self::interactive(res, &self.state) {
            if is_key_pressed(KeyCode::Left) {
                res.time -= 1.;
                let dst = (self.music.position() - 1.).max(0.);
                self.music.seek_to(dst)?;
                tm.seek_to(dst as f64);
            }
            if is_key_pressed(KeyCode::Right) {
                res.time += 5.;
                let dst = (self.music.position() + 5.).min(res.track_length);
                self.music.seek_to(dst)?;
                tm.seek_to(dst as f64);
            }
            if is_key_pressed(KeyCode::Q) {
                self.should_exit = true;
            }
        }
        for e in &mut self.effects {
            e.update(&self.res);
        }
        if let Some((id, text)) = take_input() {
            let parse_time = |s: &str| -> Option<f32> {
                if s.is_empty() {
                    return None;
                }
                let r = s.split(':').collect::<Vec<_>>();
                if r.len() > 3 {
                    return None;
                }
                let mut iter = r.into_iter().rev();
                let mut res = iter.next().unwrap().parse::<f32>().ok()?;
                if res < 0. {
                    return None;
                }
                if let Some(mins) = iter.next() {
                    res += mins.parse::<u32>().ok()? as f32 * 60.;
                }
                if let Some(hrs) = iter.next() {
                    res += hrs.parse::<u32>().ok()? as f32 * 3600.;
                }
                Some(res)
            };
            let offset = self.offset().min(0.);
            #[allow(clippy::manual_clamp)]
            match id.as_str() {
                "exercise_start" => {
                    if let Some(t) = parse_time(&text) {
                        if !(offset..self.res.track_length.min(self.exercise_range.end - 3.).max(offset)).contains(&t) {
                            show_message("时间不在范围内");
                        } else {
                            self.exercise_range.start = t;
                            show_message("设置成功");
                        }
                    } else {
                        show_message("格式有误");
                    }
                }
                "exercise_end" => {
                    if let Some(t) = parse_time(&text) {
                        if !((self.exercise_range.start + 3.).max(offset).min(self.res.track_length)..self.res.track_length).contains(&t) {
                            show_message("时间不在范围内");
                        } else {
                            self.exercise_range.end = t;
                            show_message("设置成功");
                        }
                    } else {
                        show_message("格式有误");
                    }
                }
                _ => return_input(id, text),
            }
        }
        Ok(())
    }

    fn touch(&mut self, tm: &mut TimeManager, touch: &Touch) -> Result<bool> {
        if self.mode == GameMode::Exercise && tm.paused() {
            if self.exercise_btns.0.touch(touch) {
                request_input("exercise_start", &fmt_time(self.exercise_range.start));
                return Ok(true);
            }
            if self.exercise_btns.1.touch(touch) {
                request_input("exercise_end", &fmt_time(self.exercise_range.end));
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn render(&mut self, tm: &mut TimeManager, ui: &mut Ui) -> Result<()> {
        let res = &mut self.res;
        let asp = screen_aspect();
        let dim = (self.get_size_fn)();
        if res.update_size(dim) {
            set_camera(&res.camera);
        }

        let msaa = res.config.sample_count > 1;

        let chart_onto = res
            .chart_target
            .as_ref()
            .map(|it| if msaa { it.input() } else { it.output() })
            .or(res.camera.render_target);
        push_camera_state();
        self.gl.quad_gl.viewport(None);
        set_camera(&Camera2D {
            zoom: vec2(1., -screen_aspect()),
            render_target: chart_onto,
            ..Default::default()
        });
        clear_background(BLACK);
        draw_background(*res.background);
        pop_camera_state();

        self.gl.quad_gl.render_pass(chart_onto.map(|it| it.render_pass));
        self.gl.quad_gl.viewport(res.camera.viewport);

        let h = 1. / res.aspect_ratio;
        draw_rectangle(-1., -h, 2., h * 2., Color::new(0., 0., 0., res.alpha * res.info.background_dim));

        self.chart.render(ui, res);

        self.gl.quad_gl.render_pass(
            res.chart_target
                .as_ref()
                .map(|it| it.output().render_pass)
                .or_else(|| res.camera.render_pass()),
        );

        self.bad_notes.retain(|dummy| dummy.render(res));
        let t = tm.real_time();
        let dt = (t - std::mem::replace(&mut self.last_update_time, t)) as f32;
        if res.config.particle {
            res.emitter.draw(dt);
        }
        self.ui(ui, tm)?;
        self.overlay_ui(ui, tm)?;

        if self.mode == GameMode::TweakOffset {
            push_camera_state();
            self.gl.quad_gl.viewport(None);
            set_camera(&Camera2D {
                zoom: vec2(1., -screen_aspect()),
                render_target: self.res.chart_target.as_ref().map(|it| it.output()).or(self.res.camera.render_target),
                ..Default::default()
            });
            self.tweak_offset(ui, Self::interactive(&self.res, &self.state));
            pop_camera_state();
        }

        if !self.res.no_effect && !self.effects.is_empty() {
            push_camera_state();
            set_camera(&Camera2D {
                zoom: vec2(1., asp),
                ..Default::default()
            });
            for e in &self.effects {
                e.render(&mut self.res);
            }
            pop_camera_state();
        }
        if msaa || !self.res.no_effect {
            // render the texture onto screen
            if let Some(target) = &self.res.chart_target {
                self.gl.flush();
                if !self.compatible_mode
                    && !copy_fbo(
                        target.output().render_pass.gl_internal_id(self.gl.quad_context),
                        self.res
                            .camera
                            .render_target
                            .map_or(0, |it| it.render_pass.gl_internal_id(self.gl.quad_context)),
                        dim,
                    )
                {
                    self.compatible_mode = true;
                }
                if self.compatible_mode {
                    push_camera_state();
                    self.gl.quad_gl.viewport(None);
                    set_camera(&Camera2D {
                        zoom: vec2(1., screen_aspect()),
                        render_target: self.res.camera.render_target,
                        ..Default::default()
                    });
                    draw_texture_ex(
                        target.output().texture,
                        -1.,
                        -ui.top,
                        WHITE,
                        DrawTextureParams {
                            dest_size: Some(vec2(2., ui.top * 2.)),
                            ..Default::default()
                        },
                    );
                    pop_camera_state();
                }
            }
        }
        Ok(())
    }

    fn next_scene(&mut self, tm: &mut TimeManager) -> NextScene {
        if self.should_exit {
            if tm.paused() {
                tm.resume();
            }
            tm.speed = 1.0;
            match self.mode {
                GameMode::Normal | GameMode::Exercise => NextScene::Pop,
                GameMode::TweakOffset => NextScene::PopWithResult(Box::new(None::<f32>)),
            }
        } else if let Some(next_scene) = self.next_scene.take() {
            tm.speed = 1.0;
            next_scene
        } else {
            NextScene::None
        }
    }
}
