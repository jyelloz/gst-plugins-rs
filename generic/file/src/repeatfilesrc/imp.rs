// Copyright (C) 2026 Jordan Yelloz <jordan@yelloz.me>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
//
// SPDX-License-Identifier: MIT OR Apache-2.0

use gst::{glib, prelude::*, subclass::prelude::*};

use std::sync::{LazyLock, Mutex};

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "repeatfilesrc",
        gst::DebugColorFlags::empty(),
        Some("Repeating File Source"),
    )
});

#[derive(Debug, Default)]
struct Settings {
    location: Option<String>,
}

#[derive(Default)]
enum State {
    #[default]
    Initial,
    Running {
        src: gst::Element,
    },
}

impl State {
    fn running(src: gst::Element) -> Self {
        Self::Running { src }
    }
    fn append_fragment(&self, path: String) {
        if let Self::Running { src } = self {
            src.emit_by_name::<bool>(
                "add-fragment",
                &[&path, &None::<gst::ClockTime>, &None::<gst::ClockTime>],
            );
        }
    }
}

#[derive(Default)]
pub struct RepeatFileSrc {
    settings: Mutex<Settings>,
    state: Mutex<State>,
}

impl RepeatFileSrc {
    fn set_location(&self, location: Option<String>) -> Result<(), glib::Error> {
        let mut settings_lock = self.settings.lock().unwrap();
        settings_lock.location = location;
        Ok(())
    }
    fn get_location(&self) -> Option<String> {
        let settings_lock = self.settings.lock().unwrap();
        settings_lock.location.clone()
    }
}

#[glib::object_subclass]
impl ObjectSubclass for RepeatFileSrc {
    const NAME: &'static str = "GstRepeatFileSrc";
    type Type = super::RepeatFileSrc;
    type ParentType = gst::Bin;
}

impl ObjectImpl for RepeatFileSrc {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![glib::ParamSpecString::builder("location")
                .nick("File Location")
                .blurb("Location of the file to read from")
                .mutable_ready()
                .build()]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "location" => {
                let location = value
                    .get::<Option<String>>()
                    .expect("type checked upstream");
                let _ = self.set_location(location);
            }
            _ => unimplemented!(),
        };
    }

    fn property(&self, _: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "location" => {
                let location = self.get_location();
                glib::Value::from(location)
            }
            _ => unimplemented!(),
        }
    }

    fn constructed(&self) {
        self.parent_constructed();

        let obj = self.obj();

        obj.set_suppressed_flags(gst::ElementFlags::SOURCE | gst::ElementFlags::SINK);
        obj.set_element_flags(gst::ElementFlags::SOURCE);
    }
}

impl GstObjectImpl for RepeatFileSrc {}

impl ElementImpl for RepeatFileSrc {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Repeating File Source",
                "Source/File",
                "Repeat a file",
                "Jordan Yelloz <jordan@yelloz.me>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
            let caps = gst::Caps::new_any();
            let src_pad_template = gst::PadTemplate::with_gtype(
                "src_%u",
                gst::PadDirection::Src,
                gst::PadPresence::Sometimes,
                &caps,
                gst::GhostPad::static_type(),
            )
            .unwrap();

            vec![src_pad_template]
        });

        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        let obj = self.obj();

        if transition == gst::StateChange::NullToReady {
            let Some(location) = self.get_location() else {
                return Err(gst::StateChangeError);
            };
            let splitmuxsrc = gst::ElementFactory::make("splitmuxsrc")
                .build()
                .map_err(|_| gst::StateChangeError)?;
            splitmuxsrc.connect_pad_added({
                let weak = obj.downgrade();
                move |_, pad| {
                    let Some(parent) = weak.upgrade() else {
                        return;
                    };

                    let identity = gst::ElementFactory::make("identity")
                        .property("single-segment", true)
                        .build()
                        .expect("failed to build identity");

                    let id_sink = identity.static_pad("sink").expect("sink pad");
                    let id_src = identity.static_pad("src").expect("src pad");

                    parent.add(&identity).expect("failed to add identity");

                    identity
                        .sync_state_with_parent()
                        .expect("failed to sync state");

                    pad.link(&id_sink).expect("failed to link");

                    let templ = parent.pad_template("src_%u").expect("pad template");

                    let ghostpad =
                        gst::GhostPad::builder_from_template_with_target(&templ, &id_src)
                            .expect("failed to create new ghostpad")
                            .name_if_some(Some(pad.name()))
                            .build();

                    parent.add_pad(&ghostpad).expect("failed to add new pad");
                }
            });
            obj.add(&splitmuxsrc).map_err(|_| gst::StateChangeError)?;
            let mut state = self.state.lock().unwrap();
            *state = State::running(splitmuxsrc.clone());
            state.append_fragment(location);
            splitmuxsrc
                .sync_state_with_parent()
                .map_err(|_| gst::StateChangeError)?;
        }

        self.parent_change_state(transition)
    }
}

impl BinImpl for RepeatFileSrc {
    fn handle_message(&self, message: gst::Message) {
        if message.has_name("splitmuxsrc-fragment-started") {
            if let Some(location) = self.get_location() {
                let state = self.state.lock().unwrap();
                state.append_fragment(location);
            }
        }
        self.parent_handle_message(message)
    }
}
