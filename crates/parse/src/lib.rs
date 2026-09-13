//! Anime filename parser. An anitomy-style element extractor that follows
//! anitomy's rules where the corpus agrees with them and its own where it
//! does not: [`parse`] splits a filename into tokens and identifies known
//! elements (resolution, source, audio and video terms, checksum, ...).

mod element;
mod keyword;
mod options;
mod parser;
mod string;
mod token;
mod tokenizer;

pub use element::{ElementKind, Elements, Span};
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

    let origin = remove_ignored(&mut text, &options.ignored_strings);

    elements.insert(ElementKind::FileName, text.clone());
    let mut tokens = tokenizer::tokenize(&text, options, table);
    if let Some(origin) = origin {
        for token in &mut tokens {
            token.span = Span {
                start: origin[token.span.start],
                end: origin[token.span.end],
            };
        }
    }
    parser::Parser::new(tokens, elements, options, table).run()
}

/// Removes every ignored string from `text`. When one was found, returns the
/// byte offset in the caller's input of every byte of the shortened text and
/// of its end, so token spans can be mapped back; spans need no mapping
/// otherwise.
fn remove_ignored(text: &mut String, ignored: &[String]) -> Option<Vec<usize>> {
    let mut origin: Option<Vec<usize>> = None;
    for needle in ignored.iter().filter(|needle| !needle.is_empty()) {
        let mut from = 0;
        while let Some(found) = text[from..].find(needle.as_str()) {
            let at = from + found;
            let end = at + needle.len();
            origin
                .get_or_insert_with(|| (0..=text.len()).collect())
                .drain(at..end);
            text.replace_range(at..end, "");
            from = at;
        }
    }
    origin
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
    fn removing_an_ignored_string_maps_the_rest_back_to_the_input() {
        let input = "[EnigmaBD 1080p]";
        let mut text = input.to_owned();
        let origin = remove_ignored(&mut text, &["Enigma".to_owned()]).unwrap();
        assert_eq!(text, "[BD 1080p]");
        assert_eq!(&input[origin[1]..origin[3]], "BD");
        assert_eq!(&input[origin[4]..origin[9]], "1080p");
        assert_eq!(origin[text.len()], input.len());
        assert_eq!(remove_ignored(&mut text, &["Enigma".to_owned()]), None);
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
