use crate::engine::RuleName;
use crate::reading::Certainty;

/// A byte range into the string the caller passed to [`crate::parse`]. The
/// parser never rewrites that string, so a span always slices it, and a shell
/// can point at what a rule read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn slice<'a>(&self, input: &'a str) -> &'a str {
        &input[self.start..self.end]
    }
}

/// A category of information extracted from a filename.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ElementKind {
    AnimeSeason,
    AnimeSeasonPrefix,
    AnimeTitle,
    AnimeType,
    AnimeYear,
    AudioTerm,
    DeviceCompatibility,
    EpisodeNumber,
    EpisodeNumberAlt,
    EpisodePrefix,
    EpisodeTitle,
    FileChecksum,
    FileExtension,
    FileName,
    Language,
    Other,
    ReleaseGroup,
    ReleaseInformation,
    ReleaseVersion,
    Source,
    Subtitles,
    VideoResolution,
    VideoTerm,
    VolumeNumber,
    VolumePrefix,
}

impl ElementKind {
    pub const ALL: [ElementKind; 25] = [
        ElementKind::AnimeSeason,
        ElementKind::AnimeSeasonPrefix,
        ElementKind::AnimeTitle,
        ElementKind::AnimeType,
        ElementKind::AnimeYear,
        ElementKind::AudioTerm,
        ElementKind::DeviceCompatibility,
        ElementKind::EpisodeNumber,
        ElementKind::EpisodeNumberAlt,
        ElementKind::EpisodePrefix,
        ElementKind::EpisodeTitle,
        ElementKind::FileChecksum,
        ElementKind::FileExtension,
        ElementKind::FileName,
        ElementKind::Language,
        ElementKind::Other,
        ElementKind::ReleaseGroup,
        ElementKind::ReleaseInformation,
        ElementKind::ReleaseVersion,
        ElementKind::Source,
        ElementKind::Subtitles,
        ElementKind::VideoResolution,
        ElementKind::VideoTerm,
        ElementKind::VolumeNumber,
        ElementKind::VolumePrefix,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ElementKind::AnimeSeason => "anime_season",
            ElementKind::AnimeSeasonPrefix => "anime_season_prefix",
            ElementKind::AnimeTitle => "anime_title",
            ElementKind::AnimeType => "anime_type",
            ElementKind::AnimeYear => "anime_year",
            ElementKind::AudioTerm => "audio_term",
            ElementKind::DeviceCompatibility => "device_compatibility",
            ElementKind::EpisodeNumber => "episode_number",
            ElementKind::EpisodeNumberAlt => "episode_number_alt",
            ElementKind::EpisodePrefix => "episode_prefix",
            ElementKind::EpisodeTitle => "episode_title",
            ElementKind::FileChecksum => "file_checksum",
            ElementKind::FileExtension => "file_extension",
            ElementKind::FileName => "file_name",
            ElementKind::Language => "language",
            ElementKind::Other => "other",
            ElementKind::ReleaseGroup => "release_group",
            ElementKind::ReleaseInformation => "release_information",
            ElementKind::ReleaseVersion => "release_version",
            ElementKind::Source => "source",
            ElementKind::Subtitles => "subtitles",
            ElementKind::VideoResolution => "video_resolution",
            ElementKind::VideoTerm => "video_term",
            ElementKind::VolumeNumber => "volume_number",
            ElementKind::VolumePrefix => "volume_prefix",
        }
    }

    pub fn from_label(label: &str) -> Option<ElementKind> {
        ElementKind::ALL
            .into_iter()
            .find(|kind| kind.label() == label)
    }

    /// A singular kind holds one value per filename, so the terms rule
    /// reads no second one. Rules that read shapes append regardless, as
    /// `[v2]` beside `05v2` makes two release versions.
    pub(crate) fn is_singular(self) -> bool {
        !matches!(
            self,
            ElementKind::AnimeSeason
                | ElementKind::AnimeType
                | ElementKind::AudioTerm
                | ElementKind::DeviceCompatibility
                | ElementKind::EpisodeNumber
                | ElementKind::Language
                | ElementKind::Other
                | ElementKind::ReleaseInformation
                | ElementKind::Source
                | ElementKind::VideoTerm
        )
    }
}

/// One value the parser read, and the evidence behind it: where in the
/// input, which rule, and how sure that rule is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub kind: ElementKind,
    /// Not always the spanned text as written: `v2` reads as `2`, and a
    /// title's delimiters fold to spaces.
    pub value: String,
    pub span: Span,
    pub by: RuleName,
    pub certainty: Certainty,
}

/// The elements parsed out of one filename, in the order the rules read
/// them. Never sorted by position: the fixtures pin the order of repeated
/// kinds, and a pre-identified term comes before one the table matched
/// whatever their places in the name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Elements {
    facts: Vec<Fact>,
}

impl Elements {
    pub fn get(&self, kind: ElementKind) -> Option<&str> {
        self.fact(kind).map(|fact| fact.value.as_str())
    }

    pub fn get_all(&self, kind: ElementKind) -> Vec<&str> {
        self.facts
            .iter()
            .filter(|fact| fact.kind == kind)
            .map(|fact| fact.value.as_str())
            .collect()
    }

    pub fn contains(&self, kind: ElementKind) -> bool {
        self.facts.iter().any(|fact| fact.kind == kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = (ElementKind, &str)> {
        self.facts
            .iter()
            .map(|fact| (fact.kind, fact.value.as_str()))
    }

    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    /// The first fact of a kind.
    pub fn fact(&self, kind: ElementKind) -> Option<&Fact> {
        self.facts.iter().find(|fact| fact.kind == kind)
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// An empty value is no fact at all.
    pub(crate) fn push(&mut self, fact: Fact) {
        if !fact.value.is_empty() {
            self.facts.push(fact);
        }
    }

    /// Drops the first fact of the kind with this value.
    pub(crate) fn retract(&mut self, kind: ElementKind, value: &str) {
        if let Some(position) = self
            .facts
            .iter()
            .position(|fact| fact.kind == kind && fact.value == value)
        {
            self.facts.remove(position);
        }
    }

    pub(crate) fn retag_first(&mut self, from: ElementKind, to: ElementKind) {
        if let Some(fact) = self.facts.iter_mut().find(|fact| fact.kind == from) {
            fact.kind = to;
        }
    }
}

#[cfg(test)]
pub(crate) fn fact(kind: ElementKind, value: &str) -> Fact {
    Fact {
        kind,
        value: value.to_owned(),
        span: Span { start: 0, end: 0 },
        by: RuleName::Terms,
        certainty: Certainty::Shaped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elements(items: &[(ElementKind, &str)]) -> Elements {
        let mut elements = Elements::default();
        for (kind, value) in items {
            elements.push(fact(*kind, value));
        }
        elements
    }

    #[test]
    fn labels_round_trip_for_every_kind() {
        for kind in ElementKind::ALL {
            assert_eq!(ElementKind::from_label(kind.label()), Some(kind));
        }
        assert_eq!(ElementKind::from_label("no_such_kind"), None);
    }

    #[test]
    fn push_appends_for_every_kind() {
        let elements = elements(&[
            (ElementKind::AudioTerm, "FLAC"),
            (ElementKind::AudioTerm, "Dual Audio"),
            (ElementKind::VideoResolution, "720p"),
            (ElementKind::VideoResolution, "1080p"),
        ]);
        assert_eq!(
            elements.get_all(ElementKind::AudioTerm),
            ["FLAC", "Dual Audio"]
        );
        assert_eq!(elements.get(ElementKind::AudioTerm), Some("FLAC"));
        assert_eq!(
            elements.get_all(ElementKind::VideoResolution),
            ["720p", "1080p"]
        );
        assert_eq!(elements.get(ElementKind::VideoResolution), Some("720p"));
        assert_eq!(elements.len(), 4);
    }

    #[test]
    fn retract_drops_the_first_matching_fact_only() {
        let mut elements = elements(&[(ElementKind::Other, "Ita"), (ElementKind::Other, "Ita")]);
        elements.retract(ElementKind::Other, "Ita");
        assert_eq!(elements.get_all(ElementKind::Other), ["Ita"]);
        elements.retract(ElementKind::Other, "END");
        assert_eq!(elements.len(), 1);
    }

    #[test]
    fn empty_values_are_ignored() {
        let elements = elements(&[(ElementKind::AnimeTitle, "")]);
        assert!(elements.is_empty());
    }
}
