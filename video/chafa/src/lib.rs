// SPDX-License-Identifier: MPL-2.0
#![allow(unused_doc_comments)]

/**
 * plugin-chafa:
 * @title: Chafa Video Sink
 *
 */
use gst::glib;

mod chafasink;

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    chafasink::register(plugin)?;
    Ok(())
}

gst::plugin_define!(
    chafa,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    "MPL-2.0",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
