// SPDX-License-Identifier: MPL-2.0

use gst::glib;
use gst::prelude::*;

/**
 * SECTION:element-audioridgeline
 */
mod imp;

const NAME: &str = "audioridgeline";
const DESCRIPTION: &str = "2.5D Audio ridgeline plotter";

glib::wrapper! {
    pub struct AudioRidgeline(ObjectSubclass<imp::AudioRidgeline>) @extends gst_pbutils::AudioVisualizer, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    /**
     * element-audioridgeline:
     *
     * The `audioridgeline` renders a ridgeline plot of an audio stream.
     *
     * ## Sample Pipelines
     *
     * Basic 3D spectrogram from your local audio capture source:
     *
     * ```shell
     * gst-launch-1.0 \
     *   autoaudiosrc \
     *   ! queue \
     *   ! audioconvert \
     *   ! audioresample \
     *   ! audioridgeline \
     *   ! videoconvert \
     *   ! queue \
     *   ! autovideosink
     * ```
     */
    gst::Element::register(
        Some(plugin),
        NAME,
        gst::Rank::NONE,
        AudioRidgeline::static_type(),
    )?;
    Ok(())
}
