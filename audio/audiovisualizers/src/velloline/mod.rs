// SPDX-License-Identifier: MPL-2.0

use gst::glib;
use gst::prelude::*;

/**
 * SECTION:element-velloline
 */
mod imp;

const NAME: &str = "velloline";
const DESCRIPTION: &str = "Vello-rendered white line on black background";

glib::wrapper! {
    pub struct VelloLine(ObjectSubclass<imp::VelloLine>) @extends gst_pbutils::AudioVisualizer, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    /**
     * element-velloline:
     *
     * ## Sample Pipelines
     *
     * ```shell
     * gst-launch-1.0 \
     *   autoaudiosrc \
     *   ! queue \
     *   ! audioconvert \
     *   ! audioresample \
     *   ! velloline \
     *   ! videoconvert \
     *   ! queue \
     *   ! autovideosink
     * ```
     */
    gst::Element::register(
        Some(plugin),
        NAME,
        gst::Rank::NONE,
        VelloLine::static_type(),
    )?;
    Ok(())
}
