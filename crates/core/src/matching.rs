//! Matching a playback title against the library.
//!
//! [`propose`] runs the filename parser over a raw player title and decides
//! which library entry, if any, it names. The decision is a
//! [`ProposedMatch`]: the shell shows it and the store keeps the latest one.

use std::time::SystemTime;

use ryuuji_parse::{ElementKind, Options, parse};

use crate::{EntryId, LibraryEntry, PlaybackEvent};

/// How sure the matcher is that a proposal names a library entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    Exact,
    Likely,
    Unmatched,
}

impl Confidence {
    /// Every confidence, strongest first.
    pub const ALL: [Confidence; 3] = [Confidence::Exact, Confidence::Likely, Confidence::Unmatched];

    /// Stable identifier stored in the database.
    pub fn tag(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Likely => "likely",
            Confidence::Unmatched => "unmatched",
        }
    }

    /// Inverse of [`Confidence::tag`].
    pub fn from_tag(tag: &str) -> Option<Confidence> {
        Confidence::ALL
            .into_iter()
            .find(|confidence| confidence.tag() == tag)
    }

    pub fn label(self) -> &'static str {
        match self {
            Confidence::Exact => "Exact match",
            Confidence::Likely => "Likely match",
            Confidence::Unmatched => "No library entry",
        }
    }
}

/// What has happened to a proposal so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchOutcome {
    Proposed,
    Confirmed,
    Dismissed,
}

impl MatchOutcome {
    /// Every outcome, in lifecycle order.
    pub const ALL: [MatchOutcome; 3] = [
        MatchOutcome::Proposed,
        MatchOutcome::Confirmed,
        MatchOutcome::Dismissed,
    ];

    /// Stable identifier stored in the database.
    pub fn tag(self) -> &'static str {
        match self {
            MatchOutcome::Proposed => "proposed",
            MatchOutcome::Confirmed => "confirmed",
            MatchOutcome::Dismissed => "dismissed",
        }
    }

    /// Inverse of [`MatchOutcome::tag`].
    pub fn from_tag(tag: &str) -> Option<MatchOutcome> {
        MatchOutcome::ALL
            .into_iter()
            .find(|outcome| outcome.tag() == tag)
    }

    pub fn label(self) -> &'static str {
        match self {
            MatchOutcome::Proposed => "Proposed",
            MatchOutcome::Confirmed => "Confirmed",
            MatchOutcome::Dismissed => "Dismissed",
        }
    }
}

/// One playback title's decision against the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedMatch {
    pub raw_title: String,
    /// Empty when the parser found no title.
    pub parsed_title: String,
    pub episode: Option<u32>,
    pub season: Option<u32>,
    pub release_group: Option<String>,
    pub entry: Option<EntryId>,
    pub confidence: Confidence,
    pub outcome: MatchOutcome,
    pub player: String,
    pub at: SystemTime,
    /// (ElementKind label, value) for every parsed element; memory only,
    /// empty after reload.
    pub elements: Vec<(String, String)>,
}

/// Folds a title to the loose form matching compares: lowercase ASCII
/// alphanumeric tokens, `&` spelled out, a leading "the" dropped, roman
/// numerals and season phrasings reduced to the bare number.
pub fn normalize_title(title: &str) -> String {
    let cleaned: String = title
        .replace('&', " and ")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
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

/// The matching decision for an already-parsed title.
fn resolve(parsed_title: &str, library: &[LibraryEntry]) -> (Option<EntryId>, Confidence) {
    let needle = normalize_title(parsed_title);
    if needle.is_empty() {
        return (None, Confidence::Unmatched);
    }
    if let Some(entry) = library
        .iter()
        .find(|entry| normalize_title(&entry.title) == needle)
    {
        return (Some(entry.id), Confidence::Exact);
    }
    if needle.chars().count() < 5 {
        return (None, Confidence::Unmatched);
    }
    let mut best: Option<(EntryId, f64)> = None;
    for entry in library {
        let score = similarity(&normalize_title(&entry.title), &needle);
        if best.is_none_or(|(_, top)| score > top) {
            best = Some((entry.id, score));
        }
    }
    match best {
        Some((id, score)) if score >= 0.85 => (Some(id), Confidence::Likely),
        _ => (None, Confidence::Unmatched),
    }
}

/// Parses one playback title and decides which library entry it names.
pub fn propose(event: &PlaybackEvent, library: &[LibraryEntry]) -> ProposedMatch {
    let parsed = parse(&event.title, &Options::default());
    let parsed_title = parsed
        .get(ElementKind::AnimeTitle)
        .unwrap_or_default()
        .to_owned();
    let (entry, confidence) = resolve(&parsed_title, library);
    let score = (confidence == Confidence::Likely)
        .then(|| {
            entry
                .and_then(|id| library.iter().find(|candidate| candidate.id == id))
                .map(|candidate| {
                    similarity(
                        &normalize_title(&candidate.title),
                        &normalize_title(&parsed_title),
                    )
                })
        })
        .flatten();
    tracing::debug!(
        confidence = confidence.tag(),
        entry = entry.map(EntryId::as_i64),
        score,
        "match proposed"
    );
    ProposedMatch {
        raw_title: event.title.clone(),
        parsed_title,
        episode: parsed.episode_number(),
        season: parsed.season_number(),
        release_group: parsed.get(ElementKind::ReleaseGroup).map(str::to_owned),
        entry,
        confidence,
        outcome: MatchOutcome::Proposed,
        player: event.player.clone(),
        at: event.observed_at,
        elements: parsed
            .iter()
            .map(|(kind, value)| (kind.label().to_owned(), value.to_owned()))
            .collect(),
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
            resolve("Frieren - Beyond Journey's End", &library),
            (Some(library[0].id), Confidence::Exact)
        );
    }

    #[test]
    fn resolve_finds_a_likely_match_above_the_threshold() {
        let library = library(&["Frieren Beyond Journeys End"]);
        assert_eq!(
            resolve("Frieren Beyond Journey End", &library),
            (Some(library[0].id), Confidence::Likely)
        );
    }

    #[test]
    fn resolve_prefers_the_first_entry_on_a_tie() {
        let library = library(&["abcdefgx", "abcdefgy"]);
        assert_eq!(
            resolve("abcdefgz", &library),
            (Some(library[0].id), Confidence::Likely)
        );
    }

    #[test]
    fn resolve_without_a_close_entry_is_unmatched() {
        let library = library(&["Frieren"]);
        assert_eq!(
            resolve("Mushoku Tensei", &library),
            (None, Confidence::Unmatched)
        );
        assert_eq!(resolve("", &library), (None, Confidence::Unmatched));
    }

    #[test]
    fn resolve_never_calls_a_short_needle_likely() {
        let library = library(&["abcz"]);
        assert_eq!(resolve("abcd", &library), (None, Confidence::Unmatched));
    }

    #[test]
    fn confidence_tags_round_trip() {
        for confidence in Confidence::ALL {
            assert_eq!(Confidence::from_tag(confidence.tag()), Some(confidence));
        }
        assert_eq!(Confidence::from_tag("nope"), None);
    }

    #[test]
    fn match_outcome_tags_round_trip() {
        for outcome in MatchOutcome::ALL {
            assert_eq!(MatchOutcome::from_tag(outcome.tag()), Some(outcome));
        }
        assert_eq!(MatchOutcome::from_tag("nope"), None);
    }

    #[test]
    fn propose_carries_player_at_and_elements() {
        let event = event("[Subs] Show - 03 (1080p).mkv");
        let proposal = propose(&event, &[]);
        assert_eq!(proposal.raw_title, event.title);
        assert_eq!(proposal.player, "mpv");
        assert_eq!(proposal.at, event.observed_at);
        assert_eq!(proposal.outcome, MatchOutcome::Proposed);
        let expected: Vec<(String, String)> = parse(&event.title, &Options::default())
            .iter()
            .map(|(kind, value)| (kind.label().to_owned(), value.to_owned()))
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(proposal.elements, expected);
    }

    #[test]
    fn propose_without_a_title_is_unmatched() {
        let proposal = propose(&event(""), &library(&["Show"]));
        assert_eq!(proposal.parsed_title, "");
        assert_eq!(proposal.entry, None);
        assert_eq!(proposal.confidence, Confidence::Unmatched);
    }
}
