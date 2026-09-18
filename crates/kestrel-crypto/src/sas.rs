//! Short authentication strings.
//!
//! The server relays signalling, so a hostile one could substitute DTLS
//! fingerprints and sit in the middle of a call. Nothing in the protocol can
//! detect that, because the attacker controls everything the protocol sees.
//!
//! What it cannot control is the two people talking. Both ends derive a short
//! phrase from the key agreement and read it aloud; an attacker in the middle
//! has agreed a *different* secret with each of them and cannot make both
//! phrases match. This is the only defence here that survives a malicious
//! server, and it costs one sentence spoken out loud.

use hkdf::Hkdf;
use sha2::Sha256;

use crate::identity::IdentityKey;
use crate::seal::SharedSecret;

/// How many words a short authentication string has.
///
/// Four words from a 256-word list is 32 bits: an attacker would have to
/// re-run the key agreement about four billion times to land a matching
/// phrase, while both people wait.
pub const SAS_WORDS: usize = 4;

/// A short authentication string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sas {
    words: [&'static str; SAS_WORDS],
}

impl Sas {
    /// The words, in order.
    #[must_use]
    pub fn words(&self) -> &[&'static str; SAS_WORDS] {
        &self.words
    }

    /// The phrase as a user should see and say it.
    #[must_use]
    pub fn phrase(&self) -> String {
        self.words.join(" ")
    }

    /// Whether a phrase the user typed matches.
    ///
    /// Compares words rather than raw text, so spacing and capitalisation do
    /// not make a correct answer look wrong.
    #[must_use]
    pub fn matches(&self, spoken: &str) -> bool {
        let spoken: Vec<String> = spoken.split_whitespace().map(str::to_lowercase).collect();
        spoken.len() == SAS_WORDS
            && spoken
                .iter()
                .zip(self.words.iter())
                .all(|(said, expected)| said == expected)
    }
}

impl std::fmt::Display for Sas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.phrase())
    }
}

/// Derive the phrase both sides of a call should see.
///
/// `participants` are the identity keys of everyone in the call. They are
/// sorted here, so every participant derives the same phrase regardless of who
/// called whom — a phrase that depended on ordering would differ between the
/// two ends and be useless for comparing.
#[must_use]
pub fn derive(shared: &SharedSecret, call_id: &[u8], participants: &[IdentityKey]) -> Sas {
    let mut sorted: Vec<&IdentityKey> = participants.iter().collect();
    sorted.sort_unstable();

    let mut context = Vec::with_capacity(call_id.len() + sorted.len() * 32);
    context.extend_from_slice(call_id);
    for key in sorted {
        context.extend_from_slice(key.as_bytes());
    }

    let hkdf = Hkdf::<Sha256>::new(Some(&context), shared.as_bytes());
    let mut output = [0u8; SAS_WORDS];
    let _ = hkdf.expand(b"kestrel/sas/v0", &mut output);

    Sas {
        words: std::array::from_fn(|i| WORDS[output[i] as usize]),
    }
}

/// 256 short words, one per possible byte.
///
/// Chosen to be easy to say over a bad connection and hard to mishear for one
/// another: no two differ only by a voiced consonant, and none are homophones.
const WORDS: [&str; 256] = [
    "acid", "acorn", "actor", "adapt", "afraid", "agent", "airway", "album", "alert", "alien",
    "almond", "alpha", "amber", "amuse", "anchor", "angle", "ankle", "apple", "april", "arcade",
    "arctic", "argue", "armor", "arrow", "artist", "aspect", "atlas", "atom", "attic", "author",
    "autumn", "avenue", "bacon", "badge", "bagel", "baker", "bamboo", "banjo", "barley", "basin",
    "basket", "batch", "beacon", "beagle", "beast", "beaver", "bishop", "bitter", "black", "blade",
    "blanket", "blast", "blend", "blink", "block", "blossom", "blue", "board", "bobcat", "bonus",
    "border", "bottle", "boulder", "bounce", "brain", "branch", "brave", "bread", "brick",
    "bridge", "bright", "bronze", "brown", "brush", "bubble", "bucket", "buffalo", "bundle",
    "bunker", "burden", "butter", "button", "cabin", "cactus", "camel", "camera", "campus",
    "canal", "candle", "canvas", "canyon", "carbon", "cargo", "carpet", "castle", "cattle",
    "cavern", "cedar", "cement", "census", "chalk", "chamber", "chapel", "charm", "cheese",
    "cherry", "chess", "chimney", "chorus", "cinema", "circus", "citrus", "clamp", "clarity",
    "classic", "clever", "cliff", "climate", "cloak", "clock", "closet", "cloud", "clover",
    "cluster", "cobalt", "cobra", "cocoa", "coffee", "collar", "colony", "column", "combat",
    "comet", "comfort", "compass", "concert", "condor", "copper", "coral", "cottage", "cotton",
    "county", "cousin", "coyote", "cradle", "crater", "crayon", "credit", "cricket", "crimson",
    "crystal", "cuckoo", "cupboard", "curfew", "current", "curtain", "custom", "cymbal", "dagger",
    "dairy", "dancer", "danger", "dapple", "daring", "dawn", "daylight", "decade", "decoy",
    "deluxe", "denim", "dentist", "depart", "desert", "design", "diamond", "diesel", "digit",
    "dinner", "dolphin", "domain", "donkey", "dossier", "double", "dragon", "drama", "drawer",
    "dream", "driver", "drizzle", "dublin", "duchess", "dugout", "eagle", "earlobe", "eastern",
    "echo", "eclipse", "edible", "effort", "eggshell", "eight", "elbow", "elder", "electric",
    "elegant", "element", "elephant", "eleven", "email", "ember", "emblem", "embrace", "emerald",
    "empire", "enamel", "encore", "endless", "engine", "enlist", "enrich", "ensign", "entire",
    "entry", "envelope", "equal", "equator", "eraser", "escape", "essay", "estate", "ethics",
    "evening", "exact", "exhale", "exhibit", "exile", "exodus", "expand", "expert", "extra",
    "fabric", "facade", "falcon", "family", "famous", "fancy", "fantasy", "farmer", "fashion",
    "fathom", "feather", "fedora", "female", "fennel", "ferry", "fiber",
];

#[cfg(test)]
mod tests {
    use super::{SAS_WORDS, WORDS, derive};
    use crate::identity::Identity;
    use crate::seal::EphemeralKey;

    /// A shared secret plus the two identity keys that belong to it.
    fn call() -> (
        crate::seal::SharedSecret,
        crate::seal::SharedSecret,
        Vec<crate::identity::IdentityKey>,
    ) {
        let alice_eph = EphemeralKey::generate();
        let bob_eph = EphemeralKey::generate();
        let keys = vec![Identity::generate().public(), Identity::generate().public()];
        (
            alice_eph.agree(&bob_eph.public()),
            bob_eph.agree(&alice_eph.public()),
            keys,
        )
    }

    #[test]
    fn the_wordlist_is_the_right_size_and_has_no_repeats() {
        assert_eq!(WORDS.len(), 256, "one word per possible byte");
        let mut sorted = WORDS;
        sorted.sort_unstable();
        let mut deduped = sorted.to_vec();
        deduped.dedup();
        assert_eq!(deduped.len(), 256, "a repeated word halves the entropy");
    }

    #[test]
    fn words_are_lowercase_and_short_enough_to_read_aloud() {
        for word in WORDS {
            assert!(!word.is_empty() && word.len() <= 9, "{word} is unwieldy");
            assert!(
                word.chars().all(|c| c.is_ascii_lowercase()),
                "{word} should be plain lowercase"
            );
        }
    }

    #[test]
    fn both_sides_of_a_call_see_the_same_phrase() {
        // If they differed, comparing them would prove nothing.
        let (alice, bob, keys) = call();
        assert_eq!(derive(&alice, b"c1", &keys), derive(&bob, b"c1", &keys));
    }

    #[test]
    fn the_phrase_does_not_depend_on_who_called_whom() {
        let (alice, bob, keys) = call();
        let reversed: Vec<_> = keys.iter().rev().copied().collect();
        assert_eq!(derive(&alice, b"c1", &keys), derive(&bob, b"c1", &reversed));
    }

    #[test]
    fn a_different_call_gives_a_different_phrase() {
        // Otherwise a phrase confirmed once could be replayed into a later
        // call the attacker controls.
        let (alice, _bob, keys) = call();
        assert_ne!(derive(&alice, b"c1", &keys), derive(&alice, b"c2", &keys));
    }

    #[test]
    fn a_man_in_the_middle_sees_two_different_phrases() {
        // Mallory agrees a separate secret with each side, so she cannot make
        // both ends read the same words.
        let alice = EphemeralKey::generate();
        let bob = EphemeralKey::generate();
        let mallory = EphemeralKey::generate();
        let keys = vec![Identity::generate().public(), Identity::generate().public()];

        let alice_to_mallory = alice.agree(&mallory.public());
        let mallory_to_bob = mallory.agree(&bob.public());

        assert_ne!(
            derive(&alice_to_mallory, b"c1", &keys),
            derive(&mallory_to_bob, b"c1", &keys),
            "an attacker in the middle must not be able to match both phrases"
        );
    }

    #[test]
    fn different_participants_give_a_different_phrase() {
        let (alice, _bob, keys) = call();
        let mut intruder = keys.clone();
        intruder.push(Identity::generate().public());
        assert_ne!(
            derive(&alice, b"c1", &keys),
            derive(&alice, b"c1", &intruder)
        );
    }

    #[test]
    fn a_phrase_has_the_expected_shape() {
        let (alice, _bob, keys) = call();
        let sas = derive(&alice, b"c1", &keys);
        assert_eq!(sas.words().len(), SAS_WORDS);
        assert_eq!(sas.phrase().split_whitespace().count(), SAS_WORDS);
    }

    #[test]
    fn matching_forgives_spacing_and_capitals_but_not_wrong_words() {
        let (alice, _bob, keys) = call();
        let sas = derive(&alice, b"c1", &keys);
        let phrase = sas.phrase();

        assert!(sas.matches(&phrase));
        assert!(sas.matches(&phrase.to_uppercase()));
        assert!(sas.matches(&format!("  {phrase}  ")));

        assert!(!sas.matches(""), "an empty answer is not a match");
        assert!(!sas.matches(&format!("{phrase} extra")));
        let mut wrong: Vec<&str> = phrase.split(' ').collect();
        wrong[0] = if wrong[0] == "acid" { "acorn" } else { "acid" };
        assert!(!sas.matches(&wrong.join(" ")));
    }
}
