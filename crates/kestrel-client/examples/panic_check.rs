//! Check that a panic leaves a report behind.
//!
//! The handler only matters at the moment everything else has gone wrong, so
//! whether it works cannot be left to inspection.

fn main() {
    kestrel_client::crash::write_panics_to_a_file();
    let Some(log) = kestrel_client::crash::crash_log() else {
        println!("FAIL: nowhere to write a crash report");
        std::process::exit(1);
    };
    println!("reports go to {}", log.display());
    panic!("a deliberate panic, to check the report is written");
}
