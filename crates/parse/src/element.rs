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

    /// A singular kind holds one value per filename, so the keyword pass
    /// stops looking once one is set.
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

/// The elements parsed out of one filename, in the order they were found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Elements {
    items: Vec<(ElementKind, String)>,
}

impl Elements {
    pub fn get(&self, kind: ElementKind) -> Option<&str> {
        self.items
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, value)| value.as_str())
    }

    pub fn get_all(&self, kind: ElementKind) -> Vec<&str> {
        self.items
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn contains(&self, kind: ElementKind) -> bool {
        self.items.iter().any(|(k, _)| *k == kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = (ElementKind, &str)> {
        self.items
            .iter()
            .map(|(kind, value)| (*kind, value.as_str()))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn insert(&mut self, kind: ElementKind, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.items.push((kind, value));
    }

    pub fn remove(&mut self, kind: ElementKind) {
        self.items.retain(|(k, _)| *k != kind);
    }

    pub(crate) fn remove_value(&mut self, kind: ElementKind, value: &str) {
        if let Some(position) = self
            .items
            .iter()
            .position(|(k, v)| *k == kind && v == value)
        {
            self.items.remove(position);
        }
    }

    pub(crate) fn retag_first(&mut self, from: ElementKind, to: ElementKind) {
        if let Some(item) = self.items.iter_mut().find(|(k, _)| *k == from) {
            item.0 = to;
        }
    }

    pub fn episode_number(&self) -> Option<u32> {
        leading_number(self.get(ElementKind::EpisodeNumber)?)
    }

    pub fn season_number(&self) -> Option<u32> {
        leading_number(self.get(ElementKind::AnimeSeason)?)
    }
}

fn leading_number(value: &str) -> Option<u32> {
    let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_for_every_kind() {
        for kind in ElementKind::ALL {
            assert_eq!(ElementKind::from_label(kind.label()), Some(kind));
        }
        assert_eq!(ElementKind::from_label("no_such_kind"), None);
    }

    #[test]
    fn episode_number_reads_the_leading_digit_run() {
        let cases = [
            ("01v2", Some(1)),
            ("4a", Some(4)),
            ("7.5", Some(7)),
            ("v2", None),
        ];
        for (value, expected) in cases {
            let mut elements = Elements::default();
            elements.insert(ElementKind::EpisodeNumber, value);
            assert_eq!(elements.episode_number(), expected, "value {value:?}");
        }
        assert_eq!(Elements::default().episode_number(), None);
    }

    #[test]
    fn season_number_reads_the_first_anime_season() {
        let mut elements = Elements::default();
        elements.insert(ElementKind::AnimeSeason, "2");
        elements.insert(ElementKind::AnimeSeason, "3");
        assert_eq!(elements.season_number(), Some(2));
    }

    #[test]
    fn insert_appends_for_every_kind() {
        let mut elements = Elements::default();
        elements.insert(ElementKind::AudioTerm, "FLAC");
        elements.insert(ElementKind::AudioTerm, "Dual Audio");
        elements.insert(ElementKind::VideoResolution, "720p");
        elements.insert(ElementKind::VideoResolution, "1080p");
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
    fn empty_values_are_ignored() {
        let mut elements = Elements::default();
        elements.insert(ElementKind::AnimeTitle, "");
        assert!(elements.is_empty());
    }
}
