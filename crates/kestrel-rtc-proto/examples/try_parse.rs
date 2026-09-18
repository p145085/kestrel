//! Try to reduce an SDP file to the compact form, and say what is missing.

fn main() {
    let path = std::env::args().nth(1).expect("usage: try_parse <file>");
    let text = std::fs::read_to_string(&path).expect("should read");

    for (label, prefix) in [
        ("fingerprint", "a=fingerprint:sha-256 "),
        ("ice-ufrag", "a=ice-ufrag:"),
        ("ice-pwd", "a=ice-pwd:"),
        ("setup", "a=setup:"),
    ] {
        let found = text.lines().any(|l| l.trim().starts_with(prefix));
        println!("{label:12} {}", if found { "present" } else { "MISSING" });
    }

    match kestrel_rtc_proto::sdp::from_sdp(&text) {
        Some(compact) => println!("\nreduced ok: profile={:?}", compact.profile),
        None => println!("\nCOULD NOT REDUCE"),
    }
}
