//! Choosing a capture device.
//!
//! Kept apart from the window because reading a choice back out of a dropdown
//! is exactly the kind of thing that looks obviously right and silently is
//! not: an off-by-one or a failed downcast both produce "the system default",
//! which is indistinguishable from the setting having been ignored.

use gtk::prelude::*;

/// What the first entry says, meaning "whatever the system would pick".
pub const DEFAULT: &str = "(system default)";

/// A dropdown over a list of device names, with the default first.
#[must_use]
pub fn chooser(names: &[String]) -> gtk::DropDown {
    let mut entries: Vec<&str> = vec![DEFAULT];
    entries.extend(names.iter().map(String::as_str));
    let chooser = gtk::DropDown::from_strings(&entries);
    chooser.set_hexpand(true);
    chooser
}

/// Point a chooser at a name it offers, if it offers it.
///
/// Used to show what is already chosen when the dialog opens again, so a
/// setting that was made once does not read as never having been made.
pub fn select(chooser: &gtk::DropDown, wanted: Option<&str>) {
    let Some(wanted) = wanted else {
        chooser.set_selected(0);
        return;
    };
    let Some(list) = chooser.model().and_downcast::<gtk::StringList>() else {
        return;
    };
    for index in 0..list.n_items() {
        if list.string(index).is_some_and(|name| name == wanted) {
            chooser.set_selected(index);
            return;
        }
    }
}

/// What a chooser is pointing at, or `None` for the system default.
#[must_use]
pub fn chosen(chooser: &gtk::DropDown) -> Option<String> {
    let selected = chooser.selected();
    if selected == 0 {
        return None;
    }
    chooser
        .model()
        .and_downcast::<gtk::StringList>()
        .and_then(|list| list.string(selected))
        .map(|name| name.to_string())
        .filter(|name| name != DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, deliberately.
    ///
    /// GTK is not thread-safe and the test harness runs tests in parallel, so
    /// a second one touching widgets on another thread corrupts the type
    /// system rather than failing cleanly. Everything here shares one
    /// initialisation on one thread.
    ///
    /// Building widgets needs a display; without one -- a build machine, say
    /// -- there is nothing to test rather than something to fail.
    #[test]
    fn a_chooser_reads_back_what_was_chosen() {
        if gtk::init().is_err() {
            return;
        }

        let names: Vec<String> = ["USB Camera", "Emil's S22 (Windows Virtual Camera)", "OBS"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        let chooser = chooser(&names);

        for (index, name) in names.iter().enumerate() {
            // The list is offset by one, because the default sits in front.
            let at = u32::try_from(index).expect("a small index") + 1;
            chooser.set_selected(at);
            assert_eq!(
                chosen(&chooser).as_ref(),
                Some(name),
                "choosing {name} read back as something else"
            );
        }

        chooser.set_selected(0);
        assert_eq!(chosen(&chooser), None, "the first entry is the default");

        select(&chooser, Some("OBS"));
        assert_eq!(
            chosen(&chooser).as_deref(),
            Some("OBS"),
            "a choice has to survive being shown again"
        );

        // A camera unplugged since it was chosen should not silently become
        // whichever device happens to sit at that index now.
        select(&chooser, Some("a camera that went away"));
        assert_eq!(chosen(&chooser).as_deref(), Some("OBS"));

        select(&chooser, None);
        assert_eq!(chosen(&chooser), None);
    }
}
