// Copyright (C) 2024 Jordan Yelloz <jordan@yelloz.me>
// SPDX-License-Identifier: MPL-2.0

use std::sync::Mutex;

use gst::{glib::{self, bool_error}, prelude::*, subclass::prelude::*};
use gst_audio::AudioBufferRef;
use gst_pbutils::{
    subclass::{prelude::*, AudioVisualizerSetupToken},
    AudioVisualizer,
};
use gst_video::{VideoFormat, VideoFrameExt as _, VideoFrameRef};
use once_cell::sync::Lazy;
use spectrum_analyzer::{
    scaling::{combined, SpectrumDataStats},
    windows::hann_window,
    FrequencyLimit, FrequencySpectrum,
};
use byte_slice_cast::AsSliceOf as _;

const WINDOW_SIZE: usize = 256;
const SILENCE_THRESHOLD_DBFS: f32 = 90f32;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        super::NAME,
        gst::DebugColorFlags::empty(),
        Some(super::DESCRIPTION),
    )
});

type BoolResult<T> = Result<T, glib::BoolError>;

struct Scratchpad {
    data: Box<[u8]>,
    current_column: usize,
    width: usize,
    height: usize,
}

impl Scratchpad {
    fn new(width: usize, height: usize, buffer_size: usize) -> Self {
        let data = vec![0u8; buffer_size];
        Self {
            data: data.into_boxed_slice(),
            current_column: 0,
            width,
            height,
        }
    }
    fn next(&mut self) {
        self.current_column += 1;
        if self.current_column >= self.width {
            self.current_column = 0;
        }
    }
    fn copy_into(
        &self,
        frame: &mut VideoFrameRef<&mut gst::BufferRef>,
    ) -> BoolResult<()> {
        let stride = frame.comp_stride(0) as usize;
        let pstride = frame.comp_pstride(0) as usize;
        let outbuf = frame.comp_data_mut(0)?;
        let pivot = (self.current_column + 1) * pstride;
        let left_size = (self.width - pivot) * pstride;
        for row in 0..self.height {
            let row_offset = row * stride;
            let in_row = &self.data[row_offset + pivot..];
            let in_row = &in_row[..left_size];
            let out_row = &mut outbuf[row_offset..];
            let out_row = &mut out_row[..left_size];
            out_row.copy_from_slice(in_row);
        }
        for row in 0..self.height {
            let row_offset = row * stride;
            let in_row = &self.data[row_offset..];
            let in_row = &in_row[..pivot];
            let out_row = &mut outbuf[row_offset + left_size..];
            let out_row = &mut out_row[..pivot];
            out_row.copy_from_slice(in_row);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct AudioSpectrogram {
    scratchpad: Mutex<Option<Scratchpad>>,
}

fn scale_to_dbfs(amplitude: f32, stats: &SpectrumDataStats) -> f32 {
    if amplitude == 0.0 {
        -SILENCE_THRESHOLD_DBFS
    } else {
        20.0 * (amplitude.abs() / stats.n).log10()
    }
}

fn scale_dbfs_to_gray(dbfs: f32, _stats: &SpectrumDataStats) -> f32 {
    if dbfs == 0.0 {
        0.0
    } else {
        ((dbfs + SILENCE_THRESHOLD_DBFS).max(0f32) * 255f32) / SILENCE_THRESHOLD_DBFS
    }
}

impl AudioSpectrogram {
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
    fn analyze(
        &self,
        buffer: &gst::BufferRef,
    ) -> BoolResult<Option<FrequencySpectrum>> {
        let audio_info = self.require_audio_info()?;
        let bpf = audio_info.bpf() as usize;
        let window_bytes = bpf * WINDOW_SIZE;
        let size_bytes = buffer.size();
        if size_bytes < window_bytes {
            gst::debug!(CAT, imp = self, "not enough data for FFT, skipping");
            return Ok(None);
        }
        let offset = size_bytes - window_bytes;
        let sub_buffer = buffer
            .copy_region(gst::BufferCopyFlags::MEMORY, offset..)?;
        let audio_buffer = AudioBufferRef::from_buffer_ref_readable(&sub_buffer, &audio_info)?;
        let rate = audio_info.rate();
        let bytes = audio_buffer.plane_data(0)?;
        let window = bytes.as_slice_of::<f32>()
            .map(hann_window)
            .map_err(|e| bool_error!("failed to interpret audio buffer as f32 array: {e:?}"))?;

        spectrum_analyzer::samples_fft_to_spectrum(
            &window,
            rate,
            FrequencyLimit::All,
            Some(&combined(&[&scale_to_dbfs, &scale_dbfs_to_gray])),
        )
        .map(Some)
        .map_err(|e| bool_error!("failed to analyze sample: {:?}", e))
    }

    #[inline]
    const fn row_to_bin(row: u32, height: u32) -> usize {
        let bin_count = (WINDOW_SIZE / 2) as u32;
        (bin_count - (row * bin_count / height)) as usize
    }

    fn draw_column(
        &self,
        sample: &FrequencySpectrum,
        stride: u32,
        pstride: u32,
        height: u32,
        column: u32,
        plane: &mut [u8],
    ) {
        let data = sample.data();
        let column_offset = (column * pstride) as usize;
        for row in 0..height {
            let bin = Self::row_to_bin(row, height);
            let (_freq, color) = &data[bin];
            let row_offset = (row * stride) as usize;
            let pixel = &mut plane[row_offset + column_offset..];
            pixel[0] = color.val() as u8;
        }
    }

    fn visualize(
        &self,
        sample: &FrequencySpectrum,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
    ) -> BoolResult<()> {
        let mut scratch_lock = self.scratchpad.lock().unwrap();
        let scratch = scratch_lock.as_mut()
            .ok_or(bool_error!("scratchpad not yet available"))?;
        let column = scratch.current_column as u32;
        let data = &mut scratch.data;
        self.draw_column(
            sample,
            video_frame.comp_stride(0) as u32,
            video_frame.comp_pstride(0) as u32,
            video_frame.height(),
            column,
            data,
        );
        scratch.copy_into(video_frame)?;
        scratch.next();
        Ok(())
    }
}

#[glib::object_subclass]
impl ObjectSubclass for AudioSpectrogram {
    const NAME: &'static str = "GstAudioSpectrogram";
    type Type = super::AudioSpectrogram;
    type ParentType = AudioVisualizer;
}

impl ObjectImpl for AudioSpectrogram {}
impl GstObjectImpl for AudioSpectrogram {}

impl ElementImpl for AudioSpectrogram {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                super::DESCRIPTION,
                "Visualization",
                "Renders a side-scrolling spectrogram of an audio stream",
                "Jordan Yelloz <jordan@yelloz.me>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            let sink_caps = gst_audio::AudioCapsBuilder::new()
                .format(gst_audio::AudioFormat::F32le)
                .channels(1)
                .build();
            let src_caps = gst_video::VideoCapsBuilder::new()
                .format(VideoFormat::Gray8)
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

impl AudioVisualizerImpl for AudioSpectrogram {
    fn render(
        &self,
        audio_buffer: &gst::BufferRef,
        video_frame: &mut VideoFrameRef<&mut gst::BufferRef>,
    ) -> Result<(), gst::LoggableError> {
        let Some(sample) = self.analyze(audio_buffer)? else {
            return Ok(());
        };
        self.visualize(&sample, video_frame)?;
        Ok(())
    }
    fn setup(
        &self,
        token: &AudioVisualizerSetupToken,
    ) -> Result<(), gst::LoggableError> {
        self.parent_setup(token)?;
        let Some(video_info) = self.video_info() else {
            return Ok(());
        };
        let scratch = Scratchpad::new(
            video_info.width() as usize,
            video_info.height() as usize,
            video_info.size(),
        );
        self.scratchpad.lock().unwrap().replace(scratch);
        Ok(())
    }
}
