// SPDX-License-Identifier: MPL-2.0

use gst::glib;
use gst::prelude::*;

/**
 * SECTION:element-audiospectrogram
 */
mod imp;

const NAME: &str = "audiospectrogram";
const DESCRIPTION: &str = "Audio Spectrogram renderer";

glib::wrapper! {
    pub struct AudioSpectrogram(ObjectSubclass<imp::AudioSpectrogram>) @extends gst_pbutils::AudioVisualizer, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    /**
     * element-audiospectrogram:
     *
     * The `audiospectrogram` renders a side-scrolling grayscale spectrogram of
     * an audio stream.
     *
     * ## Sample Pipelines
     *
     * Basic grayscale spectrogram from your local audio capture source:
     *
     * ```shell
     * gst-launch-1.0 \
     *   autoaudiosrc \
     *   ! queue \
     *   ! audioconvert \
     *   ! audioresample \
     *   ! audiospectrogram \
     *   ! videoconvert \
     *   ! queue \
     *   ! autovideosink
     * ```
     *
     * Since the element only produces grayscale, you might want to colorize it
     * using the `coloreffects` element:
     *
     * ```shell
     * gst-launch-1.0 \
     *   autoaudiosrc \
     *   ! queue \
     *   ! audioconvert \
     *   ! audioresample \
     *   ! audiospectrogram \
     *   ! videoconvert \
     *   ! coloreffects preset=xpro \
     *   ! queue \
     *   ! autovideosink
     * ```
     */
    gst::Element::register(
        Some(plugin),
        NAME,
        gst::Rank::NONE,
        AudioSpectrogram::static_type(),
    )?;
    Ok(())
}
