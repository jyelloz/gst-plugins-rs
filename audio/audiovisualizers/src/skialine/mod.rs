// SPDX-License-Identifier: MPL-2.0

use gst::glib;
use gst::prelude::*;

/**
 * SECTION:element-skialine
 */
mod imp;

const NAME: &str = "skialine";
const DESCRIPTION: &str = "Skia-rendered white line on black background";

glib::wrapper! {
    pub struct SkiaLine(ObjectSubclass<imp::SkiaLine>) @extends gst_pbutils::AudioVisualizer, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    /**
     * element-skialine:
     *
     * The `skialine` renders a static white diagonal line on a black
     * background using the skia-safe bindings, disregarding the incoming
     * audio data entirely.
     *
     * ## Sample Pipelines
     *
     * ```shell
     * gst-launch-1.0 \
     *   autoaudiosrc \
     *   ! queue \
     *   ! audioconvert \
     *   ! audioresample \
     *   ! skialine \
     *   ! videoconvert \
     *   ! queue \
     *   ! autovideosink
     * ```
     */
    gst::Element::register(
        Some(plugin),
        NAME,
        gst::Rank::NONE,
        SkiaLine::static_type(),
    )?;
    Ok(())
}
