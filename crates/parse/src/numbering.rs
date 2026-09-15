//! What a number-shaped word spells: `05v2`, `01-02`, `S01E06`, `OVA1`,
//! `07.5`, `4a`, `#12`, `12話`. These read text and answer; the rules
//! decide.

mod scanner;

use std::ops::Range;

use scanner::{Scanner, eat_episode_separator, version};

use crate::element::ElementKind;
use crate::keyword::KeywordTable;
use crate::string;

pub(crate) const ANIME_YEAR_MIN: u32 = 1900;
pub(crate) const ANIME_YEAR_MAX: u32 = 2050;
pub(crate) const EPISODE_NUMBER_MAX: u32 = ANIME_YEAR_MIN - 1;
pub(crate) const VOLUME_NUMBER_MAX: u32 = 20;

/// A number counted off against a prefix. Episodes and volumes differ only
/// in their bounds and in which shapes they may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extent {
    Episode,
    Volume,
}

impl Extent {
    pub(crate) fn number(self) -> ElementKind {
        match self {
            Extent::Episode => ElementKind::EpisodeNumber,
            Extent::Volume => ElementKind::VolumeNumber,
        }
    }

    fn max_digits(self) -> usize {
        match self {
            Extent::Episode => 4,
            Extent::Volume => 2,
        }
    }

    pub(crate) fn max_value(self) -> u32 {
        match self {
            Extent::Episode => EPISODE_NUMBER_MAX,
            Extent::Volume => VOLUME_NUMBER_MAX,
        }
    }
}

/// A digit run that overflows `u32` compares as larger than every bound, so
/// the bound checks reject it.
pub(crate) fn leading_value(text: &str) -> u32 {
    string::leading_number(text).unwrap_or(u32::MAX)
}

/// A piece of a word: the value it spells and the byte range it was read
/// from, in the word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) text: String,
    pub(crate) at: Range<usize>,
}

/// One value a word spells. A held piece leaves its chars to a title, the
/// way an unidentifiable term does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) kind: ElementKind,
    pub(crate) part: Part,
    pub(crate) held: bool,
}

/// What one word spells, in reading order, and the byte range of the word
/// the numbers use up. `OVA1` uses up only the `1`; every other shape uses
/// up the whole word, so no stray `S`, `v` or `#` is left for a title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Numbering {
    pub(crate) pieces: Vec<Piece>,
    pub(crate) used: Range<usize>,
    /// The first episode decides for the whole word: when it only repeats
    /// an episode already read, the word records nothing. A range, a number
    /// sign, a fraction and a part letter decide; a version suffix, a season
    /// with its episode and a counter record the rest regardless.
    pub(crate) decides: bool,
}

impl Numbering {
    fn new(used: Range<usize>) -> Numbering {
        Numbering {
            pieces: Vec::new(),
            used,
            decides: false,
        }
    }

    fn push(&mut self, kind: ElementKind, part: Part) {
        self.pieces.push(Piece {
            kind,
            part,
            held: false,
        });
    }

    pub(crate) fn shift(mut self, by: usize) -> Numbering {
        for piece in &mut self.pieces {
            piece.part.at = piece.part.at.start + by..piece.part.at.end + by;
        }
        self.used = self.used.start + by..self.used.end + by;
        self
    }
}

/// The shapes, tried in order; the first that fits the word whole wins.
pub(crate) fn read(word: &str, extent: Extent) -> Option<Numbering> {
    read_with(word, |trimmed| {
        version_suffix(trimmed, extent)
            .or_else(|| range(trimmed, extent))
            .or_else(|| match extent {
                Extent::Episode => season_and_episode(trimmed)
                    .or_else(|| type_and_episode(trimmed))
                    .or_else(|| fraction(trimmed))
                    .or_else(|| partial(trimmed))
                    .or_else(|| number_sign(trimmed))
                    .or_else(|| counter(trimmed)),
                Extent::Volume => None,
            })
    })
}

/// Runs one shape over the word trimmed of dashes and spaces, with the
/// pieces shifted back to the word's own offsets. A plain number is no
/// shape at all.
pub(crate) fn read_with(
    word: &str,
    shape: impl FnOnce(&str) -> Option<Numbering>,
) -> Option<Numbering> {
    if string::is_numeric(word) {
        return None;
    }
    let trimmed = word.trim_matches([' ', '-']);
    if trimmed.is_empty() {
        return None;
    }
    let start = word.find(trimmed).unwrap_or(0);
    let mut numbering = shape(trimmed)?.shift(start);
    // The dashes and spaces trimmed off are used up with the number, so a
    // glued `S01E01-` leaves no dash behind for a title.
    if numbering.used.start == start {
        numbering.used.start = 0;
    }
    numbering.used.end = word.len();
    Some(numbering)
}

/// `05v2`: a number and its release version.
pub(crate) fn version_suffix(word: &str, extent: Extent) -> Option<Numbering> {
    let mut scanner = Scanner::new(word);
    let number = scanner.digits(extent.max_digits())?;
    let release_version = version(&mut scanner)?;
    if !scanner.done() {
        return None;
    }
    let mut numbering = Numbering::new(0..word.len());
    numbering.push(extent.number(), number);
    numbering.push(ElementKind::ReleaseVersion, release_version);
    Some(numbering)
}

/// `01-02v2`, `01+02`: a batch, the lower bound within the extent's limit.
pub(crate) fn range(word: &str, extent: Extent) -> Option<Numbering> {
    let mut scanner = Scanner::new(word);
    let lower = scanner.digits(extent.max_digits())?;
    // Only an episode range carries a version on its lower bound.
    let lower_version = match extent {
        Extent::Episode => version(&mut scanner),
        Extent::Volume => None,
    };
    if !scanner.eat_any(&['-', '~', '&', '+']) {
        return None;
    }
    let upper = scanner.digits(extent.max_digits())?;
    let upper_version = version(&mut scanner);
    if !scanner.done() {
        return None;
    }
    if leading_value(&lower.text) >= leading_value(&upper.text) {
        return None;
    }
    if leading_value(&lower.text) > extent.max_value() {
        return None;
    }
    let mut numbering = Numbering::new(0..word.len());
    numbering.decides = true;
    numbering.push(extent.number(), lower);
    numbering.push(extent.number(), upper);
    for part in [lower_version, upper_version].into_iter().flatten() {
        numbering.push(ElementKind::ReleaseVersion, part);
    }
    Some(numbering)
}

/// `S01E06v2`, `2x05`, `S01-02E03-04`: seasons and episodes together.
pub(crate) fn season_and_episode(word: &str) -> Option<Numbering> {
    let mut scanner = Scanner::new(word);
    scanner.eat_any(&['S', 's']);
    let first_season = scanner.digits(2)?;
    let second_season = scanner.attempt(|scanner| {
        scanner.eat('-').then(|| {
            scanner.eat_any(&['S', 's']);
            scanner.digits(2)
        })?
    });
    if !eat_episode_separator(&mut scanner) {
        return None;
    }
    let first_episode = scanner.digits(4)?;
    let second_episode = scanner.attempt(|scanner| {
        scanner.eat('-').then(|| {
            scanner.eat_any(&['E', 'e']);
            scanner.digits(4)
        })?
    });
    let release_version = version(&mut scanner);
    if !scanner.done() {
        return None;
    }
    if leading_value(&first_season.text) == 0 {
        return None;
    }
    let mut numbering = Numbering::new(0..word.len());
    numbering.push(ElementKind::AnimeSeason, first_season);
    if let Some(season) = second_season {
        numbering.push(ElementKind::AnimeSeason, season);
    }
    numbering.push(ElementKind::EpisodeNumber, first_episode);
    if let Some(episode) = second_episode {
        numbering.push(ElementKind::EpisodeNumber, episode);
    }
    if let Some(part) = release_version {
        numbering.push(ElementKind::ReleaseVersion, part);
    }
    Some(numbering)
}

/// `OVA1`: an anime type glued to its number. The type stays a title word
/// when the table says it is not identifiable; the number is read through
/// the other shapes, or whole.
pub(crate) fn type_and_episode(word: &str) -> Option<Numbering> {
    let digit_pos = word.find(|c: char| c.is_ascii_digit())?;
    let prefix = &word[..digit_pos];
    let keyword = KeywordTable::builtin().find(ElementKind::AnimeType, &prefix.to_uppercase())?;
    let number = &word[digit_pos..];
    let mut numbering = match read(number, Extent::Episode) {
        Some(numbering) => numbering.shift(digit_pos),
        None => {
            let mut numbering = Numbering::new(digit_pos..word.len());
            numbering.decides = true;
            numbering.push(
                ElementKind::EpisodeNumber,
                Part {
                    text: number.to_owned(),
                    at: digit_pos..word.len(),
                },
            );
            numbering
        }
    };
    numbering.pieces.insert(
        0,
        Piece {
            kind: ElementKind::AnimeType,
            part: Part {
                text: prefix.to_owned(),
                at: 0..digit_pos,
            },
            held: !keyword.identifiable,
        },
    );
    Some(numbering)
}

/// `07.5` is an episode; `1.11` and `8.0` are parts of titles and `5.1` is
/// an audio term, so any other fraction needs a leading zero (`04.1`)
/// before it counts, since no title or term writes one.
pub(crate) fn fraction(word: &str) -> Option<Numbering> {
    let mut scanner = Scanner::new(word);
    let whole = scanner.digits(usize::MAX)?;
    if !scanner.eat('.') {
        return None;
    }
    let leading_zero = whole.text.len() > 1 && whole.text.starts_with('0');
    let fraction_ok = if leading_zero {
        scanner.digits(1).is_some()
    } else {
        scanner.eat('5')
    };
    if !fraction_ok || !scanner.done() {
        return None;
    }
    whole_word(word, Extent::Episode)
}

/// `4a`: an episode and a part letter.
pub(crate) fn partial(word: &str) -> Option<Numbering> {
    let digits = word.chars().take_while(char::is_ascii_digit).count();
    let suffix: Vec<char> = word.chars().skip(digits).collect();
    if digits > 0 && suffix.len() == 1 && matches!(suffix[0], 'A'..='C' | 'a'..='c') {
        whole_word(word, Extent::Episode)
    } else {
        None
    }
}

/// `#01-02v2`: a number sign, then one or two episodes and a version.
pub(crate) fn number_sign(word: &str) -> Option<Numbering> {
    let rest = word.strip_prefix('#')?;
    let mut scanner = Scanner::new(rest);
    let first = scanner.digits(4)?;
    let second = scanner.attempt(|scanner| {
        scanner
            .eat_any(&['-', '~', '&', '+'])
            .then(|| scanner.digits(4))?
    });
    let release_version = version(&mut scanner);
    if !scanner.done() {
        return None;
    }
    if leading_value(&first.text) > EPISODE_NUMBER_MAX {
        return None;
    }
    let mut numbering = Numbering::new(0..rest.len());
    numbering.decides = true;
    numbering.push(ElementKind::EpisodeNumber, first);
    if let Some(episode) = second
        && leading_value(&episode.text) <= EPISODE_NUMBER_MAX
    {
        numbering.push(ElementKind::EpisodeNumber, episode);
    }
    if let Some(part) = release_version {
        numbering.push(ElementKind::ReleaseVersion, part);
    }
    let mut numbering = numbering.shift(1);
    numbering.used = 0..word.len();
    Some(numbering)
}

/// `12話`: a digit run closed by the Japanese episode counter. The `第`
/// before it is an episode prefix, read by the in-word prefix rule.
pub(crate) fn counter(word: &str) -> Option<Numbering> {
    if !word.ends_with('話') {
        return None;
    }
    let mut scanner = Scanner::new(word);
    let episode = scanner.digits(4)?;
    if !scanner.eat('話') || !scanner.done() {
        return None;
    }
    let mut numbering = Numbering::new(0..word.len());
    numbering.push(ElementKind::EpisodeNumber, episode);
    Some(numbering)
}

/// The whole word as one number, within the extent's limit.
fn whole_word(word: &str, extent: Extent) -> Option<Numbering> {
    if leading_value(word) > extent.max_value() {
        return None;
    }
    let mut numbering = Numbering::new(0..word.len());
    numbering.decides = true;
    numbering.push(
        extent.number(),
        Part {
            text: word.to_owned(),
            at: 0..word.len(),
        },
    );
    Some(numbering)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(word: &str, extent: Extent) -> Vec<(ElementKind, String, Range<usize>, bool)> {
        read(word, extent)
            .map(|numbering| {
                numbering
                    .pieces
                    .into_iter()
                    .map(|piece| (piece.kind, piece.part.text, piece.part.at, piece.held))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_version_suffix_reads_the_number_and_the_version() {
        assert_eq!(
            pieces("05v2", Extent::Episode),
            [
                (ElementKind::EpisodeNumber, "05".to_owned(), 0..2, false),
                (ElementKind::ReleaseVersion, "2".to_owned(), 2..4, false),
            ]
        );
        assert_eq!(
            pieces("3v1", Extent::Volume),
            [
                (ElementKind::VolumeNumber, "3".to_owned(), 0..1, false),
                (ElementKind::ReleaseVersion, "1".to_owned(), 1..3, false),
            ]
        );
    }

    #[test]
    fn a_range_reads_both_ends_and_their_versions() {
        assert_eq!(
            pieces("01v2-02v3", Extent::Episode),
            [
                (ElementKind::EpisodeNumber, "01".to_owned(), 0..2, false),
                (ElementKind::EpisodeNumber, "02".to_owned(), 5..7, false),
                (ElementKind::ReleaseVersion, "2".to_owned(), 2..4, false),
                (ElementKind::ReleaseVersion, "3".to_owned(), 7..9, false),
            ]
        );
        assert!(pieces("02-01", Extent::Episode).is_empty());
        assert!(pieces("1900-1901", Extent::Episode).is_empty());
        assert!(pieces("1v2-2", Extent::Volume).is_empty());
    }

    #[test]
    fn a_season_and_episode_word_reads_every_number() {
        assert_eq!(
            pieces("S01-02E03-04v2", Extent::Episode),
            [
                (ElementKind::AnimeSeason, "01".to_owned(), 1..3, false),
                (ElementKind::AnimeSeason, "02".to_owned(), 4..6, false),
                (ElementKind::EpisodeNumber, "03".to_owned(), 7..9, false),
                (ElementKind::EpisodeNumber, "04".to_owned(), 10..12, false),
                (ElementKind::ReleaseVersion, "2".to_owned(), 12..14, false),
            ]
        );
        assert_eq!(
            pieces("2x05", Extent::Episode),
            [
                (ElementKind::AnimeSeason, "2".to_owned(), 0..1, false),
                (ElementKind::EpisodeNumber, "05".to_owned(), 2..4, false),
            ]
        );
        assert!(pieces("S00E01", Extent::Episode).is_empty());
    }

    #[test]
    fn a_type_glued_to_its_number_holds_the_type() {
        let numbering = read("OVA1", Extent::Episode).unwrap();
        assert_eq!(
            pieces("OVA1", Extent::Episode),
            [
                (ElementKind::AnimeType, "OVA".to_owned(), 0..3, true),
                (ElementKind::EpisodeNumber, "1".to_owned(), 3..4, false),
            ]
        );
        assert_eq!(numbering.used, 3..4);
        assert_eq!(
            pieces("OVA01v2", Extent::Episode),
            [
                (ElementKind::AnimeType, "OVA".to_owned(), 0..3, true),
                (ElementKind::EpisodeNumber, "01".to_owned(), 3..5, false),
                (ElementKind::ReleaseVersion, "2".to_owned(), 5..7, false),
            ]
        );
    }

    #[test]
    fn fractions_and_partials_read_the_whole_word() {
        assert_eq!(
            pieces("07.5", Extent::Episode),
            [(ElementKind::EpisodeNumber, "07.5".to_owned(), 0..4, false)]
        );
        assert_eq!(
            pieces("04.1", Extent::Episode),
            [(ElementKind::EpisodeNumber, "04.1".to_owned(), 0..4, false)]
        );
        assert!(pieces("1.11", Extent::Episode).is_empty());
        assert!(pieces("8.0", Extent::Episode).is_empty());
        assert_eq!(
            pieces("4a", Extent::Episode),
            [(ElementKind::EpisodeNumber, "4a".to_owned(), 0..2, false)]
        );
        assert!(pieces("4d", Extent::Episode).is_empty());
    }

    #[test]
    fn a_number_sign_and_a_counter_read_their_digits() {
        assert_eq!(
            pieces("#01-02v2", Extent::Episode),
            [
                (ElementKind::EpisodeNumber, "01".to_owned(), 1..3, false),
                (ElementKind::EpisodeNumber, "02".to_owned(), 4..6, false),
                (ElementKind::ReleaseVersion, "2".to_owned(), 6..8, false),
            ]
        );
        assert_eq!(
            pieces("12話", Extent::Episode),
            [(ElementKind::EpisodeNumber, "12".to_owned(), 0..2, false)]
        );
    }

    #[test]
    fn only_shapes_that_give_up_on_their_first_episode_decide() {
        let decides = |word: &str| read(word, Extent::Episode).map(|numbering| numbering.decides);
        for word in ["01-02", "#03", "07.5", "4a", "OVA1", "OVA01-02"] {
            assert_eq!(decides(word), Some(true), "{word}");
        }
        for word in ["05v2", "S01E02", "12\u{8a71}", "OVA01v2"] {
            assert_eq!(decides(word), Some(false), "{word}");
        }
    }

    #[test]
    fn a_trimmed_word_keeps_offsets_into_the_original() {
        let numbering = read("-05v2-", Extent::Episode).unwrap();
        assert_eq!(numbering.used, 0..6);
        assert_eq!(numbering.pieces[0].part.at, 1..3);
        assert!(read("05", Extent::Episode).is_none());
        assert!(read("--", Extent::Episode).is_none());
    }
}
