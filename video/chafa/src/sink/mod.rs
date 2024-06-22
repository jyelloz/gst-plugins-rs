// SPDX-License-Identifier: MPL-2.0

/**
 * SECTION:element-chafasink
 *
 */
use gst::glib::{self, prelude::*};

mod imp;

glib::wrapper! {
    pub struct ChafaSink(ObjectSubclass<imp::ChafaSink>)
        @extends gst_video::VideoSink, gst_base::BaseSink, gst::Element, gst::Object;
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "chafasink",
        gst::Rank::NONE,
        ChafaSink::static_type(),
    )
}
