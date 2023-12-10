// SPDX-License-Identifier: MPL-2.0
mod imp;

use gst::glib;

glib::wrapper! {
    pub struct LiveKitWebRTCSrc(ObjectSubclass<imp::LiveKitWebRTCSrc>) @extends gst::Bin, gst::Element, gst::Object, @implements gst::ChildProxy;
}
