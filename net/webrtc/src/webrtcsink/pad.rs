// SPDX-License-Identifier: MPL-2.0

use gst::{glib, prelude::*, subclass::prelude::*};
use once_cell::sync::Lazy;
use std::sync::Mutex;

#[derive(Default)]
pub struct WebRTCSinkPad {
    settings: Mutex<Settings>,
}

#[derive(Debug, Default)]
struct Settings {
    msid: Option<String>,
    mid: Option<String>,
    rid: Option<String>,
}

#[glib::object_subclass]
impl ObjectSubclass for WebRTCSinkPad {
    const NAME: &'static str = "GstWebRTCSinkPad";
    type Type = super::WebRTCSinkPad;
    type ParentType = gst::GhostPad;
}

impl ObjectImpl for WebRTCSinkPad {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPS: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::builder("msid")
                    .flags(glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY)
                    .blurb("Remote MediaStream ID in use for this pad")
                    .build(),
                glib::ParamSpecString::builder("mid")
                    .flags(glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY)
                    .blurb("The Media Identification (MID) value to produce from this pad")
                    .build(),
                glib::ParamSpecString::builder("rid")
                    .flags(glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY)
                    .blurb("The RtpStreamId (RID) value to produce from this pad")
                    .build(),
            ]
        });
        PROPS.as_ref()
    }
    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let mut settings = self.settings.lock().unwrap();
        match pspec.name() {
            "msid" => {
                settings.msid = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
            }
            "mid" => {
                settings.mid = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
            }
            "rid" => {
                settings.rid = value
                    .get::<Option<String>>()
                    .expect("type checked upstream")
            }
            name => panic!("no writable property {name:?}"),
        }
    }
    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();
        match pspec.name() {
            "msid" => settings.msid.to_value(),
            "mid" => settings.mid.to_value(),
            "rid" => settings.rid.to_value(),
            name => panic!("no readable property {name:?}"),
        }
    }
}

impl GstObjectImpl for WebRTCSinkPad {}
impl PadImpl for WebRTCSinkPad {}
impl ProxyPadImpl for WebRTCSinkPad {}
impl GhostPadImpl for WebRTCSinkPad {}
