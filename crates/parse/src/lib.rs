//! Anime filename parser. A clean-room, anitomy-style element extractor:
//! [`parse`] splits a filename into tokens and identifies known elements
//! (resolution, source, audio and video terms, checksum, ...).

mod element;
mod keyword;
mod options;
mod parser;
mod string;
mod token;
mod tokenizer;

pub use element::{ElementKind, Elements};
pub use options::Options;

pub fn parse(input: &str, options: &Options) -> Elements {
    let table = keyword::KeywordTable::builtin();
    let mut elements = Elements::default();
    let mut text = input.to_owned();

    if options.parse_file_extension
        && let Some(dot) = text.rfind('.')
    {
        let extension = text[dot + 1..].to_owned();
        if (1..=4).contains(&extension.len())
            && extension.chars().all(|c| c.is_ascii_alphanumeric())
            && table
                .find(ElementKind::FileExtension, &extension.to_uppercase())
                .is_some()
        {
            elements.insert(ElementKind::FileExtension, extension);
            text.truncate(dot);
        }
    }

    for ignored in &options.ignored_strings {
        if !ignored.is_empty() {
            text = text.replace(ignored, "");
        }
    }

    elements.insert(ElementKind::FileName, text.clone());
    let tokens = tokenizer::tokenize(&text, options, table);
    parser::Parser::new(tokens, elements, options, table).run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extension_is_stripped_with_original_case() {
        let elements = parse("Toradora!.MKV", &Options::default());
        assert_eq!(elements.get(ElementKind::FileExtension), Some("MKV"));
        assert_eq!(elements.get(ElementKind::FileName), Some("Toradora!"));
    }

    #[test]
    fn extension_stays_when_parsing_is_disabled() {
        let options = Options {
            parse_file_extension: false,
            ..Options::default()
        };
        let elements = parse("Toradora!.mkv", &options);
        assert_eq!(elements.get(ElementKind::FileExtension), None);
        assert_eq!(elements.get(ElementKind::FileName), Some("Toradora!.mkv"));
    }

    #[test]
    fn unknown_extension_stays() {
        let elements = parse("Toradora!.xyz", &Options::default());
        assert_eq!(elements.get(ElementKind::FileExtension), None);
        assert_eq!(elements.get(ElementKind::FileName), Some("Toradora!.xyz"));
    }

    #[test]
    fn ignored_strings_are_removed_before_tokenization() {
        let options = Options {
            ignored_strings: vec!["Enigma".to_owned()],
            ..Options::default()
        };
        let elements = parse("[EnigmaBD 1080p].mkv", &options);
        assert_eq!(elements.get(ElementKind::FileName), Some("[BD 1080p]"));
        assert_eq!(elements.get(ElementKind::Source), Some("BD"));
    }
}
