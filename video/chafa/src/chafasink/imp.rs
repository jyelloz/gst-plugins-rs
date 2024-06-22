// Copyright (C) 2024 Jordan Yelloz <jordan@yelloz.me>
// SPDX-License-Identifier: MPL-2.0

use std::io::{self, prelude::*};
use std::sync::Mutex;

use gst::{glib, subclass::prelude::*};
use gst_base::subclass::prelude::*;
use gst_video::{prelude::*, subclass::prelude::*, VideoFrameRef};
use once_cell::sync::Lazy;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        super::NAME,
        gst::DebugColorFlags::empty(),
        Some(super::DESCRIPTION),
    )
});

#[derive(Default)]
pub struct ChafaSink {
    info: Mutex<Option<gst_video::VideoInfo>>,
}

#[glib::object_subclass]
impl ObjectSubclass for ChafaSink {
    const NAME: &'static str = "GstChafaSink";
    type Type = super::ChafaSink;
    type ParentType = gst_video::VideoSink;
}

impl ObjectImpl for ChafaSink {}
impl GstObjectImpl for ChafaSink {}

impl ElementImpl for ChafaSink {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                super::DESCRIPTION,
                "Sink/Video",
                "Renders incoming video to the terminal using the Chafa library",
                "Jordan Yelloz <jordan@yelloz.me>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }
    fn pad_templates() -> &'static [gst::PadTemplate] {
        static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
            let fps_lo = gst::Fraction::from_integer(1);
            let fps_hi = gst::Fraction::from_integer(60);
            let caps = gst_video::VideoCapsBuilder::new()
                .format(gst_video::VideoFormat::Rgba)
                .framerate_range(fps_lo..fps_hi)
                .build();
            let template = gst::PadTemplate::new(
                "sink",
                gst::PadDirection::Sink,
                gst::PadPresence::Always,
                &caps,
            )
            .unwrap();
            vec![template]
        });
        PAD_TEMPLATES.as_ref()
    }
}

impl BaseSinkImpl for ChafaSink {
    fn start(&self) -> Result<(), gst::ErrorMessage> {
        gst::log!(CAT, imp = self, "starting");
        self.parent_start()?;
        self.setup_terminal()?;
        Ok(())
    }
    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        gst::log!(CAT, imp = self, "stopping");
        self.teardown_terminal()?;
        self.parent_stop()?;
        Ok(())
    }
    //FIXME: This could be simplified if GstVideoSink::set_info was available
    fn set_caps(&self, caps: &gst::Caps) -> Result<(), gst::LoggableError> {
        let video_info = gst_video::VideoInfo::from_caps(caps)?;
        gst::log!(CAT, imp = self, "video info {video_info:?}");
        self.info.lock().unwrap().replace(video_info);
        self.parent_set_caps(caps)?;
        Ok(())
    }
}

impl VideoSinkImpl for ChafaSink {
    fn show_frame(&self, buffer: &gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
        let info = self.info.lock().unwrap();
        let Some(info) = info.as_ref() else {
            return Err(gst::FlowError::NotNegotiated);
        };
        let Ok(frame) = VideoFrameRef::from_buffer_ref_readable(buffer, info) else {
            return Err(gst::FlowError::Error);
        };
        self.write_to_terminal(&frame)
            .map_err(|_| gst::FlowError::Error)?;
        Ok(gst::FlowSuccess::Ok)
    }
}

impl ChafaSink {
    // TODO: customize output stream
    fn stream(&self) -> impl io::Write {
        io::stderr()
    }
    fn setup_terminal(&self) -> Result<(), gst::ErrorMessage> {
        let mut out = self.stream();
        anes::execute!(out, anes::SwitchBufferToAlternate, anes::HideCursor).map_err(|_| {
            gst::error_msg!(gst::CoreError::StateChange, ["Failed to set up terminal"])
        })?;
        Ok(())
    }
    fn teardown_terminal(&self) -> Result<(), gst::ErrorMessage> {
        let mut out = self.stream();
        anes::execute!(out, anes::ShowCursor, anes::SwitchBufferToNormal).map_err(|_| {
            gst::error_msg!(gst::CoreError::StateChange, ["Failed to teardown terminal"])
        })?;
        Ok(())
    }
    fn write_to_terminal(&self, frame: &VideoFrameRef<&gst::BufferRef>) -> io::Result<()> {
        let width = frame.width();
        let height = frame.height();
        let canvas = chafa::ChafaCanvas::from_term(width, height);
        let plane = frame.plane_data(0).expect("RGBA frame must have plane 0");
        let text = canvas.draw(plane, width, height);
        let mut out = self.stream();
        anes::execute!(out, anes::SaveCursorPosition, anes::MoveCursorTo(0, 0))?;
        write!(out, "{}", text)?;
        anes::execute!(out, anes::RestoreCursorPosition)?;
        Ok(())
    }
}
