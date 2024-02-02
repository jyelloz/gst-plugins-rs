// SPDX-License-Identifier: MPL-2.0

use crate::signaller::Signallable;

/**
 * SECTION:element-webrtcsink
 * @symbols:
 *   - GstBaseWebRTCSink
 *   - GstRSWebRTCSignallableIface
 *
 * `webrtcsink` is an element that can be used to serve media streams
 * to multiple consumers through WebRTC.
 *
 * It uses a signaller that implements the protocol supported by the default
 * signalling server we additionally provide, take a look at the subclasses of
 * #GstBaseWebRTCSink for other supported protocols, or implement your own.
 *
 * See the [documentation of the plugin](plugin-rswebrtc) for more information
 * on features and usage.
 */

/**
 * GstBaseWebRTCSink:
 * @title: Base class for WebRTC producers
 *
 * Base class for WebRTC sinks to implement and provide their own protocol for.
 */

/**
 * GstRSWebRTCSignallableIface:
 * @title: Interface for WebRTC signalling protocols
 *
 * Interface that WebRTC elements can implement their own protocol with.
 */
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

mod homegrown_cc;

mod imp;
mod pad;

glib::wrapper! {
    pub struct BaseWebRTCSink(ObjectSubclass<imp::BaseWebRTCSink>) @extends gst::Bin, gst::Element, gst::Object, @implements gst::ChildProxy, gst_video::Navigation;
}

glib::wrapper! {
    pub struct WebRTCSinkPad(ObjectSubclass<pad::WebRTCSinkPad>) @extends gst::GhostPad, gst::ProxyPad, gst::Pad, gst::Object;
}

glib::wrapper! {
    pub struct WebRTCSink(ObjectSubclass<imp::WebRTCSink>) @extends BaseWebRTCSink, gst::Bin, gst::Element, gst::Object, @implements gst::ChildProxy, gst_video::Navigation;
}

glib::wrapper! {
    pub struct AwsKvsWebRTCSink(ObjectSubclass<imp::AwsKvsWebRTCSink>) @extends BaseWebRTCSink, gst::Bin, gst::Element, gst::Object, @implements gst::ChildProxy, gst_video::Navigation;
}

glib::wrapper! {
    pub struct WhipWebRTCSink(ObjectSubclass<imp::WhipWebRTCSink>) @extends BaseWebRTCSink, gst::Bin, gst::Element, gst::Object, @implements gst::ChildProxy, gst_video::Navigation;
}

glib::wrapper! {
    pub struct LiveKitWebRTCSink(ObjectSubclass<imp::LiveKitWebRTCSink>) @extends BaseWebRTCSink, gst::Bin, gst::Element, gst::Object, @implements gst::ChildProxy, gst_video::Navigation;
}

#[derive(thiserror::Error, Debug)]
pub enum WebRTCSinkError {
    #[error("no session with id")]
    NoSessionWithId(String),
    #[error("consumer refused media")]
    ConsumerRefusedMedia { session_id: String, media_idx: u32 },
    #[error("consumer did not provide valid payload for media")]
    ConsumerNoValidPayload { session_id: String, media_idx: u32 },
    #[error("SDP mline index is currently mandatory")]
    MandatorySdpMlineIndex,
    #[error("duplicate session id")]
    DuplicateSessionId(String),
    #[error("error setting up consumer pipeline")]
    SessionPipelineError {
        session_id: String,
        peer_id: String,
        details: String,
    },
}

impl Default for BaseWebRTCSink {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl BaseWebRTCSink {
    pub fn with_signaller(signaller: Signallable) -> Self {
        let ret: BaseWebRTCSink = glib::Object::new();

        let ws = ret.imp();
        ws.set_signaller(signaller).unwrap();

        ret
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone, Copy, glib::Enum)]
#[repr(u32)]
#[enum_type(name = "GstWebRTCSinkCongestionControl")]
pub enum WebRTCSinkCongestionControl {
    #[enum_value(name = "Disabled: no congestion control is applied", nick = "disabled")]
    Disabled,
    #[enum_value(name = "Homegrown: simple sender-side heuristic", nick = "homegrown")]
    Homegrown,
    #[enum_value(name = "Google Congestion Control algorithm", nick = "gcc")]
    GoogleCongestionControl,
}

#[glib::flags(name = "GstWebRTCSinkMitigationMode")]
enum WebRTCSinkMitigationMode {
    #[flags_value(name = "No mitigation applied", nick = "none")]
    NONE = 0b00000000,
    #[flags_value(name = "Lowered resolution", nick = "downscaled")]
    DOWNSCALED = 0b00000001,
    #[flags_value(name = "Lowered framerate", nick = "downsampled")]
    DOWNSAMPLED = 0b00000010,
}

pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    WebRTCSinkPad::static_type().mark_as_plugin_api(gst::PluginAPIFlags::empty());
    BaseWebRTCSink::static_type().mark_as_plugin_api(gst::PluginAPIFlags::empty());
    WebRTCSinkCongestionControl::static_type().mark_as_plugin_api(gst::PluginAPIFlags::empty());
    gst::Element::register(
        Some(plugin),
        "webrtcsink",
        gst::Rank::None,
        WebRTCSink::static_type(),
    )?;
    gst::Element::register(
        Some(plugin),
        "awskvswebrtcsink",
        gst::Rank::None,
        AwsKvsWebRTCSink::static_type(),
    )?;
    gst::Element::register(
        Some(plugin),
        "whipwebrtcsink",
        gst::Rank::None,
        WhipWebRTCSink::static_type(),
    )?;
    gst::Element::register(
        Some(plugin),
        "livekitwebrtcsink",
        gst::Rank::None,
        LiveKitWebRTCSink::static_type(),
    )?;

    Ok(())
}
