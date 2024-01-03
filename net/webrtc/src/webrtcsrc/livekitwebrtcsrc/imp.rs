// SPDX-License-Identifier: MPL-2.0

use gst::prelude::*;

use crate::signaller::{prelude::*, Signallable};
use crate::utils::{Codec, Codecs, AUDIO_CAPS, RTP_CAPS, VIDEO_CAPS};
use crate::webrtcsrc::WebRTCSrcPad;
use crate::livekit_signaller::LiveKitSignaller;
use anyhow::{Context, Error};
use gst::glib;
use gst::glib::once_cell::sync::Lazy;
use gst::subclass::prelude::*;
use gst_webrtc::WebRTCDataChannel;
use std::collections::HashSet;
use std::sync::atomic::AtomicU16;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

const DEFAULT_STUN_SERVER: Option<&str> = Some("stun://stun.l.google.com:19302");

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "livekitwebrtcsrc",
        gst::DebugColorFlags::empty(),
        Some("LiveKit WebRTC src"),
    )
});

const RTP_TWCC_URI: &str =
    "http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01";

struct Settings {
    stun_server: Option<String>,
    turn_servers: gst::Array,
    signaller: Signallable,
    meta: Option<gst::Structure>,
    video_codecs: Vec<Codec>,
    audio_codecs: Vec<Codec>,
}

#[derive(Default)]
pub struct LiveKitWebRTCSrc {
    settings: Mutex<Settings>,
    n_video_pads: AtomicU16,
    n_audio_pads: AtomicU16,
    state: Mutex<State>,
}

#[glib::object_subclass]
impl ObjectSubclass for LiveKitWebRTCSrc {
    const NAME: &'static str = "GstLiveKitWebRTCSrc";
    type Type = super::LiveKitWebRTCSrc;
    type ParentType = gst::Bin;
    type Interfaces = (gst::ChildProxy,);
}

impl ObjectImpl for LiveKitWebRTCSrc {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPS: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
            vec![
                glib::ParamSpecString::builder("stun-server")
                    .nick("The STUN server to use")
                    .blurb("The STUN server of the form stun://host:port")
                    .flags(glib::ParamFlags::READWRITE)
                    .default_value(DEFAULT_STUN_SERVER)
                    .mutable_ready()
                    .build(),
                gst::ParamSpecArray::builder("turn-servers")
                    .nick("List of TURN servers to use")
                    .blurb("The TURN servers of the form <\"turn(s)://username:password@host:port\", \"turn(s)://username1:password1@host1:port1\">")
                    .element_spec(&glib::ParamSpecString::builder("turn-server")
                        .nick("TURN Server")
                        .blurb("The TURN server of the form turn(s)://username:password@host:port.")
                        .build()
                    )
                    .mutable_ready()
                    .build(),
                glib::ParamSpecObject::builder::<Signallable>("signaller")
                    .flags(glib::ParamFlags::READWRITE | glib::ParamFlags::CONSTRUCT_ONLY)
                    .blurb("The Signallable object to use to handle WebRTC Signalling")
                    .build(),
                glib::ParamSpecBoxed::builder::<gst::Structure>("meta")
                    .flags(glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY)
                    .blurb("Free form metadata about the consumer")
                    .build(),
                gst::ParamSpecArray::builder("video-codecs")
                    .flags(glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY)
                    .blurb(&format!("Names of video codecs to be be used during the SDP negotiation. Valid values: [{}]",
                        Codecs::video_codec_names().into_iter().collect::<Vec<String>>().join(", ")
                    ))
                    .element_spec(&glib::ParamSpecString::builder("video-codec-name").build())
                    .build(),
                gst::ParamSpecArray::builder("audio-codecs")
                    .flags(glib::ParamFlags::READWRITE | gst::PARAM_FLAG_MUTABLE_READY)
                    .blurb(&format!("Names of audio codecs to be be used during the SDP negotiation. Valid values: [{}]",
                        Codecs::audio_codec_names().into_iter().collect::<Vec<String>>().join(", ")
                    ))
                    .element_spec(&glib::ParamSpecString::builder("audio-codec-name").build())
                    .build(),
            ]
        });

        PROPS.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        let mut settings = self.settings.lock().unwrap();
        match pspec.name() {
            "signaller" => {
                let signaller = value
                    .get::<Option<Signallable>>()
                    .expect("type checked upstream");
                if let Some(signaller) = signaller {
                    settings.signaller = signaller;
                }
            }
            "video-codecs" => {
                settings.video_codecs = value
                    .get::<gst::ArrayRef>()
                    .expect("type checked upstream")
                    .as_slice()
                    .iter()
                    .filter_map(|codec_name| {
                        Codecs::find(codec_name.get::<&str>().expect("type checked upstream"))
                    })
                    .collect::<Vec<Codec>>()
            }
            "audio-codecs" => {
                settings.audio_codecs = value
                    .get::<gst::ArrayRef>()
                    .expect("type checked upstream")
                    .as_slice()
                    .iter()
                    .filter_map(|codec_name| {
                        Codecs::find(codec_name.get::<&str>().expect("type checked upstream"))
                    })
                    .collect::<Vec<Codec>>()
            }
            "stun-server" => {
                settings.stun_server = value
                    .get::<Option<String>>()
                    .expect("type checked upstream");
            }
            "turn-servers" => {
                let mut settings = settings;
                settings.turn_servers = value.get::<gst::Array>().expect("type checked upstream");
            }
            "meta" => {
                settings.meta = value
                    .get::<Option<gst::Structure>>()
                    .expect("type checked upstream")
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        let settings = self.settings.lock().unwrap();
        match pspec.name() {
            "signaller" => settings.signaller.to_value(),
            "video-codecs" => gst::Array::new(
                settings
                    .video_codecs
                    .iter()
                    .map(|v| &v.name),
            )
            .to_value(),
            "audio-codecs" => gst::Array::new(
                settings
                    .audio_codecs
                    .iter()
                    .map(|v| &v.name),
            )
            .to_value(),
            "stun-server" => settings.stun_server.to_value(),
            "turn-servers" => settings.turn_servers.to_value(),
            "meta" => settings.meta.to_value(),
            name => panic!("{} getter not implemented", name),
        }
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: Lazy<Vec<glib::subclass::Signal>> = Lazy::new(|| {
            vec![
                /**
                 * BaseWebRTCSrc::request-encoded-filter:
                 * @producer_id: Identifier of the producer
                 * @pad_name: The name of the output pad
                 * @allowed_caps: the allowed caps for the output pad
                 *
                 * This signal can be used to insert a filter
                 * element between:
                 *
                 * - the depayloader and the decoder.
                 * - the depayloader and downstream element if
                 *   no decoders are used.
                 *
                 * Returns: the element to insert.
                 */
                glib::subclass::Signal::builder("request-encoded-filter")
                    .param_types([
                        String::static_type(),
                        String::static_type(),
                        Option::<gst::Caps>::static_type(),
                    ])
                    .return_type::<gst::Element>()
                    .build(),
            ]
        });

        SIGNALS.as_ref()
    }

    fn constructed(&self) {
        self.parent_constructed();
        let signaller = self.settings.lock().unwrap().signaller.clone();

        self.connect_signaller(&signaller);

        let obj = &*self.obj();

        obj.set_suppressed_flags(gst::ElementFlags::SINK | gst::ElementFlags::SOURCE);
        obj.set_element_flags(gst::ElementFlags::SOURCE);
    }
}

impl Default for Settings {
    fn default() -> Self {
        let signaller = LiveKitSignaller::new_consumer();

        Self {
            stun_server: DEFAULT_STUN_SERVER.map(|v| v.to_string()),
            turn_servers: Default::default(),
            signaller: signaller.upcast(),
            meta: Default::default(),
            audio_codecs: Codecs::audio_codecs()
                .into_iter()
                .filter(|codec| codec.has_decoder())
                .collect(),
            video_codecs: Codecs::video_codecs()
                .into_iter()
                .filter(|codec| codec.has_decoder())
                .collect(),
        }
    }
}

#[allow(dead_code)]
struct SignallerSignals {
    error: glib::SignalHandlerId,
    request_meta: glib::SignalHandlerId,
    session_description: glib::SignalHandlerId,
    handle_ice: glib::SignalHandlerId,
}

impl LiveKitWebRTCSrc {
    fn publisher_pc(&self) -> gst::Bin {
        let settings = self.state.lock().unwrap();
        let bin = &settings.publisher_pc;
        bin.clone()
            .expect("publisher_pc should only be called when state >= ready")
            .clone()
            .downcast()
            .expect("publisher_pc must be a bin")
    }

    fn subscriber_pc(&self) -> gst::Bin {
        let settings = self.state.lock().unwrap();
        let bin = &settings.subscriber_pc;
        bin.clone()
            .expect("subscriber_pc should only be called when state >= ready")
            .clone()
            .downcast()
            .expect("subscriber_pc must be a bin")
    }

    const fn session_id(&self) -> &'static str {
        "-"
    }

    fn signaller(&self) -> Signallable {
        self.settings.lock().unwrap().signaller.clone()
    }

    fn producer_peer_id(&self) -> Option<String> {
        self.signaller().property::<Option<String>>("producer-peer-id")
    }

    // Maps the `webrtcbin` pad to our exposed source pad using the pad stream ID.
    fn get_src_pad_from_webrtcbin_pad(&self, webrtcbin_src: &gst::Pad) -> Option<WebRTCSrcPad> {
        let stream_id = self.get_stream_id(
            Some(webrtcbin_src.property::<gst_webrtc::WebRTCRTPTransceiver>("transceiver")),
            None,
        )?;

        self.obj().iterate_src_pads().into_iter().find_map(|s| {
            let pad = s.ok()?.downcast::<WebRTCSrcPad>().unwrap();
            if pad.imp().stream_id() == stream_id {
                Some(pad)
            } else {
                None
            }
        })
    }

    fn handle_webrtc_src_pad(&self, bin: &gst::Bin, pad: &gst::Pad) {
        gst::debug!(CAT, imp: self, "pad added {:?}, bin={:?}", pad.name(), bin.name());
        let srcpad = self.get_src_pad_from_webrtcbin_pad(pad);
        if let Some(ref srcpad) = srcpad {
            let stream_id = srcpad.imp().stream_id();
            let mut builder = gst::event::StreamStart::builder(&stream_id);
            if let Some(stream_start) = pad.sticky_event::<gst::event::StreamStart>(0) {
                builder = builder
                    .seqnum(stream_start.seqnum())
                    .group_id(stream_start.group_id().unwrap_or_else(gst::GroupId::next));
            }

            gst::debug!(CAT, imp: self, "Storing id {stream_id} on {pad:?}");
            pad.store_sticky_event(&builder.build()).ok();
        }

        let ghostpad = gst::GhostPad::builder(gst::PadDirection::Src)
            .with_target(pad)
            .unwrap()
            .proxy_pad_chain_function(glib::clone!(@weak self as this => @default-panic, move
                |pad, parent, buffer| {
                    let padret = gst::ProxyPad::chain_default(pad, parent, buffer);
                    let ret = this.state.lock().unwrap().flow_combiner.update_flow(padret);

                    ret
                }
            ))
            .proxy_pad_event_function(glib::clone!(@weak self as this => @default-panic, move |pad, parent, event| {
                let event = if let gst::EventView::StreamStart(stream_start) = event.view() {
                    let webrtcpad = pad.peer().unwrap();

                    this.get_src_pad_from_webrtcbin_pad(&webrtcpad)
                        .map(|srcpad| {
                            gst::event::StreamStart::builder(&srcpad.imp().stream_id())
                                .seqnum(stream_start.seqnum())
                                .group_id(stream_start.group_id().unwrap_or_else(gst::GroupId::next))
                                .build()
                        }).unwrap_or(event)
                } else {
                    event
                };

                gst::Pad::event_default(pad, parent, event)
            }))
            .build();

        bin.add_pad(&ghostpad)
            .expect("Adding ghostpad to the bin should always work");

        if let Some(srcpad) = srcpad {
            let producer_id = self.producer_peer_id();
            let encoded_filter = self.obj().emit_by_name::<Option<gst::Element>>(
                "request-encoded-filter",
                &[&producer_id, &srcpad.name(), &srcpad.allowed_caps()],
            );

            if srcpad.imp().needs_decoding() {
                let decodebin = gst::ElementFactory::make("decodebin3")
                    .build()
                    .expect("decodebin3 needs to be present!");
                self.obj().add(&decodebin).unwrap();
                decodebin.sync_state_with_parent().unwrap();
                decodebin.connect_pad_added(
                    glib::clone!(@weak self as this, @weak srcpad => move |_webrtcbin, pad| {
                        if pad.direction() == gst::PadDirection::Sink {
                            return;
                        }

                        srcpad.set_target(Some(pad)).unwrap();
                    }),
                );

                gst::debug!(CAT, imp: self, "Decoding for {}", srcpad.imp().stream_id());

                if let Some(encoded_filter) = encoded_filter {
                    let filter_sink_pad = encoded_filter
                        .static_pad("sink")
                        .expect("encoded filter must expose a static sink pad");

                    let parsebin = gst::ElementFactory::make("parsebin")
                        .build()
                        .expect("parsebin needs to be present!");
                    self.obj().add_many([&parsebin, &encoded_filter]).unwrap();

                    parsebin.connect_pad_added(move |_, pad| {
                        pad.link(&filter_sink_pad)
                            .expect("parsebin ! encoded_filter linking failed");
                        encoded_filter
                            .link(&decodebin)
                            .expect("encoded_filter ! decodebin3 linking failed");

                        encoded_filter.sync_state_with_parent().unwrap();
                    });

                    ghostpad
                        .link(&parsebin.static_pad("sink").unwrap())
                        .expect("webrtcbin ! parsebin linking failed");

                    parsebin.sync_state_with_parent().unwrap();
                } else {
                    let sinkpad = decodebin
                        .static_pad("sink")
                        .expect("decodebin has a sink pad");
                    ghostpad
                        .link(&sinkpad)
                        .expect("webrtcbin ! decodebin3 linking failed");
                }
            } else {
                gst::debug!(
                    CAT,
                    imp: self,
                    "NO decoding for {}",
                    srcpad.imp().stream_id()
                );

                if let Some(encoded_filter) = encoded_filter {
                    let filter_sink_pad = encoded_filter
                        .static_pad("sink")
                        .expect("encoded filter must expose a static sink pad");
                    let filter_src_pad = encoded_filter
                        .static_pad("src")
                        .expect("encoded filter must expose a static src pad");

                    self.obj().add(&encoded_filter).unwrap();

                    ghostpad
                        .link(&filter_sink_pad)
                        .expect("webrtcbin ! encoded_filter linking failed");
                    srcpad.set_target(Some(&filter_src_pad)).unwrap();

                    encoded_filter.sync_state_with_parent().unwrap();
                } else {
                    srcpad.set_target(Some(&ghostpad)).unwrap();
                }
            }
        } else {
            gst::debug!(CAT, imp: self, "Unused webrtcbin pad {pad:?}");
        }
    }

    fn prepare(&self) -> Result<(), Error> {
        let publisher_pc = gst::ElementFactory::make("webrtcbin")
            .name("publisher-pc")
            .property("bundle-policy", gst_webrtc::WebRTCBundlePolicy::MaxBundle)
            .build()
            .with_context(|| "Failed to make element webrtcbin".to_string())?;
        let subscriber_pc = gst::ElementFactory::make("webrtcbin")
            .name("subscriber-pc")
            .property("bundle-policy", gst_webrtc::WebRTCBundlePolicy::MaxBundle)
            .build()
            .with_context(|| "Failed to make element webrtcbin".to_string())?;

        {
            let settings = self.settings.lock().unwrap();

            if let Some(stun_server) = settings.stun_server.as_ref() {
                publisher_pc.set_property("stun-server", stun_server);
                subscriber_pc.set_property("stun-server", stun_server);
            }

            for turn_server in settings.turn_servers.iter() {
                publisher_pc.emit_by_name::<bool>("add-turn-server", &[&turn_server]);
                subscriber_pc.emit_by_name::<bool>("add-turn-server", &[&turn_server]);
            }
        }

        let bin = gst::Bin::new();
        bin.connect_pad_removed(glib::clone!(@weak self as this => move |_, pad|
            this.state.lock().unwrap().flow_combiner.remove_pad(pad);
        ));
        bin.connect_pad_added(glib::clone!(@weak self as this => move |_, pad|
            this.state.lock().unwrap().flow_combiner.add_pad(pad);
        ));

        subscriber_pc.connect_pad_added(
            glib::clone!(@weak self as this, @weak bin, => move |_bin, pad| {
                if pad.direction() == gst::PadDirection::Sink {
                    return;
                }
                this.handle_webrtc_src_pad(&bin, pad);
            }),
        );

        for webrtcbin in [&publisher_pc, &subscriber_pc] {
            webrtcbin.connect_closure(
                "on-ice-candidate",
                false,
                glib::closure!(@weak-allow-none self as this => move |
                    _webrtcbin: gst::Bin,
                    sdp_m_line_index: u32,
                    candidate: String| {
                        this.unwrap().on_ice_candidate(sdp_m_line_index, candidate);
                    }),
            );
            webrtcbin.connect_closure(
                "on-data-channel",
                false,
                glib::closure!(@weak-allow-none self as this => move |
                    _webrtcbin: gst::Bin,
                    data_channel: glib::Object| {
                        this.unwrap().on_data_channel(data_channel);
                    }),
            );
            bin.add(webrtcbin).unwrap();
        }
        self.signaller()
            .emit_by_name::<()>("consumer-added", &[&"none", &publisher_pc]);

        self.obj().add(&bin).context("Could not add `bin`")?;

        let mut state = self.state.lock().unwrap();
        state.publisher_pc.replace(publisher_pc);
        state.subscriber_pc.replace(subscriber_pc);

        Ok(())
    }

    fn get_stream_id(
        &self,
        transceiver: Option<gst_webrtc::WebRTCRTPTransceiver>,
        mline: Option<u32>,
    ) -> Option<String> {
        let mline = transceiver.map_or(mline, |t| Some(t.mlineindex()));

        // Same logic as gst_pad_create_stream_id and friends, making a hash of
        // the URI (session id, if URI doesn't exist) and adding `:<some-id>`, here the ID is the mline of the
        // stream in the SDP.
        mline.map(|mline| {
            let mut cs = glib::Checksum::new(glib::ChecksumType::Sha256).unwrap();

            let data: String = if self
                .signaller()
                .has_property("uri", Some(String::static_type()))
            {
                self.signaller().property::<Option<String>>("uri").unwrap()
            } else {
                // use the session id
                self.session_id().to_string()
            };

            cs.update(data.as_bytes());

            format!("{}:{mline}", cs.string().unwrap())
        })
    }

    fn unprepare(&self) -> Result<(), Error> {
        gst::info!(CAT, imp: self, "unpreparing");

        let obj = self.obj();
        self.maybe_stop_signaller();
        for pad in obj.src_pads() {
            obj.remove_pad(&pad)
                .map_err(|err| anyhow::anyhow!("Couldn't remove pad? {err:?}"))?;
        }

        self.n_video_pads.store(0, Ordering::SeqCst);
        self.n_audio_pads.store(0, Ordering::SeqCst);

        Ok(())
    }

    fn connect_signaller(&self, signaller: &Signallable) {
        let instance = &*self.obj();

        let _ = self
            .state
            .lock()
            .unwrap()
            .signaller_signals
            .insert(SignallerSignals {
            error: signaller.connect_closure(
                "error",
                false,
                glib::closure!(@watch instance => move |
                _signaller: glib::Object, error: String| {
                    gst::element_error!(
                        instance,
                        gst::StreamError::Failed,
                        ["Signalling error: {}", error]
                    );
                }),
            ),

            request_meta: signaller.connect_closure(
                "request-meta",
                false,
                glib::closure!(@watch instance => move |
                    _signaller: glib::Object| -> Option<gst::Structure> {
                    instance.imp().settings.lock().unwrap().meta.clone()
                }),
            ),

            session_description: signaller.connect_closure(
                "session-description",
                false,
                glib::closure!(@watch instance => move |
                        _signaller: glib::Object,
                        _peer_id: &str,
                        desc: &gst_webrtc::WebRTCSessionDescription| {
                    assert_eq!(desc.type_(), gst_webrtc::WebRTCSDPType::Offer);

                    instance.imp().handle_offer(desc);
                }),
            ),

            // sdp_mid is exposed for future proofing, see
            // https://gitlab.freedesktop.org/gstreamer/gst-plugins-bad/-/issues/1174,
            // at the moment sdp_m_line_index must be Some
            handle_ice: signaller.connect_closure(
                "handle-ice",
                false,
                glib::closure!(@watch instance => move |
                        _signaller: glib::Object,
                        peer_id: &str,
                        sdp_m_line_index: u32,
                        _sdp_mid: Option<String>,
                        candidate: &str| {
                    instance.imp().handle_ice(peer_id, Some(sdp_m_line_index), None, candidate);
                }),
            ),
        });

        // previous signals are disconnected when dropping the old structure
    }

    // Creates and adds our `WebRTCSrcPad` source pad, returning caps accepted
    // downstream
    fn create_and_probe_src_pad(&self, caps: &gst::Caps, stream_id: &str) -> bool {
        gst::log!(CAT, "Creating pad for {caps:?}, stream: {stream_id}");

        let obj = self.obj();
        let media_type = caps
            .structure(0)
            .expect("Passing empty caps is invalid")
            .get::<&str>("media")
            .expect("Only caps with a `media` field are expected when creating the pad");

        let (template, name, raw_caps) = if media_type == "video" {
            (
                obj.pad_template("video_%u").unwrap(),
                format!("video_{}", self.n_video_pads.fetch_add(1, Ordering::SeqCst)),
                VIDEO_CAPS.to_owned(),
            )
        } else if media_type == "audio" {
            (
                obj.pad_template("audio_%u").unwrap(),
                format!("audio_{}", self.n_audio_pads.fetch_add(1, Ordering::SeqCst)),
                AUDIO_CAPS.to_owned(),
            )
        } else {
            gst::info!(CAT, imp: self, "Not an audio or video media {media_type:?}");

            return false;
        };

        let caps_with_raw = [caps.clone(), raw_caps.clone()]
            .into_iter()
            .collect::<gst::Caps>();
        let ghost = gst::GhostPad::builder_from_template(&template)
            .name(name)
            .build()
            .downcast::<WebRTCSrcPad>()
            .unwrap();
        ghost.imp().set_stream_id(stream_id);
        obj.add_pad(&ghost)
            .expect("Adding ghost pad should never fail");

        let downstream_caps = ghost.peer_query_caps(Some(&caps_with_raw));
        if let Some(first_struct) = downstream_caps.structure(0) {
            if first_struct.has_name(raw_caps.structure(0).unwrap().name()) {
                ghost.imp().set_needs_decoding(true)
            }
        }

        true
    }

    fn all_codec_names(&self) -> HashSet<String> {
        let settings = self.settings.lock().unwrap();
        settings
            .video_codecs
            .iter()
            .chain(settings.audio_codecs.iter())
            .map(|codec| codec.name.clone())
            .collect::<HashSet<String>>()
    }

    fn handle_offer(&self, offer: &gst_webrtc::WebRTCSessionDescription) {
        let sdp = offer.sdp();
        gst::log!(CAT, imp: self, "Got offer {}", sdp.to_string());

        let subscriber_pc = self.subscriber_pc();
        let codec_names = Lazy::new(|| self.all_codec_names());
        for (i, media) in sdp.medias().enumerate() {
            let caps = media
                .formats()
                .filter_map(|format| format.parse::<i32>().ok())
                .filter_map(|pt| media.caps_from_media(pt))
                .filter_map(|mut mediacaps| {
                    let s = mediacaps.structure(0).unwrap();
                    if !codec_names.contains(s.get::<&str>("encoding-name").ok()?) {
                        return None;
                    }

                    let mut filtered_s = gst::Structure::new_empty("application/x-rtp");
                    filtered_s.extend(s.iter().filter_map(|(key, value)| {
                        if key.starts_with("rtcp-") {
                            None
                        } else {
                            Some((key, value.to_owned()))
                        }
                    }));

                    if media.attributes_to_caps(mediacaps.get_mut().unwrap()).is_err() {
                        gst::warning!(
                            CAT,
                            imp: self,
                            "Failed to retrieve attributes from media!"
                        );
                        return None;
                    }

                    let s = mediacaps.structure(0).unwrap();

                    filtered_s.extend(s.iter().filter_map(|(key, value)| {
                        if key.starts_with("extmap-") {
                            if let Ok(s) = value.get::<String>() {
                                if s == RTP_TWCC_URI {
                                    return Some((key, value.to_owned()));
                                }
                            }
                        }

                        None
                    }));

                    Some(filtered_s)
                })
                .collect::<gst::Caps>();

            if !caps.is_empty() {
                let stream_id = self.get_stream_id(None, Some(i as u32)).unwrap();
                if self.create_and_probe_src_pad(&caps, &stream_id) {
                    gst::info!(
                        CAT,
                        imp: self,
                        "Adding transceiver for {stream_id} with caps: {caps:#?}"
                    );
                    let transceiver = subscriber_pc.emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>(
                        "add-transceiver",
                        &[&gst_webrtc::WebRTCRTPTransceiverDirection::Recvonly, &caps],
                    );

                    transceiver.set_property("do_nack", true);
                    transceiver.set_property("fec-type", gst_webrtc::WebRTCFECType::UlpRed);
                }
            } else {
                gst::info!(
                    CAT,
                    "Not using media: {media:#?} as it doesn't match our codec restrictions"
                );
            }
        }

        subscriber_pc.emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

        let promise = gst::Promise::with_change_func(
            glib::clone!(@weak self as this => move |reply| {
                this.on_answer_created(reply);
            })
        );

        subscriber_pc.emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);
    }

    fn on_answer_created(&self, reply: Result<Option<&gst::StructureRef>, gst::PromiseError>) {
        let reply = match reply {
            Ok(Some(reply)) => {
                if !reply.has_field_with_type(
                    "answer",
                    gst_webrtc::WebRTCSessionDescription::static_type(),
                ) {
                    gst::element_error!(
                        self.obj(),
                        gst::StreamError::Failed,
                        ["create-answer::Promise returned with no reply"]
                    );
                    return;
                } else if reply.has_field_with_type("error", glib::Error::static_type()) {
                    gst::element_error!(
                        self.obj(),
                        gst::LibraryError::Failed,
                        ["create-offer::Promise returned with error: {:?}", reply]
                    );
                    return;
                }

                reply
            }
            Ok(None) => {
                gst::element_error!(
                    self.obj(),
                    gst::StreamError::Failed,
                    ["create-answer::Promise returned with no reply"]
                );

                return;
            }
            Err(err) => {
                gst::element_error!(
                    self.obj(),
                    gst::LibraryError::Failed,
                    ["create-answer::Promise returned with error {:?}", err]
                );

                return;
            }
        };

        let answer = reply
            .value("answer")
            .unwrap()
            .get::<gst_webrtc::WebRTCSessionDescription>()
            .expect("Invalid argument");

        self.subscriber_pc()
            .emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);

        gst::log!(CAT, imp: self, "Sending SDP, {}", answer.sdp().to_string());
        let signaller = self.signaller();
        signaller.send_sdp(self.session_id(), &answer);
    }

    fn on_data_channel(&self, data_channel: glib::Object) {
        gst::log!(CAT, imp: self, "Received data channel {data_channel:?}");
        let mut state = self.state.lock().unwrap();
        state.data_channel = data_channel.dynamic_cast::<WebRTCDataChannel>().ok();
    }

    fn on_ice_candidate(&self, sdp_m_line_index: u32, candidate: String) {
        let signaller = self.signaller();
        signaller.add_ice(self.session_id(), &candidate, sdp_m_line_index, None::<String>);
    }

    /// Called by the signaller with an ice candidate
    fn handle_ice(
        &self,
        peer_id: &str,
        sdp_m_line_index: Option<u32>,
        _sdp_mid: Option<String>,
        candidate: &str,
    ) {
        let sdp_m_line_index = match sdp_m_line_index {
            Some(m_line) => m_line,
            None => {
                gst::error!(CAT, imp: self, "No mandatory mline");
                return;
            }
        };
        gst::log!(CAT, imp: self, "Got ice from {peer_id}: {candidate}");

        self.publisher_pc()
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_m_line_index, &candidate]);
    }

    fn maybe_start_signaller(&self) {
        let obj = self.obj();
        let mut state = self.state.lock().unwrap();
        if state.signaller_state == SignallerState::Stopped
            && obj.current_state() >= gst::State::Paused
        {
            self.signaller().start();

            gst::info!(CAT, imp: self, "Started signaller");
            state.signaller_state = SignallerState::Started;
        }
    }

    fn maybe_stop_signaller(&self) {
        let mut state = self.state.lock().unwrap();
        if state.signaller_state == SignallerState::Started {
            self.signaller().stop();
            state.signaller_state = SignallerState::Stopped;
            gst::info!(CAT, imp: self, "Stopped signaller");
        }
    }

    pub fn set_signaller(&self, signaller: Signallable) -> Result<(), Error> {
        let sigobj = signaller.clone();
        let mut settings = self.settings.lock().unwrap();

        self.connect_signaller(&sigobj);
        settings.signaller = signaller;

        Ok(())
    }
}

impl ElementImpl for LiveKitWebRTCSrc {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "LiveKitWebRTCSrc",
                "Source/Network/WebRTC",
                "WebRTC source with LiveKit signaller",
                "Jordan Yelloz <jordan.yelloz@collabora.com>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }

    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            let mut video_caps_builder = gst::Caps::builder_full()
                .structure_with_any_features(VIDEO_CAPS.structure(0).unwrap().to_owned())
                .structure(RTP_CAPS.structure(0).unwrap().to_owned());

            for codec in Codecs::video_codecs() {
                video_caps_builder =
                    video_caps_builder.structure(codec.caps.structure(0).unwrap().to_owned());
            }

            let mut audio_caps_builder = gst::Caps::builder_full()
                .structure_with_any_features(AUDIO_CAPS.structure(0).unwrap().to_owned())
                .structure(RTP_CAPS.structure(0).unwrap().to_owned());

            for codec in Codecs::audio_codecs() {
                audio_caps_builder =
                    audio_caps_builder.structure(codec.caps.structure(0).unwrap().to_owned());
            }

            vec![
                gst::PadTemplate::with_gtype(
                    "video_%u",
                    gst::PadDirection::Src,
                    gst::PadPresence::Sometimes,
                    &video_caps_builder.build(),
                    WebRTCSrcPad::static_type(),
                )
                .unwrap(),
                gst::PadTemplate::with_gtype(
                    "audio_%u",
                    gst::PadDirection::Src,
                    gst::PadPresence::Sometimes,
                    &audio_caps_builder.build(),
                    WebRTCSrcPad::static_type(),
                )
                .unwrap(),
            ]
        });

        PAD_TEMPLATES.as_ref()
    }

    fn change_state(
        &self,
        transition: gst::StateChange,
    ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
        let obj = &*self.obj();
        if let gst::StateChange::NullToReady = transition {
            if let Err(err) = self.prepare() {
                gst::element_error!(
                    obj,
                    gst::StreamError::Failed,
                    ["Failed to prepare: {}", err]
                );
                return Err(gst::StateChangeError);
            }
        }

        let mut ret = self.parent_change_state(transition);

        match transition {
            gst::StateChange::PausedToReady => {
                if let Err(err) = self.unprepare() {
                    gst::element_error!(
                        obj,
                        gst::StreamError::Failed,
                        ["Failed to unprepare: {}", err]
                    );
                    return Err(gst::StateChangeError);
                }
            }
            gst::StateChange::ReadyToPaused => {
                ret = Ok(gst::StateChangeSuccess::NoPreroll);
            }
            gst::StateChange::PlayingToPaused => {
                ret = Ok(gst::StateChangeSuccess::NoPreroll);
            }
            gst::StateChange::PausedToPlaying => {
                self.maybe_start_signaller();
            }
            _ => (),
        }

        ret
    }
}

impl ChildProxyImpl for LiveKitWebRTCSrc {
    fn child_by_index(&self, index: u32) -> Option<glib::Object> {
        if index == 0 {
            Some(self.signaller().upcast())
        } else {
            None
        }
    }

    fn children_count(&self) -> u32 {
        1
    }

    fn child_by_name(&self, name: &str) -> Option<glib::Object> {
        match name {
            "signaller" => {
                gst::info!(CAT, imp: self, "Getting signaller");
                Some(self.signaller().upcast())
            }
            _ => None,
        }
    }
}

struct State {
    signaller_state: SignallerState,
    publisher_pc: Option<gst::Element>,
    subscriber_pc: Option<gst::Element>,
    flow_combiner: gst_base::UniqueFlowCombiner,
    signaller_signals: Option<SignallerSignals>,
    data_channel: Option<WebRTCDataChannel>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            signaller_state: SignallerState::Stopped,
            publisher_pc: None,
            subscriber_pc: None,
            flow_combiner: Default::default(),
            signaller_signals: Default::default(),
            data_channel: None,
        }
    }
}

impl GstObjectImpl for LiveKitWebRTCSrc {}
impl BinImpl for LiveKitWebRTCSrc {}

#[derive(PartialEq)]
enum SignallerState {
    Started,
    Stopped,
}
