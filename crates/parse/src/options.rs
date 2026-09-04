/// Knobs for [`crate::parse`].
///
/// Deserializes field by field over [`Options::default`], so a document may
/// name only the knobs it changes, and an unknown key is an error rather
/// than a silently ignored typo.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    /// Characters that may split a token group; `-` is never a delimiter.
    pub allowed_delimiters: String,
    /// Substrings removed from the filename before tokenization.
    pub ignored_strings: Vec<String>,
    pub parse_episode_number: bool,
    pub parse_episode_title: bool,
    pub parse_file_extension: bool,
    pub parse_release_group: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            allowed_delimiters: " _.&+,|".to_owned(),
            ignored_strings: Vec::new(),
            parse_episode_number: true,
            parse_episode_title: true,
            parse_file_extension: true,
            parse_release_group: true,
        }
    }
}
