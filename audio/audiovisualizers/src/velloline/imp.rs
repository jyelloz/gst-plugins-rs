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
use vello::kurbo::{BezPath, Join, Point, Stroke};
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

type Coord3D = Cartesian3d<RangedCoordf64, RangedCoordf64, RangedCoordf64>;

fn make_coord(width: i32, height: i32) -> Coord3D {
    let margin = 20;
    let actual_x = margin..(width - margin).max(margin + 1);
    let actual_y = margin..(height - margin).max(margin + 1);
    Cartesian3d::with_projection(
        0.0..(NUM_BINS as f64),
        0.0..10.0,
        0.0..(NUM_LINES as f64),
        (actual_x, actual_y),
        |mut pb| {
            pb.yaw = 0.5;
            pb.pitch = 0.5;
            pb.scale = 0.7;
            pb.into_matrix()
        },
    )
}

struct F64Coord<'a>(&'a Coord3D);

impl F64Coord<'_> {
    fn project(&self, x: f64, y: f64, z: f64) -> Point {
        let Self(coord) = self;
        let (x_proj, y_proj) = coord.translate(&(x, y, z));
        Point::new(x_proj as f64, y_proj as f64)
    }
}

struct History {
    rows: VecDeque<[f32; NUM_BINS]>,
    scroll_phase: f64,
    scroll_step: f64,
}

impl History {
    fn new(scroll_step: f64) -> Self {
        let mut me = Self {
            rows: VecDeque::with_capacity(NUM_LINES),
            scroll_phase: 0.0,
            scroll_step,
        };
        for _ in 0..NUM_LINES {
            me.push([0f32; NUM_BINS]);
        }
        me
    }

    fn push(&mut self, row: [f32; NUM_BINS]) {
        if self.rows.len() >= NUM_LINES {
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

struct State {
    ctx: RenderContext,
    resources: Resources,
    coord: Coord3D,
}

impl State {
    fn new(width: i32, height: i32) -> Self {
        Self {
            ctx: vello_cpu::RenderContext::new(width as u16, height as u16),
            resources: Resources::default(),
            coord: make_coord(width, height),
        }
    }
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
pub struct VelloLine {
    history: Mutex<Option<History>>,
    state: Mutex<Option<State>>,
}

impl VelloLine {
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
        state: &mut State,
    ) -> Result<(), LoggableError> {
        let width = video_frame.width() as i32;
        let height = video_frame.height() as i32;
        let plane = video_frame.plane_data_mut(0)?;

        plane.fill(0);

        let n = history.len();
        if n == 0 {
            return Ok(());
        }

        let stroke = Stroke::new(STROKE_WIDTH).with_join(Join::Round);

        let ctx = &mut state.ctx;
        let coord = F64Coord(&state.coord);
        let pixmap = PixmapMut::new(width as u16, height as u16, plane)
            .ok_or(bool_error!("failed to map plane to pixmap"))?;

        ctx.reset();

        for (z, row) in history.iter().enumerate().map(|(z, h)| (z as f64, h)) {
            let mut iter = row.iter().enumerate().map(|(x, y)| (x as f64, *y as f64));
            let Some((x0, y0)) = iter.next() else {
                continue;
            };

            let distance_from_newest = (n - 1) as f64 - z + history.scroll_phase;
            let t_new = (distance_from_newest / SCALE_RAMP_LINES).clamp(0.0, 1.0);

            let distance_from_oldest = z + 1. - history.scroll_phase;
            let t_old = (distance_from_oldest / SCALE_RAMP_LINES).clamp(0., 1.);

            let scale_new = Self::ease(t_new);
            let scale_old = Self::ease(t_old);
            let scale = scale_new.min(scale_old);

            let scaler = |y: f64| y * scale;

            let z_f = z - history.scroll_phase;
            let p = coord.project(x0, scaler(y0), z_f);
            let mut ridgeline = BezPath::with_capacity(row.len());
            ridgeline.move_to(p);
            for (x, y) in iter.map(|(x, y)| (x, scaler(y))) {
                let p = coord.project(x, y, z_f);
                ridgeline.line_to(p);
            }

            let mut under_ridgeline = ridgeline.clone();
            let bottom_left = coord.project(0., 0., z_f);
            let bottom_right = coord.project(NUM_BINS as f64, 0., z_f);
            under_ridgeline.line_to(bottom_right);
            under_ridgeline.line_to(bottom_left);

            let scale = (scale * 255.) as u8;

            ctx.set_paint(css::BLACK);
            ctx.fill_path(&under_ridgeline);

            ctx.set_paint(OpaqueColor::from_rgb8(scale, scale, scale));
            ctx.set_stroke(stroke.clone());
            ctx.stroke_path(&ridgeline);
        }

        ctx.flush();
        ctx.render(pixmap, &mut state.resources);

        Ok(())
    }
}

#[glib::object_subclass]
impl ObjectSubclass for VelloLine {
    const NAME: &'static str = "GstVelloLine";
    type Type = super::VelloLine;
    type ParentType = AudioVisualizer;
}

impl ObjectImpl for VelloLine {}
impl GstObjectImpl for VelloLine {}

impl ElementImpl for VelloLine {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                super::DESCRIPTION,
                "Visualization",
                "Renders a ridgeline plot of the incoming audio",
                "Jordan Yelloz <jordan@yelloz.me>",
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

impl AudioVisualizerImpl for VelloLine {
    fn setup(&self, token: &AudioVisualizerSetupToken) -> Result<(), gst::LoggableError> {
        self.parent_setup(token)?;

        let Some(video_info) = self.video_info() else {
            return Ok(());
        };

        let fps = video_info.fps();
        let fps_n = fps.numer() as f64;
        let fps_d = fps.denom() as f64;
        let width = video_info.width() as i32;
        let height = video_info.height() as i32;

        let history = History::new(LINES_PER_SECOND * fps_d / fps_n);

        self.history.lock().unwrap().replace(history);

        let state = State::new(width, height);

        self.state.lock().unwrap().replace(state);

        Ok(())
    }
    fn render(
        &self,
        audio_buffer: &gst::BufferRef,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
    ) -> Result<(), gst::LoggableError> {
        let mut state_lock = self.state.lock().unwrap();
        let state = state_lock
            .as_mut()
            .ok_or(bool_error!("state not yet available"))?;
        let mut history_lock = self.history.lock().unwrap();
        let history = history_lock
            .as_mut()
            .ok_or(bool_error!("history not yet available"))?;

        history.scroll_phase += history.scroll_step;
        if history.scroll_phase >= 1. {
            history.scroll_phase -= 1.;
            if let Some(bins) = self.analyze(audio_buffer)? {
                history.push(bins);
            }
        }
        self.draw_frame(video_frame, history, state)
    }
}
