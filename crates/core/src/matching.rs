//! Matching a playback title against the library.
//!
//! [`propose`] runs the filename parser over a raw player title and decides
//! which library entry, if any, it names. The decision is a
//! [`ProposedMatch`]: the shell shows it and the store keeps the latest one.
//! A title the person has confirmed once is remembered in [`Aliases`] and
//! settles the decision outright, ahead of every other gate.

use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::time::SystemTime;

use ryuuji_parse::{Options, parse};

use crate::tagged::tagged_enum;
use crate::{EntryId, LibraryEntry, PlaybackEvent};

tagged_enum! {
    /// How sure the matcher is that a proposal names a library entry,
    /// strongest first. Tags are stored in the database.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Confidence {
        Exact => "exact", "Exact match",
        Likely => "likely", "Likely match",
        Unmatched => "unmatched", "No library entry",
    }
}

/// One playback title's decision against the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedMatch {
    pub raw_title: String,
    /// Empty when the parser found no title.
    pub parsed_title: String,
    /// Every episode the file carries; a batch spans more than one.
    pub episode: Option<RangeInclusive<u32>>,
    pub season: Option<u32>,
    pub release_group: Option<String>,
    pub link: Link,
    pub player: String,
    pub at: SystemTime,
}

impl ProposedMatch {
    /// What to call this proposal without a library to consult: the parsed
    /// title, else the raw player title.
    pub fn shown_title(&self) -> &str {
        if self.parsed_title.is_empty() {
            &self.raw_title
        } else {
            &self.parsed_title
        }
    }

    /// The matched entry's title, else [`ProposedMatch::shown_title`].
    pub fn title_in<'a>(&'a self, library: &'a [LibraryEntry]) -> &'a str {
        self.link
            .entry()
            .and_then(|id| library.iter().find(|entry| entry.id == id))
            .map_or_else(|| self.shown_title(), |entry| entry.title.as_str())
    }
}

/// Whether the name is for something beside an episode: a preview, a
/// trailer, an opening. Read off the raw title rather than carried on the
/// proposal, because it is a fact about the name and not about which show
/// the name turned out to match, and Now playing already re-reads the raw
/// title for the same reason.
pub fn is_extra(raw_title: &str) -> bool {
    parse(raw_title, &Options::default()).extra().is_some()
}

/// Folds a title to the loose form matching compares: lowercase
/// alphanumeric tokens, `&` spelled out, a leading "the" dropped, roman
/// numerals and season phrasings reduced to the bare number.
///
/// Every letter counts, not only the ASCII ones. A title written in kana or
/// han folded to nothing while the punctuation around it folded to spaces,
/// and an empty fold is how a title reaches [`Resolution::Unmatched`] before
/// the aliases are even read, so no such title could ever be remembered.
pub fn normalize_title(title: &str) -> String {
    let cleaned: String = title
        .replace('&', " and ")
        .chars()
        .flat_map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().collect::<Vec<char>>()
            } else {
                vec![' ']
            }
        })
        .collect();
    let mut tokens: Vec<&str> = cleaned.split_whitespace().map(rewrite_roman).collect();
    if tokens.first() == Some(&"the") {
        tokens.remove(0);
    }
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let next = tokens.get(i + 1).copied();
        if token == "season"
            && let Some(n) = next.and_then(|t| t.parse::<u32>().ok())
        {
            out.push(n.to_string());
            i += 2;
        } else if let Some(n) = ordinal(token)
            && next == Some("season")
        {
            out.push(n.to_string());
            i += 2;
        } else if token == "s"
            && let Some(n) = next.and_then(|t| t.parse::<u32>().ok())
        {
            out.push(n.to_string());
            i += 2;
        } else if let Some(n) = season_shorthand(token) {
            out.push(n.to_string());
            i += 1;
        } else {
            out.push(token.to_owned());
            i += 1;
        }
    }
    out.join(" ")
}

fn rewrite_roman(token: &str) -> &str {
    match token {
        "ii" => "2",
        "iii" => "3",
        "iv" => "4",
        "vi" => "6",
        "vii" => "7",
        "viii" => "8",
        "ix" => "9",
        other => other,
    }
}

fn ordinal(token: &str) -> Option<u32> {
    let n = match token {
        "1st" | "first" => 1,
        "2nd" | "second" => 2,
        "3rd" | "third" => 3,
        "4th" | "fourth" => 4,
        "5th" | "fifth" => 5,
        "6th" | "sixth" => 6,
        "7th" | "seventh" => 7,
        "8th" | "eighth" => 8,
        "9th" | "ninth" => 9,
        _ => return None,
    };
    Some(n)
}

/// `s` plus one or two digits, as in "S2" or "s02".
fn season_shorthand(token: &str) -> Option<u32> {
    let digits = token.strip_prefix('s')?;
    if !(1..=2).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// 1.0 minus the Levenshtein distance over the longer character count.
/// Two empty strings are 0.0, not a perfect match.
pub fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    1.0 - levenshtein(&a, &b) as f64 / longest as f64
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitution = prev[j] + usize::from(ca != cb);
            current[j + 1] = substitution.min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

/// What [`resolve`] decided. An entry and the confidence in it travel in the
/// same variant, so an entry carrying no confidence, or an `Exact` carrying no
/// entry, cannot be built.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Resolution {
    Exact(EntryId),
    /// `score` is the similarity that cleared the threshold, kept because the
    /// caller logs it and recomputing it means walking the library again.
    Likely {
        entry: EntryId,
        score: f64,
    },
    Unmatched,
}

/// Which library entry a proposal names, and how surely. The two travel
/// together, so an entry without a confidence, or an `Exact` without an
/// entry, cannot be built, in memory or off the store.
///
/// This is the durable half of a [`Resolution`]: the score a `Likely` was
/// decided on is logged once and never stored, and its `f64` is what keeps
/// `Resolution` out of `Eq` while a [`ProposedMatch`] compares whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Exact(EntryId),
    Likely(EntryId),
    Unmatched,
}

impl Link {
    pub fn entry(self) -> Option<EntryId> {
        match self {
            Link::Exact(entry) | Link::Likely(entry) => Some(entry),
            Link::Unmatched => None,
        }
    }

    pub fn confidence(self) -> Confidence {
        match self {
            Link::Exact(_) => Confidence::Exact,
            Link::Likely(_) => Confidence::Likely,
            Link::Unmatched => Confidence::Unmatched,
        }
    }

    pub fn names_entry(self) -> bool {
        self.entry().is_some()
    }

    /// The store's two columns as one value, or `None` when they disagree.
    /// The schema's `ON DELETE SET NULL` on `entry_id` is the one known
    /// producer of a disagreeing pair, so whoever adds entry deletion has
    /// to revisit this reader.
    pub(crate) fn from_columns(entry: Option<EntryId>, confidence: Confidence) -> Option<Link> {
        match (entry, confidence) {
            (Some(entry), Confidence::Exact) => Some(Link::Exact(entry)),
            (Some(entry), Confidence::Likely) => Some(Link::Likely(entry)),
            (None, Confidence::Unmatched) => Some(Link::Unmatched),
            (Some(_), Confidence::Unmatched) | (None, Confidence::Exact | Confidence::Likely) => {
                None
            }
        }
    }
}

impl From<Resolution> for Link {
    fn from(resolution: Resolution) -> Link {
        match resolution {
            Resolution::Exact(entry) => Link::Exact(entry),
            Resolution::Likely { entry, .. } => Link::Likely(entry),
            Resolution::Unmatched => Link::Unmatched,
        }
    }
}

/// The titles a person has confirmed, each folded the way [`resolve`]
/// compares them. A remembered title settles the decision outright: the
/// person said which show the file belongs to, so neither the similarity
/// threshold nor the short-needle guard applies to it afterwards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Aliases(HashMap<String, EntryId>);

impl Aliases {
    /// Folds every stored needle as it is read, so a row written under an
    /// older normalisation still answers.
    pub(crate) fn of(rows: impl IntoIterator<Item = (String, EntryId)>) -> Aliases {
        Aliases(
            rows.into_iter()
                .map(|(needle, entry)| (normalize_title(&needle), entry))
                .collect(),
        )
    }

    /// Keeps `parsed_title` as naming `entry` for the rest of this run; the
    /// store holds the same answer for the next one.
    pub(crate) fn remember(&mut self, parsed_title: &str, entry: EntryId) {
        self.0.insert(normalize_title(parsed_title), entry);
    }

    fn entry(&self, needle: &str) -> Option<EntryId> {
        self.0.get(needle).copied()
    }
}

/// The files a person has said not to track, by the raw title the player
/// reports. Ignoring is per file, not per show: the title is what was
/// pointed at, and none of it is parsed or folded, so a clip the parser
/// cannot read a show out of is still something you can silence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Ignored(HashSet<String>);

impl Ignored {
    pub(crate) fn of(rows: impl IntoIterator<Item = String>) -> Ignored {
        Ignored(rows.into_iter().collect())
    }

    pub(crate) fn contains(&self, raw_title: &str) -> bool {
        self.0.contains(raw_title)
    }

    /// Keeps the answer for the rest of this run; the store holds it for the
    /// next one.
    pub(crate) fn ignore(&mut self, raw_title: &str) {
        self.0.insert(raw_title.to_owned());
    }

    pub(crate) fn stop(&mut self, raw_title: &str) {
        self.0.remove(raw_title);
    }
}

/// The matching decision for an already-parsed title: a remembered title
/// first, then an entry whose title folds to the same thing, then the
/// closest entry above the threshold.
pub(crate) fn resolve(
    parsed_title: &str,
    library: &[LibraryEntry],
    aliases: &Aliases,
) -> Resolution {
    let needle = normalize_title(parsed_title);
    if needle.is_empty() {
        return Resolution::Unmatched;
    }
    // A remembered title outranks every gate below, but only while the
    // library still holds what it names.
    if let Some(entry) = aliases
        .entry(&needle)
        .filter(|id| library.iter().any(|entry| entry.id == *id))
    {
        return Resolution::Exact(entry);
    }
    if let Some(entry) = library
        .iter()
        .find(|entry| normalize_title(&entry.title) == needle)
    {
        return Resolution::Exact(entry.id);
    }
    if needle.chars().count() < 5 {
        return Resolution::Unmatched;
    }
    let mut best: Option<(EntryId, f64)> = None;
    for entry in library {
        let score = similarity(&normalize_title(&entry.title), &needle);
        if best.is_none_or(|(_, top)| score > top) {
            best = Some((entry.id, score));
        }
    }
    match best {
        Some((entry, score)) if score >= 0.85 => Resolution::Likely { entry, score },
        _ => Resolution::Unmatched,
    }
}

/// Parses one playback title and decides which library entry it names.
pub(crate) fn propose(
    event: &PlaybackEvent,
    library: &[LibraryEntry],
    aliases: &Aliases,
    ignored: &Ignored,
) -> ProposedMatch {
    let reading = parse(&event.title, &Options::default());
    let parsed_title = reading
        .title()
        .map(|title| title.value.to_owned())
        .unwrap_or_default();
    // An ignored file is not matched against the library at all, so the card
    // names the file and offers the way back instead of naming a show.
    let resolution = if ignored.contains(&event.title) {
        Resolution::Unmatched
    } else {
        resolve(&parsed_title, library, aliases)
    };
    let score = match resolution {
        Resolution::Likely { score, .. } => Some(score),
        Resolution::Exact(_) | Resolution::Unmatched => None,
    };
    let link = Link::from(resolution);
    tracing::debug!(
        confidence = link.confidence().tag(),
        entry = link.entry().map(EntryId::as_i64),
        score,
        "match proposed"
    );
    ProposedMatch {
        raw_title: event.title.clone(),
        parsed_title,
        episode: reading.episodes().map(|episodes| episodes.value),
        season: reading.season().map(|season| season.value),
        release_group: reading.release_group().map(|group| group.value.to_owned()),
        link,
        player: event.player.clone(),
        at: event.observed_at,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{PlaybackSource, PlaybackStatus, WatchStatus};

    fn library(titles: &[&str]) -> Vec<LibraryEntry> {
        titles
            .iter()
            .enumerate()
            .map(|(index, title)| LibraryEntry {
                id: EntryId(index as i64 + 1),
                title: (*title).to_owned(),
                status: WatchStatus::Watching,
                progress: 0,
                total: None,
                rewatching: false,
            })
            .collect()
    }

    fn event(title: &str) -> PlaybackEvent {
        PlaybackEvent {
            player: "mpv".into(),
            title: title.into(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(10),
            duration: Duration::from_secs(1420),
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            source: PlaybackSource::Detected,
            foreground: None,
        }
    }

    #[test]
    fn punctuation_variants_normalize_alike() {
        assert_eq!(
            normalize_title("Frieren - Beyond Journey's End"),
            normalize_title("Frieren: Beyond Journey's End")
        );
        assert_eq!(normalize_title("Fate/stay night"), "fate stay night");
    }

    #[test]
    fn leading_the_is_dropped() {
        assert_eq!(
            normalize_title("The Apothecary Diaries"),
            "apothecary diaries"
        );
    }

    #[test]
    fn roman_numerals_become_digits() {
        assert_eq!(normalize_title("Mob Psycho 100 II"), "mob psycho 100 2");
    }

    #[test]
    fn season_phrasings_collapse_to_the_number() {
        assert_eq!(normalize_title("2nd Season"), "2");
        assert_eq!(normalize_title("Season 2"), "2");
        assert_eq!(normalize_title("S2"), "2");
    }

    #[test]
    fn ampersand_is_spelled_out() {
        assert_eq!(normalize_title("Hana & Alice"), "hana and alice");
    }

    // A browser reports the title a page wrote, which for most of the world
    // is not ASCII. Folding those to nothing made every such show unmatchable
    // and, worse, unrememberable: `resolve` gives up on an empty needle before
    // it reads the aliases, so confirming one could never stick.
    #[test]
    fn a_title_outside_ascii_still_folds_to_something() {
        assert_eq!(normalize_title("葬送のフリーレン"), "葬送のフリーレン");
        assert_eq!(normalize_title("《幼女戰記 2》"), "幼女戰記 2");
        assert_eq!(normalize_title("Pokémon"), "pokémon");
        assert_eq!(normalize_title("ＴＶアニメ"), "ｔｖアニメ");
    }

    #[test]
    fn a_title_outside_ascii_can_be_remembered() {
        let library = library(&["Frieren: Beyond Journey's End"]);
        let aliases = Aliases::of([("葬送のフリーレン".to_owned(), library[0].id)]);
        assert_eq!(
            resolve("葬送のフリーレン", &library, &aliases),
            Resolution::Exact(library[0].id)
        );
    }

    #[test]
    fn similarity_bounds_and_kitten() {
        assert_eq!(similarity("kitten", "kitten"), 1.0);
        assert_eq!(similarity("", ""), 0.0);
        let expected = 1.0 - 3.0 / 7.0;
        assert!((similarity("kitten", "sitting") - expected).abs() < 1e-9);
    }

    #[test]
    fn resolve_finds_an_exact_match_through_normalization() {
        let library = library(&["Frieren: Beyond Journey's End"]);
        assert_eq!(
            resolve(
                "Frieren - Beyond Journey's End",
                &library,
                &Aliases::default()
            ),
            Resolution::Exact(library[0].id)
        );
    }

    #[test]
    fn resolve_finds_a_likely_match_above_the_threshold() {
        let library = library(&["Frieren Beyond Journeys End"]);
        let Resolution::Likely { entry, score } =
            resolve("Frieren Beyond Journey End", &library, &Aliases::default())
        else {
            panic!("expected a likely match");
        };
        assert_eq!(entry, library[0].id);
        // The score is carried out of the walk rather than recomputed, so it
        // has to be the similarity that cleared the threshold.
        assert_eq!(
            score,
            similarity(
                &normalize_title(&library[0].title),
                &normalize_title("Frieren Beyond Journey End")
            )
        );
        assert!(score >= 0.85);
    }

    #[test]
    fn resolve_prefers_the_first_entry_on_a_tie() {
        let library = library(&["abcdefgx", "abcdefgy"]);
        assert!(matches!(
            resolve("abcdefgz", &library, &Aliases::default()),
            Resolution::Likely { entry, .. } if entry == library[0].id
        ));
    }

    #[test]
    fn resolve_without_a_close_entry_is_unmatched() {
        let library = library(&["Frieren"]);
        let none = Aliases::default();
        assert_eq!(
            resolve("Mushoku Tensei", &library, &none),
            Resolution::Unmatched
        );
        assert_eq!(resolve("", &library, &none), Resolution::Unmatched);
    }

    #[test]
    fn resolve_never_calls_a_short_needle_likely() {
        let library = library(&["abcz"]);
        assert_eq!(
            resolve("abcd", &library, &Aliases::default()),
            Resolution::Unmatched
        );
    }

    #[test]
    fn a_remembered_title_settles_the_match() {
        let library = library(&["Frieren: Beyond Journey's End"]);
        let aliases = Aliases::of([("sousou no frieren".to_owned(), library[0].id)]);
        assert_eq!(
            resolve("Sousou no Frieren", &library, &aliases),
            Resolution::Exact(library[0].id)
        );
        assert_eq!(
            resolve("Sousou no Frieren", &library, &Aliases::default()),
            Resolution::Unmatched
        );
    }

    #[test]
    fn a_remembered_title_beats_the_short_needle_guard() {
        let library = library(&["Bocchi the Rock!"]);
        let aliases = Aliases::of([("BTR".to_owned(), library[0].id)]);
        assert_eq!(
            resolve("BTR", &library, &aliases),
            Resolution::Exact(library[0].id)
        );
        assert_eq!(
            resolve("BTR", &library, &Aliases::default()),
            Resolution::Unmatched
        );
    }

    /// Nothing deletes an entry today, but a remembered title must not name
    /// one the library does not hold, or the gates would run against it.
    #[test]
    fn a_remembered_entry_that_left_the_library_is_ignored() {
        let library = library(&["Frieren"]);
        let aliases = Aliases::of([("sousou no frieren".to_owned(), EntryId(99))]);
        assert_eq!(
            resolve("Sousou no Frieren", &library, &aliases),
            Resolution::Unmatched
        );
    }

    #[test]
    fn stored_needles_are_folded_as_they_are_read() {
        let library = library(&["Frieren: Beyond Journey's End"]);
        let aliases = Aliases::of([("Sousou no Frieren".to_owned(), library[0].id)]);
        assert_eq!(
            resolve("sousou  no  frieren", &library, &aliases),
            Resolution::Exact(library[0].id)
        );
    }

    #[test]
    fn propose_carries_player_and_at() {
        let event = event("[Subs] Show - 03 (1080p).mkv");
        let proposal = propose(&event, &[], &Aliases::default(), &Ignored::default());
        assert_eq!(proposal.raw_title, event.title);
        assert_eq!(proposal.player, "mpv");
        assert_eq!(proposal.at, event.observed_at);
    }

    #[test]
    fn title_in_prefers_the_entry_then_parsed_then_raw() {
        let library = library(&["Frieren: Beyond Journey's End"]);
        let proposal = propose(
            &event("[Subs] Show - 03.mkv"),
            &library,
            &Aliases::default(),
            &Ignored::default(),
        );
        assert_eq!(proposal.shown_title(), "Show");
        assert_eq!(proposal.title_in(&library), "Show");
        let matched = ProposedMatch {
            link: Link::Exact(library[0].id),
            ..proposal.clone()
        };
        assert_eq!(matched.title_in(&library), "Frieren: Beyond Journey's End");
        let raw = ProposedMatch {
            parsed_title: String::new(),
            ..proposal
        };
        assert_eq!(raw.shown_title(), "[Subs] Show - 03.mkv");
        assert_eq!(raw.title_in(&library), "[Subs] Show - 03.mkv");
    }

    #[test]
    fn propose_without_a_title_is_unmatched() {
        let proposal = propose(
            &event(""),
            &library(&["Show"]),
            &Aliases::default(),
            &Ignored::default(),
        );
        assert_eq!(proposal.parsed_title, "");
        assert_eq!(proposal.link, Link::Unmatched);
    }

    #[test]
    fn an_ignored_file_is_unmatched_however_well_its_title_reads() {
        let library = library(&["Show"]);
        let event = event("Show - 03.mkv");
        assert_eq!(
            propose(&event, &library, &Aliases::default(), &Ignored::default()).link,
            Link::Exact(library[0].id)
        );
        let ignored = Ignored::of([event.title.clone()]);
        assert_eq!(
            propose(&event, &library, &Aliases::default(), &ignored).link,
            Link::Unmatched
        );
    }

    #[test]
    fn ignoring_one_file_leaves_the_rest_of_the_show_alone() {
        let library = library(&["Show"]);
        let ignored = Ignored::of(["Show - 03.mkv".to_owned()]);
        assert_eq!(
            propose(
                &event("Show - 04.mkv"),
                &library,
                &Aliases::default(),
                &ignored
            )
            .link,
            Link::Exact(library[0].id)
        );
    }

    #[test]
    fn ignoring_a_file_outranks_a_remembered_title() {
        let library = library(&["Frieren: Beyond Journey's End"]);
        let aliases = Aliases::of([("Show".to_owned(), library[0].id)]);
        let event = event("Show - 03.mkv");
        let ignored = Ignored::of([event.title.clone()]);
        assert_eq!(
            propose(&event, &library, &aliases, &ignored).link,
            Link::Unmatched
        );
    }

    #[test]
    fn link_from_columns_rejects_a_disagreeing_pair() {
        let id = EntryId(1);
        assert_eq!(
            Link::from_columns(Some(id), Confidence::Exact),
            Some(Link::Exact(id))
        );
        assert_eq!(
            Link::from_columns(Some(id), Confidence::Likely),
            Some(Link::Likely(id))
        );
        assert_eq!(
            Link::from_columns(None, Confidence::Unmatched),
            Some(Link::Unmatched)
        );
        assert_eq!(Link::from_columns(None, Confidence::Exact), None);
        assert_eq!(Link::from_columns(None, Confidence::Likely), None);
        assert_eq!(Link::from_columns(Some(id), Confidence::Unmatched), None);
    }
}
