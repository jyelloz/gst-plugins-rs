use std::{
    collections::{VecDeque, vec_deque::Iter},
    iter::Rev,
    sync::LazyLock,
};

use byte_slice_cast::AsSliceOf as _;
use gst::glib::{BoolError, bool_error};
use gst_audio::AudioBufferRef;
use spectrum_analyzer::{FrequencyLimit, FrequencySpectrum, scaling::SpectrumDataStats};

const WINDOW_SIZE: usize = 256;
const NUM_BINS: usize = WINDOW_SIZE / 2;
const SILENCE_THRESHOLD_DBFS: f32 = 90.0;

static HANN_WINDOW: LazyLock<[f32; WINDOW_SIZE]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let two_pi_i = 2.0 * std::f32::consts::PI * i as f32;
        let c = (two_pi_i / WINDOW_SIZE as f32).cos();
        0.5 * (1.0 - c)
    })
});

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new("rsaudiovisulizers", gst::DebugColorFlags::empty(), None)
});

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

pub(crate) struct History {
    rows: VecDeque<[f32; NUM_BINS]>,
    num_lines: usize,
}

impl History {
    pub(crate) fn new(num_lines: usize) -> Self {
        let mut me = Self {
            rows: VecDeque::with_capacity(num_lines),
            num_lines,
        };
        for _ in 0..num_lines {
            me.push([0f32; NUM_BINS]);
        }
        me
    }

    pub(crate) fn push(&mut self, row: [f32; NUM_BINS]) {
        if self.rows.len() >= self.num_lines {
            self.rows.pop_back();
        }
        self.rows.push_front(row);
    }

    #[inline]
    pub(crate) fn iter(&self) -> Rev<Iter<'_, [f32; NUM_BINS]>> {
        self.rows.iter().rev()
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.num_lines
    }

    #[inline]
    pub(crate) const fn num_bins(&self) -> usize {
        NUM_BINS
    }

    pub(crate) fn analyze(
        &mut self,
        buffer: AudioBufferRef<&'_ gst::BufferRef>,
    ) -> Result<bool, BoolError> {
        let Some(spectrum) = analyze_sample(buffer)? else {
            self.push([0f32; NUM_BINS]);
            return Ok(false);
        };
        let data = spectrum.data();
        let mut bins = [0.0f32; NUM_BINS];
        for (i, bin) in bins.iter_mut().enumerate() {
            if let Some((_, v)) = data.get(i) {
                *bin = v.val();
            }
        }
        self.push(bins);
        Ok(true)
    }
}

pub(crate) fn analyze_sample(
    buffer: AudioBufferRef<&'_ gst::BufferRef>,
) -> Result<Option<FrequencySpectrum>, BoolError> {
    let audio_info = buffer.info();
    let bytes = buffer.plane_data(0)?;
    let samples = bytes
        .as_slice_of::<f32>()
        .map_err(|e| bool_error!("failed to interpret audio buffer as f32 array: {e:?}"))?;

    if samples.len() < WINDOW_SIZE {
        gst::debug!(CAT, "not enough data for FFT, skipping");
        return Ok(None);
    }

    let window_samples = &samples[samples.len() - WINDOW_SIZE..];
    let window: [f32; WINDOW_SIZE] = std::array::from_fn(|i| window_samples[i] * HANN_WINDOW[i]);
    let rate = audio_info.rate();

    let scaler = |val: f32, stats: &_| scale_dbfs_to_normalized(scale_to_dbfs(val, stats), stats);
    let spectrum = spectrum_analyzer::samples_fft_to_spectrum(
        &window,
        rate,
        FrequencyLimit::All,
        Some(&scaler),
    )
    .map_err(|e| bool_error!("failed to analyze sample: {:?}", e))?;

    Ok(Some(spectrum))
}
