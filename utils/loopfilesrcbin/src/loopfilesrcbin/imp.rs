// Copyright (C) 2025 Jordan Yelloz <jordan@yelloz.me>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.
//
// SPDX-License-Identifier: MPL-2.0

use std::sync::Mutex;

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;

use std::sync::LazyLock;

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "loopfilesrcbin",
        gst::DebugColorFlags::empty(),
        Some("Looping File Source Bin"),
    )
});

#[derive(Debug, Clone, Default)]
struct Settings {
    location: String,
}

#[derive(Debug, Clone)]
struct State {
    splitmuxsrc: gst::Element,
    streamsynchronizer: gst::Element,
}

impl State {
    pub fn enqueue_fragment(&self, location: &str) {
        self.splitmuxsrc.emit_by_name::<bool>(
            "add-fragment",
            &[
                &location.to_value(),
                &None::<gst::ClockTime>.to_value(),
                &None::<gst::ClockTime>.to_value(),
            ],
        );
    }

    fn splitmuxsrc_srcpad_added(&self, srcpad: &gst::Pad) -> Result<(), glib::BoolError> {
        let sinkpad = self
            .streamsynchronizer
            .request_pad_simple("sink_%u")
            .ok_or(glib::bool_error!(
                "failed to request sinkpad from stream synchronizer"
            ))?;
        srcpad.link(&sinkpad).map_err(|e| {
            glib::bool_error!("failed to link splitmuxsrc to stream synchronizer: {e:?}")
        })?;
        Ok(())
    }

    fn drop_duration_query(
        &self,
        pad: &gst::GhostPad,
        obj: Option<&impl IsA<gst::Object>>,
        query: &mut gst::QueryRef,
    ) -> bool {
        match query.view_mut() {
            gst::QueryViewMut::Duration(..) => false,
            _ => gst::Pad::query_default(pad, obj, query),
        }
    }

    fn remove_segment_duration(
        &self,
        pad: &gst::GhostPad,
        info: &mut gst::PadProbeInfo<'_>,
    ) -> gst::PadProbeReturn {
        let Some(id) = info.id.take() else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(event) = info.event() else {
            return gst::PadProbeReturn::Ok;
        };
        let gst::EventView::Segment(segment) = event.view() else {
            return gst::PadProbeReturn::Ok;
        };

        let segment = segment.segment();
        if segment.format() == gst::Format::Time {
            return gst::PadProbeReturn::Ok;
        }
        pad.remove_probe(id);

        let mut segment = segment.clone();
        segment.set_duration(None::<gst::ClockTime>);
        segment.set_stop(None::<gst::ClockTime>);

        pad.push_event(gst::event::Segment::new(&segment));

        self.register_segment_probe(pad);

        gst::PadProbeReturn::Drop
    }

    fn register_segment_probe(&self, pad: &gst::GhostPad) {
        pad.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM,
            glib::clone! {
                #[strong(rename_to = me)] self,
                move |pad, info| me.remove_segment_duration(pad, info),
            },
        );
    }

    fn streamsynchronizer_srcpad_added(
        &self,
        srcpad: &gst::Pad,
        bin: &gst::Bin,
    ) -> Result<(), glib::BoolError> {
        let template = bin
            .pad_template("src_%u")
            .expect("failed to load srcpad template");
        let ghostpad = gst::GhostPad::builder_from_template_with_target(&template, srcpad)?
            .query_function(glib::clone! {
                #[strong(rename_to = me)] self,
                move |pad, obj, query| me.drop_duration_query(pad, obj, query)
            })
            .build();
        self.register_segment_probe(&ghostpad);
        bin.add_pad(&ghostpad)?;
        Ok(())
    }

    pub fn setup(&self, bin: &gst::Bin) {
        self.splitmuxsrc.connect_pad_added(glib::clone! {
            #[strong(rename_to = me)] self,
            move |_, pad| me.splitmuxsrc_srcpad_added(pad).unwrap()
        });
        self.streamsynchronizer.connect_pad_added(glib::clone! {
            #[strong(rename_to = me)] self,
            #[weak] bin,
            move |_, pad| {
                if pad.direction() == gst::PadDirection::Sink {
                    return;
                };
                me.streamsynchronizer_srcpad_added(pad, &bin).unwrap()
            }
        });
    }
}

#[derive(Default)]
pub struct LoopFileSrcBin {
    settings: Mutex<Settings>,
    state: Mutex<Option<State>>,
}

#[glib::object_subclass]
impl ObjectSubclass for LoopFileSrcBin {
    const NAME: &'static str = "GstLoopFileSrcBin";
    type Type = super::LoopFileSrcBin;
    type ParentType = gst::Bin;
}

impl ObjectImpl for LoopFileSrcBin {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![glib::ParamSpecString::builder("location")
                .nick("File Location")
                .blurb("Location of file to loop over")
                .mutable_ready()
                .build()]
        });
        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "location" => {
                let mut settings = self.settings.lock().unwrap();
                let new_value = value.get().expect("type checked upstream");
                settings.location = new_value;
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "location" => {
                let settings = self.settings.lock().unwrap();
                settings.location.to_value()
            }
            _ => unimplemented!(),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();

        let bin = self.obj();
        bin.set_suppressed_flags(gst::ElementFlags::SOURCE | gst::ElementFlags::SINK);
        bin.set_element_flags(gst::ElementFlags::SOURCE);
    }
}

impl GstObjectImpl for LoopFileSrcBin {}

impl BinImpl for LoopFileSrcBin {
    fn handle_message(&self, msg: gst::Message) {
        let gst::MessageView::Element(_msg) = msg.view() else {
            self.parent_handle_message(msg);
            return;
        };
        if !msg.has_name("splitmuxsrc-fragment-started") {
            self.parent_handle_message(msg);
            return;
        }
        let settings = self.settings.lock().unwrap();
        let location = &settings.location;
        let state = self.state.lock().unwrap();
        if let Some(state) = state.as_ref() {
            state.enqueue_fragment(location);
        }
    }
}

impl ElementImpl for LoopFileSrcBin {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Loop File Source",
                "Generic/Source",
                "Repeat a file source infinitely",
                "Jordan Yelloz <jordan@yelloz.me>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let template = gst::PadTemplate::new(
                "src_%u",
                gst::PadDirection::Src,
                gst::PadPresence::Sometimes,
                &gst::Caps::new_any(),
            )
            .unwrap();
            vec![template]
        });

        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        if transition == gst::StateChange::NullToReady {
            if let Err(e) = self.start() {
                gst::error!(CAT, imp = self, "failed to start: {e:?}");
                return Err(gst::StateChangeError);
            }
        }

        let res = self.parent_change_state(transition);

        if transition == gst::StateChange::ReadyToNull {
            self.stop();
        }

        res
    }
}

impl LoopFileSrcBin {
    fn start(&self) -> Result<(), glib::BoolError> {
        let mut state = self.state.lock().unwrap();

        let splitmuxsrc = gst::ElementFactory::make("splitmuxsrc").build()?;
        let streamsynchronizer = gst::ElementFactory::make("streamsynchronizer").build()?;

        let bin = self.obj();
        bin.add(&splitmuxsrc)?;
        bin.add(&streamsynchronizer)?;

        let settings = self.settings.lock().unwrap();
        let location = settings.location.clone();

        *state = Some({
            let state = State {
                splitmuxsrc,
                streamsynchronizer,
            };
            state.enqueue_fragment(&location);
            state.setup(bin.upcast_ref());
            state
        });

        Ok(())
    }

    fn stop(&self) {
        let bin = self.obj();
        bin.remove_many(bin.children())
            .expect("failed to remove children");

        for pad in bin.src_pads() {
            bin.remove_pad(&pad).expect("failed to remove ghostpad");
        }

        let mut state = self.state.lock().unwrap();

        *state = None;
    }
}
