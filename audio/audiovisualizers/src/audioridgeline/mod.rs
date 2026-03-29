// SPDX-License-Identifier: MPL-2.0

use gst::glib;
use gst::prelude::*;

/**
 * SECTION:element-audioridgeline
 */
mod imp;

const NAME: &str = "audioridgeline";
const LONG_NAME: &str = "Audio Ridgeline Plot";
const DESCRIPTION: &str = "Renders an animated 2.5D ridgeline plot of the incoming audio";
const AUTHOR: &str = "Jordan Yelloz <jordan@yelloz.me>";

glib::wrapper! {
    pub struct AudioRidgeline(ObjectSubclass<imp::AudioRidgeline>) @extends gst_pbutils::AudioVisualizer, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    /**
     * element-audioridgeline:
     *
     * ## Sample Pipelines
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
