// SPDX-License-Identifier: MPL-2.0
#![allow(unused_doc_comments)]

/**
 * plugin-rsaudiovisualizers:
 * @title: Rust Audio Visualizers
 *
 */
use gst::glib;

mod audiospectrogram;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    audiospectrogram::register(plugin)?;
    Ok(())
}

gst::plugin_define!(
    rsaudiovisualizers,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    "MPL-2.0",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
