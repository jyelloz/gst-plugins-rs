use std::{
    collections::{VecDeque, vec_deque::Iter},
    iter::Rev,
    sync::LazyLock,
};

use crate::CAT;
use byte_slice_cast::AsSliceOf as _;
use gst::glib::{BoolError, bool_error};
use spectrum_analyzer::{FrequencyLimit, scaling::SpectrumDataStats};

pub(crate) const WINDOW_SIZE: usize = 256;
const MIN_BUFFER_SIZE: usize = std::mem::size_of::<f32>() * WINDOW_SIZE;
const NUM_BINS: usize = WINDOW_SIZE / 2;
const SILENCE_THRESHOLD_DBFS: f32 = 90.0;

static HANN_WINDOW: LazyLock<[f32; WINDOW_SIZE]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let two_pi_i = 2.0 * std::f32::consts::PI * i as f32;
        let c = (two_pi_i / WINDOW_SIZE as f32).cos();
        0.5 * (1.0 - c)
    })
});

#[inline]
pub(crate) const fn empty_sample() -> [f32; NUM_BINS] {
    [0f32; NUM_BINS]
}

fn scale_to_dbfs(amplitude: f32, _: &SpectrumDataStats) -> f32 {
    let normalized = amplitude.abs() / WINDOW_SIZE as f32;
    if normalized <= 0.0 {
        -SILENCE_THRESHOLD_DBFS
    } else {
        (20.0 * normalized.log10()).max(-SILENCE_THRESHOLD_DBFS)
    }
}

fn scale_dbfs_to_normalized(dbfs: f32, stats: &SpectrumDataStats) -> f32 {
    (dbfs + SILENCE_THRESHOLD_DBFS).max(0f32) * stats.n / SILENCE_THRESHOLD_DBFS
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
            me.push(empty_sample());
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
        buffer: &[u8],
        info: &gst_audio::AudioInfo,
    ) -> Result<(), BoolError> {
        let bins = analyze_sample(buffer, info)
            .inspect_err(|e| {
                gst::error!(CAT, "failed to analyze sample: {e:?}");
            })
            .ok()
            .flatten()
            .unwrap_or_else(empty_sample);
        self.push(bins);
        Ok(())
    }
}

pub(crate) fn analyze_sample(
    buffer: &[u8],
    info: &gst_audio::AudioInfo,
) -> Result<Option<[f32; NUM_BINS]>, BoolError> {
    if buffer.len() < MIN_BUFFER_SIZE {
        return Err(bool_error!(
            "not enough data for FFT, need at least {MIN_BUFFER_SIZE} bytes"
        ));
    }

    let samples = buffer
        .as_slice_of::<f32>()
        .map_err(|e| bool_error!("failed to interpret audio buffer as f32 array: {e:?}"))?;

    let window_samples = &samples[samples.len() - WINDOW_SIZE..];
    let window: [f32; WINDOW_SIZE] = std::array::from_fn(|i| window_samples[i] * HANN_WINDOW[i]);
    let rate = info.rate();

    let scaler = |val: f32, stats: &_| scale_dbfs_to_normalized(scale_to_dbfs(val, stats), stats);
    let spectrum = spectrum_analyzer::samples_fft_to_spectrum(
        &window,
        rate,
        FrequencyLimit::All,
        Some(&scaler),
    )
    .map_err(|e| bool_error!("failed to analyze sample: {:?}", e))?;

    let data = spectrum.data();
    let mut bins = empty_sample();
    for (i, bin) in bins.iter_mut().enumerate() {
        if let Some((_, v)) = data.get(i) {
            *bin = v.val();
        }
    }
    Ok(Some(bins))
}
