// SPDX-License-Identifier: MPL-2.0

use std::sync::{LazyLock, Mutex, RwLock};

use gst::{
    LoggableError,
    glib::{self, BoolError, bool_error},
    prelude::*,
    subclass::prelude::*,
};
use gst_audio::AudioBufferRef;
use gst_pbutils::{
    AudioVisualizer,
    subclass::{AudioVisualizerSetupToken, prelude::*},
};
use gst_video::{VideoFrameExt as _, VideoFrameRef};
use plotters::coord::{CoordTranslate, ranged3d::Cartesian3d, types::RangedCoordf64};
use vello::kurbo::{BezPath, Join, Point, Rect, Stroke};
use vello_cpu::{
    PixmapMut, RenderContext, Resources,
    color::{OpaqueColor, palette::css},
};

use crate::spectrum::History;

const FADE_LINES: f64 = 2.0;

struct Projection(Cartesian3d<RangedCoordf64, RangedCoordf64, RangedCoordf64>);

impl Projection {
    fn new(
        width: i32,
        height: i32,
        num_bins: usize,
        num_lines: usize,
        yaw: f64,
        pitch: f64,
    ) -> Self {
        let min = i32::min(width, height) as f64;
        let prism = Prism::cube(min);
        let scale = prism.fit_scale(width as f64, height as f64, 0.);
        let proj = Cartesian3d::with_projection(
            0.0..(num_bins as f64),
            0.0..1000.,
            0.0..(num_lines as f64),
            (0..width, 0..height),
            |mut pb| {
                pb.yaw = yaw.to_radians();
                pb.pitch = pitch.to_radians();
                pb.scale = scale;
                pb.into_matrix()
            },
        );
        Self(proj)
    }
    fn project(&self, x: f64, y: f64, z: f64) -> Point {
        let Self(proj) = self;
        let (x_proj, y_proj) = proj.translate(&(x, y, z));
        Point::new(x_proj as f64, y_proj as f64)
    }
}

struct Prism {
    lx: f64,
    ly: f64,
    lz: f64,
}

impl Prism {
    fn new(lx: f64, ly: f64, lz: f64) -> Self {
        Self {
            lx: lx.abs(),
            ly: ly.abs(),
            lz: lz.abs(),
        }
    }

    fn cube(l: f64) -> Self {
        Self::new(l, l, l)
    }

    #[inline]
    fn max_extent(&self) -> f64 {
        (self.lx * self.lx + self.ly * self.ly + self.lz * self.lz).sqrt()
    }

    fn fit_scale(&self, frame_width: f64, frame_height: f64, margin: f64) -> f64 {
        let max_dim = self.max_extent();
        if max_dim == 0.0 {
            return 1.0;
        }

        let usable_w = (frame_width - 2.0 * margin).max(0.0);
        let usable_h = (frame_height - 2.0 * margin).max(0.0);

        usable_w.min(usable_h) / max_dim
    }
}

struct Graphics {
    ctx: RenderContext,
    resources: Resources,
}

impl Graphics {
    fn new(width: u16, height: u16) -> Self {
        Self {
            ctx: RenderContext::new(width, height),
            resources: Resources::default(),
        }
    }
}

struct ScrollingAnimation {
    phase: f64,
    step: f64,
}

impl ScrollingAnimation {
    fn new(step: f64) -> Self {
        Self { phase: 0., step }
    }
    fn advance(&mut self) -> bool {
        self.phase += self.step;
        if self.phase >= 1. {
            self.phase -= 1.;
            true
        } else {
            false
        }
    }
}

struct State {
    history: History,
    graphics: Graphics,
    scroll: ScrollingAnimation,
}

#[derive(Clone, Copy)]
struct Settings {
    yaw: f64,
    pitch: f64,
    stroke: f64,
    antialias: bool,
    num_lines: u64,
}

impl Settings {
    const YAW_DEFAULT: f64 = 30.0;
    const PITCH_DEFAULT: f64 = 30.0;
    const STROKE_DEFAULT: f64 = 1.0;
    const NUM_LINES_DEFAULT: u64 = 32;
    const ANTIALIAS_DEFAULT: bool = true;

    fn antialias_threshold(&self) -> Option<u8> {
        if self.antialias { None } else { Some(0x80) }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            yaw: Self::YAW_DEFAULT,
            pitch: Self::PITCH_DEFAULT,
            stroke: Self::STROKE_DEFAULT,
            antialias: Self::ANTIALIAS_DEFAULT,
            num_lines: Self::NUM_LINES_DEFAULT,
        }
    }
}

#[derive(Default)]
pub struct AudioRidgeline {
    state: Mutex<Option<State>>,
    settings: RwLock<Settings>,
}

impl AudioRidgeline {
    fn sinkpad(&self) -> Option<gst::Pad> {
        self.obj().static_pad("sink")
    }

    fn audio_caps(&self) -> Option<gst::Caps> {
        self.sinkpad()?.current_caps()
    }

    fn audio_info(&self) -> Option<gst_audio::AudioInfo> {
        let caps = self.audio_caps()?;
        gst_audio::AudioInfo::from_caps(&caps).ok()
    }

    fn require_audio_info(&self) -> Result<gst_audio::AudioInfo, BoolError> {
        self.audio_info()
            .ok_or(bool_error!("audio info/caps not yet available"))
    }

    fn srcpad(&self) -> Option<gst::Pad> {
        self.obj().static_pad("src")
    }

    fn video_caps(&self) -> Option<gst::Caps> {
        self.srcpad()?.current_caps()
    }

    fn video_info(&self) -> Option<gst_video::VideoInfo> {
        let caps = self.video_caps()?;
        gst_video::VideoInfo::from_caps(&caps).ok()
    }

    #[inline]
    fn ease(val: f64) -> f64 {
        val * val * (3.0 - 2.0 * val)
    }

    fn draw_frame(
        &self,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
        history: &mut History,
        graphics: &mut Graphics,
        scroll: &ScrollingAnimation,
        settings: Settings,
    ) -> Result<(), LoggableError> {
        let width = video_frame.width() as u16;
        let height = video_frame.height() as u16;
        let plane = video_frame.plane_data_mut(0)?;

        let num_lines = history.len();
        let num_bins = history.num_bins();
        if num_lines == 0 {
            return Ok(());
        }

        let stroke = Stroke::new(settings.stroke).with_join(Join::Round);
        let bg = css::BLACK;

        let ctx = &mut graphics.ctx;
        let proj = Projection::new(
            width as i32,
            height as i32,
            num_bins,
            num_lines,
            settings.yaw,
            settings.pitch,
        );
        let pixmap = PixmapMut::new(width, height, plane)
            .ok_or(bool_error!("failed to map plane to pixmap"))?;

        ctx.set_aliasing_threshold(settings.antialias_threshold());
        ctx.reset();

        let area = Rect::new(0., 0., width as f64, height as f64);
        ctx.set_paint(bg);
        ctx.fill_rect(&area);

        for (z, row) in history.iter().enumerate().map(|(z, h)| (z as f64, h)) {
            let mut iter = row.iter().enumerate().map(|(x, y)| (x as f64, *y as f64));
            let Some((x0, y0)) = iter.next() else {
                continue;
            };

            let distance_from_newest = (num_lines - 1) as f64 - z + scroll.phase;
            let t_new = (distance_from_newest / FADE_LINES).clamp(0., 1.);

            let distance_from_oldest = z + 1. - scroll.phase;
            let t_old = (distance_from_oldest / FADE_LINES).clamp(0., 1.);

            let scale = Self::ease(t_new.min(t_old));

            let height_scaler = |y: f64| y * scale;

            let z_f = z - scroll.phase;
            let p = proj.project(x0, height_scaler(y0), z_f);
            let mut ridgeline = BezPath::with_capacity(num_bins + 2);
            ridgeline.move_to(p);
            for (x, y) in iter.map(|(x, y)| (x, height_scaler(y))) {
                let p = proj.project(x, y, z_f);
                ridgeline.line_to(p);
            }

            let mut under_ridgeline = ridgeline.clone();
            let bottom_left = proj.project(0., 0., z_f);
            let bottom_right = proj.project(num_bins as f64, 0., z_f);
            under_ridgeline.line_to(bottom_right);
            under_ridgeline.line_to(bottom_left);

            let color = (scale * 255.) as u8;

            ctx.set_paint(bg);
            ctx.fill_path(&under_ridgeline);

            ctx.set_paint(OpaqueColor::from_rgb8(color, color, color));
            ctx.set_stroke(stroke.clone());
            ctx.stroke_path(&ridgeline);
        }

        ctx.flush();
        ctx.render(pixmap, &mut graphics.resources);

        Ok(())
    }
}

#[glib::object_subclass]
impl ObjectSubclass for AudioRidgeline {
    const NAME: &'static str = "GstAudioRidgeline";
    type Type = super::AudioRidgeline;
    type ParentType = AudioVisualizer;
}

impl ObjectImpl for AudioRidgeline {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecDouble::builder("yaw")
                    .nick("Yaw")
                    .blurb("Rotation around the Y axis in degrees")
                    .minimum(-360.)
                    .maximum(360.)
                    .default_value(Settings::YAW_DEFAULT)
                    .mutable_playing()
                    .controllable()
                    .build(),
                glib::ParamSpecDouble::builder("pitch")
                    .nick("Pitch")
                    .blurb("Rotation around the X axis in degrees")
                    .minimum(-360.)
                    .maximum(360.)
                    .default_value(Settings::PITCH_DEFAULT)
                    .mutable_playing()
                    .controllable()
                    .build(),
                glib::ParamSpecDouble::builder("stroke")
                    .nick("Stroke Width")
                    .blurb("Thickness of ridgelines")
                    .minimum(0.)
                    .default_value(Settings::STROKE_DEFAULT)
                    .mutable_playing()
                    .controllable()
                    .build(),
                glib::ParamSpecBoolean::builder("anti-alias")
                    .nick("Anti-alias")
                    .blurb("When true, enables anti-aliased drawing")
                    .default_value(Settings::ANTIALIAS_DEFAULT)
                    .mutable_playing()
                    .controllable()
                    .build(),
                glib::ParamSpecUInt64::builder("num-lines")
                    .nick("Number of Lines")
                    .blurb("Number of ridgeline samples to plot in the visualization")
                    .minimum(1u64)
                    .maximum(256u64)
                    .default_value(Settings::NUM_LINES_DEFAULT)
                    .mutable_ready()
                    .build(),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "yaw" => {
                let yaw = value.get::<f64>().expect("type checked upstream");
                let mut settings = self.settings.write().unwrap();
                settings.yaw = yaw;
            }
            "pitch" => {
                let pitch = value.get::<f64>().expect("type checked upstream");
                let mut settings = self.settings.write().unwrap();
                settings.pitch = pitch;
            }
            "stroke" => {
                let stroke = value.get::<f64>().expect("type checked upstream");
                let mut settings = self.settings.write().unwrap();
                settings.stroke = stroke;
            }
            "anti-alias" => {
                let antialias = value.get::<bool>().expect("type checked upstream");
                let mut settings = self.settings.write().unwrap();
                settings.antialias = antialias;
            }
            "num-lines" => {
                let num_lines = value.get::<u64>().expect("type checked upstream");
                let mut settings = self.settings.write().unwrap();
                settings.num_lines = num_lines;
            }
            _ => unimplemented!(),
        };
    }

    fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "yaw" => {
                let settings = self.settings.read().unwrap();
                settings.yaw.to_value()
            }
            "pitch" => {
                let settings = self.settings.read().unwrap();
                settings.pitch.to_value()
            }
            "stroke" => {
                let settings = self.settings.read().unwrap();
                settings.stroke.to_value()
            }
            "anti-alias" => {
                let settings = self.settings.read().unwrap();
                settings.antialias.to_value()
            }
            "num-lines" => {
                let settings = self.settings.read().unwrap();
                settings.num_lines.to_value()
            }
            _ => unimplemented!(),
        }
    }
}

impl GstObjectImpl for AudioRidgeline {}

impl ElementImpl for AudioRidgeline {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                super::LONG_NAME,
                "Visualization",
                super::DESCRIPTION,
                super::AUTHOR,
            )
        });
        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let sink_caps = gst_audio::AudioCapsBuilder::new()
                .format(gst_audio::AudioFormat::F32le)
                .channels(1)
                .build();
            let src_caps = gst_video::VideoCapsBuilder::new()
                .format(gst_video::VideoFormat::Rgba)
                .width_range(0..(u16::MAX as i32))
                .height_range(0..(u16::MAX as i32))
                .build();

            let sink_pad_template = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &sink_caps,
            )
            .unwrap();

            let src_pad_template = gst::PadTemplate::new(
                "src",
                gst::PadDirection::Src,
                gst::PadPresence::Always,
                &src_caps,
            )
            .unwrap();

            vec![sink_pad_template, src_pad_template]
        });

        PAD_TEMPLATES.as_ref()
    }
}

impl AudioVisualizerImpl for AudioRidgeline {
    fn setup(&self, token: &AudioVisualizerSetupToken) -> Result<(), gst::LoggableError> {
        self.parent_setup(token)?;

        let Some(video_info) = self.video_info() else {
            return Ok(());
        };

        let fps = video_info.fps();
        let fps_n = fps.numer() as f64;
        let fps_d = fps.denom() as f64;
        let width = video_info.width() as u16;
        let height = video_info.height() as u16;

        let num_lines = self.settings.read().unwrap().num_lines;
        let lines_per_second = num_lines as f64 / 10.;

        let history = History::new(num_lines as usize);
        let graphics = Graphics::new(width, height);
        let scroll = ScrollingAnimation::new(lines_per_second * fps_d / fps_n);
        let state = State {
            history,
            graphics,
            scroll,
        };

        self.state.lock().unwrap().replace(state);

        Ok(())
    }
    fn render(
        &self,
        audio_buffer: &gst::BufferRef,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
    ) -> Result<(), gst::LoggableError> {
        let info = self.require_audio_info()?;
        let mut state_lock = self.state.lock().unwrap();
        let State {
            history,
            graphics,
            scroll,
        } = state_lock
            .as_mut()
            .ok_or(bool_error!("element state not yet available"))?;

        let settings = *self.settings.read().unwrap();

        let audio_buffer = AudioBufferRef::from_buffer_ref_readable(audio_buffer, &info)?;

        if scroll.advance() {
            history.analyze(audio_buffer)?;
        }

        self.draw_frame(video_frame, history, graphics, scroll, settings)
    }
}
