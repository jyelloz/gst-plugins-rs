// SPDX-License-Identifier: MPL-2.0

use gst::glib;
use gst::prelude::*;
/**
 * SECTION:element-chafasink
 */

mod imp;

const NAME: &str = "chafasink";
const DESCRIPTION: &str = "Chafa terminal graphics sink";

glib::wrapper! {
    pub struct ChafaSink(ObjectSubclass<imp::ChafaSink>)
        @extends gst_video::VideoSink, gst_base::BaseSink, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    /**
     * element-chafasink:
     *
     * The `chafasink` renders incoming video into the terminal using the
     * [Chafa](https://hpjansson.org/chafa/) terminal graphics library.
     *
     * ## Sample Pipeline
     *
     * ```shell
     * gst-launch-1.0 --quiet --no-position videotestsrc ! chafasink
     * ```
     */
    gst::Element::register(
        Some(plugin),
        NAME,
        gst::Rank::NONE,
        ChafaSink::static_type(),
    )
}
