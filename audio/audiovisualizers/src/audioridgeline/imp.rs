// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::{VecDeque, vec_deque::Iter},
    f64,
    iter::Rev,
    sync::{LazyLock, Mutex},
};

use byte_slice_cast::AsSliceOf as _;
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
use spectrum_analyzer::{FrequencyLimit, scaling::SpectrumDataStats};
use vello::kurbo::{BezPath, Join, Point, Rect, Stroke};
use vello_cpu::{
    PixmapMut, RenderContext, Resources,
    color::{OpaqueColor, palette::css},
};

const WINDOW_SIZE: usize = 256;
const NUM_BINS: usize = WINDOW_SIZE / 2;
const SILENCE_THRESHOLD_DBFS: f32 = 90.0;
const NUM_LINES: usize = 64;
const LINES_PER_SECOND: f64 = 3.0;
const SCALE_RAMP_LINES: f64 = 2.0;
const STROKE_WIDTH: f64 = 1.0;

static HANN_WINDOW: LazyLock<[f32; WINDOW_SIZE]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let two_pi_i = 2.0 * std::f32::consts::PI * i as f32;
        let c = (two_pi_i / WINDOW_SIZE as f32).cos();
        0.5 * (1.0 - c)
    })
});

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        super::NAME,
        gst::DebugColorFlags::empty(),
        Some(super::DESCRIPTION),
    )
});

struct Projection(Cartesian3d<RangedCoordf64, RangedCoordf64, RangedCoordf64>);

impl Projection {
    fn new(width: i32, height: i32, num_bins: usize, num_lines: usize) -> Self {
        let range_x = (0.0, (num_bins as f64));
        let range_y = (0.0, 10.);
        let range_z = (0.0, (num_lines as f64));

        let min_dim = i32::min(width, height) as f64;
        let min_dim = min_dim * 0.8;
        let prism = Prism::new(min_dim, min_dim, min_dim);
        let scale = prism.fit_scale(width as f64, height as f64, 0.);
        let proj = Cartesian3d::with_projection(
            0.0..(num_bins as f64),
            0.0..10.,
            0.0..(num_lines as f64),
            (0..width, 0..height),
            |mut pb| {
                pb.yaw = 30f64.to_radians();
                pb.pitch = 30f64.to_radians();
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

struct History {
    rows: VecDeque<[f32; NUM_BINS]>,
    num_lines: usize,
}

impl History {
    fn new(num_lines: usize) -> Self {
        let mut me = Self {
            rows: VecDeque::with_capacity(num_lines),
            num_lines,
        };
        for _ in 0..num_lines {
            me.push([0f32; NUM_BINS]);
        }
        me
    }

    fn push(&mut self, row: [f32; NUM_BINS]) {
        if self.rows.len() >= self.num_lines {
            self.rows.pop_back();
        }
        self.rows.push_front(row);
    }

    #[inline]
    fn iter(&self) -> Rev<Iter<'_, [f32; NUM_BINS]>> {
        self.rows.iter().rev()
    }

    #[inline]
    fn len(&self) -> usize {
        self.rows.len()
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
    proj: Projection,
}

impl Graphics {
    fn new(width: u16, height: u16, num_bins: usize, num_lines: usize) -> Self {
        Self {
            ctx: vello_cpu::RenderContext::new(width, height),
            resources: Resources::default(),
            proj: Projection::new(width as i32, height as i32, num_bins, num_lines),
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

fn scale_to_dbfs(amplitude: f32, _stats: &SpectrumDataStats) -> f32 {
    let normalized = amplitude.abs() / WINDOW_SIZE as f32;
    if normalized <= 0.0 {
        -SILENCE_THRESHOLD_DBFS
    } else {
        (20.0 * normalized.log10()).max(-SILENCE_THRESHOLD_DBFS)
    }
}

fn scale_dbfs_to_normalized(dbfs: f32, _stats: &SpectrumDataStats) -> f32 {
    (dbfs + SILENCE_THRESHOLD_DBFS).max(0f32) * _stats.n / SILENCE_THRESHOLD_DBFS
}

#[derive(Default)]
pub struct AudioRidgeline {
    state: Mutex<Option<State>>,
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

    fn analyze(&self, buffer: &gst::BufferRef) -> Result<Option<[f32; NUM_BINS]>, BoolError> {
        let audio_info = self.require_audio_info()?;
        let audio_buffer = AudioBufferRef::from_buffer_ref_readable(buffer, &audio_info)?;
        let bytes = audio_buffer.plane_data(0)?;
        let samples = bytes
            .as_slice_of::<f32>()
            .map_err(|e| bool_error!("failed to interpret audio buffer as f32 array: {e:?}"))?;

        if samples.len() < WINDOW_SIZE {
            gst::debug!(CAT, imp = self, "not enough data for FFT, skipping");
            return Ok(None);
        }

        let window_samples = &samples[samples.len() - WINDOW_SIZE..];
        let window: [f32; WINDOW_SIZE] =
            std::array::from_fn(|i| window_samples[i] * HANN_WINDOW[i]);
        let rate = audio_info.rate();

        let scaler =
            |val: f32, stats: &_| scale_dbfs_to_normalized(scale_to_dbfs(val, stats), stats);
        let spectrum = spectrum_analyzer::samples_fft_to_spectrum(
            &window,
            rate,
            FrequencyLimit::All,
            Some(&scaler),
        )
        .map_err(|e| bool_error!("failed to analyze sample: {:?}", e))?;

        let data = spectrum.data();
        let mut bins = [0.0f32; NUM_BINS];
        for (i, bin) in bins.iter_mut().enumerate() {
            if let Some((_, v)) = data.get(i) {
                *bin = v.val();
            }
        }

        let max_val = bins.iter().copied().fold(0.0f32, f32::max);

        let divisor = max_val.max(0.05);

        for bin in &mut bins {
            *bin /= divisor;
        }

        Ok(Some(bins))
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
    ) -> Result<(), LoggableError> {
        let width = video_frame.width() as u16;
        let height = video_frame.height() as u16;
        let plane = video_frame.plane_data_mut(0)?;

        let n = history.len();
        if n == 0 {
            return Ok(());
        }

        let stroke = Stroke::new(STROKE_WIDTH).with_join(Join::Round);
        let bg = css::BLACK;

        let ctx = &mut graphics.ctx;
        let proj = &graphics.proj;
        let pixmap = PixmapMut::new(width, height, plane)
            .ok_or(bool_error!("failed to map plane to pixmap"))?;

        ctx.reset();

        let area = Rect::new(0., 0., width as f64, height as f64);
        ctx.set_paint(bg);
        ctx.fill_rect(&area);

        for (z, row) in history.iter().enumerate().map(|(z, h)| (z as f64, h)) {
            let mut iter = row.iter().enumerate().map(|(x, y)| (x as f64, *y as f64));
            let Some((x0, y0)) = iter.next() else {
                continue;
            };

            let distance_from_newest = (n - 1) as f64 - z + scroll.phase;
            let t_new = (distance_from_newest / SCALE_RAMP_LINES).clamp(0., 1.);

            let distance_from_oldest = z + 1. - scroll.phase;
            let t_old = (distance_from_oldest / SCALE_RAMP_LINES).clamp(0., 1.);

            let scale_new = Self::ease(t_new);
            let scale_old = Self::ease(t_old);
            let scale = scale_new.min(scale_old);

            let height_scaler = |y: f64| y * scale;

            let z_f = z - scroll.phase;
            let p = proj.project(x0, height_scaler(y0), z_f);
            let mut ridgeline = BezPath::with_capacity(row.len());
            ridgeline.move_to(p);
            for (x, y) in iter.map(|(x, y)| (x, height_scaler(y))) {
                let p = proj.project(x, y, z_f);
                ridgeline.line_to(p);
            }

            let mut under_ridgeline = ridgeline.clone();
            let bottom_left = proj.project(0., 0., z_f);
            let bottom_right = proj.project(NUM_BINS as f64, 0., z_f);
            under_ridgeline.line_to(bottom_right);
            under_ridgeline.line_to(bottom_left);

            let scale = (scale * 255.) as u8;

            ctx.set_paint(bg);
            ctx.fill_path(&under_ridgeline);

            ctx.set_paint(OpaqueColor::from_rgb8(scale, scale, scale));
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

impl ObjectImpl for AudioRidgeline {}
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

        let history = History::new(NUM_LINES);
        let graphics = Graphics::new(width, height, NUM_BINS, NUM_LINES);
        let scroll = ScrollingAnimation::new(LINES_PER_SECOND * fps_d / fps_n);
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
        let mut state_lock = self.state.lock().unwrap();
        let State {
            history,
            graphics,
            scroll,
        } = state_lock
            .as_mut()
            .ok_or(bool_error!("element state not yet available"))?;

        if scroll.advance()
            && let Some(bins) = self.analyze(audio_buffer)?
        {
            history.push(bins);
        }

        self.draw_frame(video_frame, history, graphics, scroll)
    }
}
