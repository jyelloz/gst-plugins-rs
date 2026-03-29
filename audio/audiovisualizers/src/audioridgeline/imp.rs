// Copyright (C) 2026 Jordan Yelloz <jordan@yelloz.me>
// SPDX-License-Identifier: MPL-2.0

use std::{
    collections::{vec_deque::Iter, VecDeque},
    iter::Rev,
    sync::{LazyLock, Mutex},
};

use byte_slice_cast::AsSliceOf as _;
use gst::{
    glib::{self, bool_error},
    prelude::*,
    subclass::prelude::*,
};
use gst_audio::AudioBufferRef;
use gst_pbutils::{
    subclass::{prelude::*, AudioVisualizerSetupToken},
    AudioVisualizer,
};
use gst_video::{VideoFormat, VideoFrameExt as _, VideoFrameRef};
use plotters::coord::{ranged3d::Cartesian3d, types::RangedCoordf64, CoordTranslate};
use spectrum_analyzer::{scaling::SpectrumDataStats, FrequencyLimit};

const WINDOW_SIZE: usize = 256;
const NUM_BINS: usize = WINDOW_SIZE / 2;
const SILENCE_THRESHOLD_DBFS: f32 = 90f32;
const NUM_LINES: usize = 64;
const LINES_PER_SECOND: f32 = 3.0;
const STROKE_WIDTH: u32 = 1;
const SCALE_RAMP_LINES: f64 = 2.0;

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

type BoolResult<T> = Result<T, glib::BoolError>;

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

struct History {
    rows: VecDeque<[f32; NUM_BINS]>,
    scroll_phase: f32,
    scroll_step: f32,
    coord: Coord3D,
}

impl History {
    fn new(width: i32, height: i32, scroll_step: f32) -> Self {
        let mut me = Self {
            rows: VecDeque::with_capacity(NUM_LINES),
            scroll_phase: 0.0,
            scroll_step,
            coord: make_coord(width, height),
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

#[derive(Default)]
pub struct AudioRidgeline {
    history: Mutex<Option<History>>,
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

    fn require_audio_info(&self) -> BoolResult<gst_audio::AudioInfo> {
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

    fn analyze(&self, buffer: &gst::BufferRef) -> BoolResult<Option<[f32; NUM_BINS]>> {
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

    fn draw_frame(
        &self,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
        history: &mut History,
    ) -> BoolResult<()> {
        let width = video_frame.width() as usize;
        let height = video_frame.height() as usize;
        let stride = video_frame.plane_stride()[0] as usize;
        let plane = video_frame.plane_data_mut(0)?;

        let n = history.len();
        if n == 0 {
            plane.fill(0);
            return Ok(());
        }

        plane.fill(0);

        let surface = unsafe {
            cairo::ImageSurface::create_for_data_unsafe(
                plane.as_mut_ptr(),
                cairo::Format::Rgb24,
                width as i32,
                height as i32,
                stride as i32,
            )
        }
        .map_err(|e| bool_error!("Failed to create cairo surface: {:?}", e))?;

        {
            let context = cairo::Context::new(&surface)
                .map_err(|e| bool_error!("Failed to create cairo context: {e:?}"))?;
            context.set_source_rgb(1.0, 1.0, 1.0);
            context.set_line_width(STROKE_WIDTH as f64);
            context.set_antialias(cairo::Antialias::None);
            context.set_line_join(cairo::LineJoin::Round);
            context.set_hairline(true);

            for (z, row) in history.iter().enumerate() {
                let mut iter = row.iter().enumerate();
                let Some((x0, &y0)) = iter.next() else {
                    continue;
                };

                let ease = |val: f64| val * val * (3.0 - 2.0 * val);

                let distance_from_newest = (n - 1 - z) as f64 + history.scroll_phase as f64;
                let t_new = (distance_from_newest / SCALE_RAMP_LINES).clamp(0.0, 1.0);
                let scale_new = ease(t_new);

                let distance_from_oldest = z as f64 + (1.0 - history.scroll_phase as f64);
                let t_old = (distance_from_oldest / SCALE_RAMP_LINES).clamp(0.0, 1.0);
                let scale_old = ease(t_old);

                let scale = scale_new.min(scale_old);
                let scaler = |y: f32| y as f64 * scale;

                context.new_path();

                let z_f = z as f64 - history.scroll_phase as f64;
                let (sx, sy) = history.coord.translate(&(x0 as f64, scaler(y0), z_f));
                context.move_to(sx as f64, sy as f64);
                for (x, &y) in iter {
                    let (sx, sy) = history.coord.translate(&(x as f64, scaler(y), z_f));
                    context.line_to(sx as f64, sy as f64);
                }

                let ridgeline = context
                    .copy_path()
                    .map_err(|e| bool_error!("failed to copy path: {e:?}"))?;

                let (xmin, ymin) = history.coord.translate(&(0.0, 0.0, z_f));
                let (xmax, ymax) = history.coord.translate(&(row.len() as f64, 0.0, z_f));
                context.line_to(xmax as f64, ymax as f64);
                context.line_to(xmin as f64, ymin as f64);

                context.set_source_rgb(0.0, 0.0, 0.0);
                let _ = context.fill();

                context.append_path(&ridgeline);
                context.set_source_rgb(scale, scale, scale);
                let _ = context.stroke();
            }
        }

        surface.flush();

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
                super::DESCRIPTION,
                "Visualization",
                "Renders successive spectrum analysis results as a ridgeline plot",
                "Jordan Yelloz <jordan@yelloz.me>",
            )
        });
        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            const FORMAT: VideoFormat = cfg_select! {
                target_endian = "big" => VideoFormat::Xrgb,
                _ => VideoFormat::Bgrx,
            };
            let sink_caps = gst_audio::AudioCapsBuilder::new()
                .format(gst_audio::AudioFormat::F32le)
                .channels(1)
                .build();
            let src_caps = gst_video::VideoCapsBuilder::new().format(FORMAT).build();

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
    fn render(
        &self,
        audio_buffer: &gst::BufferRef,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
    ) -> Result<(), gst::LoggableError> {
        let mut history_lock = self.history.lock().unwrap();
        let history = history_lock
            .as_mut()
            .ok_or(bool_error!("history not yet available"))?;

        history.scroll_phase += history.scroll_step;
        if history.scroll_phase >= 1.0 {
            history.scroll_phase -= 1.0;
            if let Some(bins) = self.analyze(audio_buffer)? {
                history.push(bins);
            }
        }

        self.draw_frame(video_frame, history)?;

        Ok(())
    }

    fn setup(&self, token: &AudioVisualizerSetupToken) -> Result<(), gst::LoggableError> {
        self.parent_setup(token)?;

        let Some(video_info) = self.video_info() else {
            return Ok(());
        };

        let fps = video_info.fps();
        let fps_n = fps.numer() as f32;
        let fps_d = fps.denom() as f32;
        let width = video_info.width() as i32;
        let height = video_info.height() as i32;

        let history = History::new(width, height, LINES_PER_SECOND * fps_d / fps_n);

        self.history.lock().unwrap().replace(history);
        Ok(())
    }
}
