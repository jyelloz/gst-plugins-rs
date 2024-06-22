// SPDX-License-Identifier: MPL-2.0

use std::io::prelude::*;
use std::sync::Mutex;

use gst::glib;
use gst::subclass::prelude::*;
use gst_base::subclass::prelude::*;
use gst_video::prelude::*;
use gst_video::subclass::prelude::*;
use gst_video::VideoFrameRef;
use once_cell::sync::Lazy;

static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "chafasink",
        gst::DebugColorFlags::empty(),
        Some("Chafa terminal graphics sink"),
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
    type Interfaces = ();
}

impl ObjectImpl for ChafaSink {}
impl GstObjectImpl for ChafaSink {}
impl ElementImpl for ChafaSink {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
            gst::subclass::ElementMetadata::new(
                "Chafa Sink",
                "Sink/Video",
                "A Chafa terminal graphics sink",
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
        self.parent_start()?;
        self.setup_terminal()?;
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        self.teardown_terminal()?;
        self.parent_stop()?;
        Ok(())
    }

    fn set_caps(&self, caps: &gst::Caps) -> Result<(), gst::LoggableError> {
        gst::error!(CAT, imp: self, "set caps {caps:?}");
        let video_info = gst_video::VideoInfo::from_caps(caps)?;
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
    fn setup_terminal(&self) -> Result<(), gst::ErrorMessage> {
        let mut stderr = std::io::stderr();
        anes::execute!(stderr, anes::SwitchBufferToAlternate, anes::HideCursor,).map_err(|_| {
            gst::error_msg!(gst::CoreError::StateChange, ["Failed to set up terminal"])
        })?;
        Ok(())
    }
    fn teardown_terminal(&self) -> Result<(), gst::ErrorMessage> {
        let mut stderr = std::io::stderr();
        anes::execute!(stderr, anes::ShowCursor, anes::SwitchBufferToNormal,).map_err(|_| {
            gst::error_msg!(gst::CoreError::StateChange, ["Failed to restore terminal"])
        })?;
        Ok(())
    }

    fn write_to_terminal(&self, frame: &VideoFrameRef<&gst::BufferRef>) -> std::io::Result<()> {
        let width = frame.width();
        let height = frame.height();
        let canvas = chafa::ChafaCanvas::from_term(width, height);
        let component = frame
            .comp_data(0)
            .expect("RGBA image must have component 0");
        let ansi = canvas.draw(component, width, height);
        let mut stderr = std::io::stderr();
        anes::execute!(stderr, anes::SaveCursorPosition, anes::MoveCursorTo(0, 0),)?;
        write!(stderr, "{}", ansi)?;
        anes::execute!(stderr, anes::RestoreCursorPosition)?;
        Ok(())
    }
}
