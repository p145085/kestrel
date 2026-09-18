//! Check that a video sink can be built and its paintable taken.
//!
//! The compiler can prove the types line up; it cannot prove the plugin is
//! installed, that the property exists, or that the two halves of the stack
//! agree about what a `GdkPaintable` is at runtime. Those only fail when a
//! call starts, which is the worst moment to find out.
//!
//!     cargo run -p kestrel-ui --example check_video_sink

use gtk::gdk;
use gtk::prelude::*;
use kestrel_media::gstreamer;
use kestrel_media::gstreamer::prelude::{ElementExt, GstObjectExt};

fn main() {
    if let Err(error) = gtk::init() {
        println!("FAIL: GTK would not start: {error}");
        std::process::exit(1);
    }
    if let Err(error) = kestrel_media::init() {
        println!("FAIL: GStreamer would not start: {error}");
        std::process::exit(1);
    }

    let sink = match gstreamer::ElementFactory::make("gtk4paintablesink").build() {
        Ok(sink) => sink,
        Err(error) => {
            println!("FAIL: no gtk4paintablesink: {error}");
            std::process::exit(1);
        }
    };

    let paintable: gdk::Paintable = sink.property("paintable");
    let picture = gtk::Picture::builder().paintable(&paintable).build();

    // Reading it back proves the paintable survived the round trip into a
    // widget rather than merely having the right type on the way in.
    if picture.paintable().is_none() {
        println!("FAIL: the picture did not keep the paintable");
        std::process::exit(1);
    }

    println!(
        "video sink works: {}",
        sink.factory().map_or_else(
            || "unnamed".to_owned(),
            |factory| factory.name().to_string()
        )
    );
}
