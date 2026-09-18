//! Making a crash say something.
//!
//! A graphical build has no console, so a panic prints to nowhere and the
//! window simply disappears. That is the worst possible way to learn about a
//! bug: there is nothing to report, nothing to search for, and no way to tell
//! a crash apart from somebody closing the window.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

/// Start writing panics to a file, in addition to wherever they already go.
///
/// Appends rather than replaces: the interesting crash is often not the last
/// one, and a report that overwrote its own evidence is no report at all.
pub fn write_panics_to_a_file() {
    let Some(path) = crash_log() else {
        return;
    };

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The default hook first, so a build with a console still prints.
        previous(info);

        let mut report = String::new();
        report.push_str("\n=== Kestrel panicked ===\n");
        if let Some(location) = info.location() {
            let _ = writeln!(report, "at {location}");
        }
        let _ = writeln!(report, "{}", payload_of(info));
        let _ = writeln!(report, "{}", std::backtrace::Backtrace::force_capture());

        if let Some(directory) = path.parent() {
            let _ = std::fs::create_dir_all(directory);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = file.write_all(report.as_bytes());
        }
    }));
}

/// Where a crash report is written.
#[must_use]
pub fn crash_log() -> Option<PathBuf> {
    crate::store::Store::in_config_directory()
        .and_then(|store| store.path().parent().map(|d| d.join("crash.log")))
}

fn payload_of(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic with no message".to_owned()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_crash_log_sits_beside_the_identity() {
        // Kept together so "send me everything in that folder" collects both.
        let Some(log) = super::crash_log() else {
            return;
        };
        let Some(store) = crate::store::Store::in_config_directory() else {
            return;
        };
        assert_eq!(log.parent(), store.path().parent());
    }
}
