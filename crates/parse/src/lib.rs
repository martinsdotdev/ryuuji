//! Anime filename parser. An anitomy-style element extractor that follows
//! anitomy's rules where the corpus agrees with them and its own where it
//! does not. [`parse`] splits a filename into tokens and runs a table of
//! rules over them; the [`Reading`] it returns holds every element read,
//! typed readers for what matching needs with the certainty behind each,
//! and the alternatives a guess or a retraction left behind.

mod element;
mod engine;
mod keyword;
mod numbering;
mod options;
mod reading;
mod rules;
mod string;
mod token;
mod tokenizer;

pub use element::{ElementKind, Elements, Fact, Span};
pub use engine::RuleName;
pub use options::Options;
pub use reading::{Alternative, Certainty, Claim, Reading, Sense};

/// Reads one filename or player title.
pub fn parse(input: &str, options: &Options) -> Reading {
    parse_until(input, options, None)
}

/// The prelude (extension, ignored strings, file name), the tokenizer, then
/// the rule table up to and including `until`, or all of it.
fn parse_until(input: &str, options: &Options, until: Option<RuleName>) -> Reading {
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
            elements.push(Fact {
                kind: ElementKind::FileExtension,
                value: extension,
                span: Span {
                    start: dot + 1,
                    end: text.len(),
                },
                by: RuleName::Prelude,
                certainty: Certainty::Stated,
            });
            text.truncate(dot);
        }
    }

    let origin = remove_ignored(&mut text, &options.ignored_strings);

    let mut tape = token::Tape::new(tokenizer::tokenize(&text, options, table));
    if let Some(origin) = origin {
        tape = tape.with_origin(origin);
    }
    let name_end = tape.tokens.last().map_or(0, |token| token.span.end);
    elements.push(Fact {
        kind: ElementKind::FileName,
        value: text,
        span: Span {
            start: 0,
            end: name_end,
        },
        by: RuleName::Prelude,
        certainty: Certainty::Stated,
    });
    engine::run(tape, elements, options, until)
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

    fn elements(input: &str, options: &Options) -> Elements {
        parse(input, options).elements().clone()
    }

    #[test]
    fn known_extension_is_stripped_with_original_case() {
        let elements = elements("Toradora!.MKV", &Options::default());
        assert_eq!(elements.get(ElementKind::FileExtension), Some("MKV"));
        assert_eq!(elements.get(ElementKind::FileName), Some("Toradora!"));
    }

    #[test]
    fn extension_stays_when_parsing_is_disabled() {
        let options = Options {
            parse_file_extension: false,
            ..Options::default()
        };
        let elements = elements("Toradora!.mkv", &options);
        assert_eq!(elements.get(ElementKind::FileExtension), None);
        assert_eq!(elements.get(ElementKind::FileName), Some("Toradora!.mkv"));
    }

    #[test]
    fn unknown_extension_stays() {
        let elements = elements("Toradora!.xyz", &Options::default());
        assert_eq!(elements.get(ElementKind::FileExtension), None);
        assert_eq!(elements.get(ElementKind::FileName), Some("Toradora!.xyz"));
    }

    #[test]
    fn a_name_for_something_beside_an_episode_says_so() {
        for input in [
            "Daemons of the Shadow Realm - EP24 Preview",
            "Solo Leveling | DUB TRAILER",
            "Tetsuo faces the state of the Earth - EP 1 Highlights | SNOWBALL EARTH",
            "Velvet and Sapphire Save the Day | THE RIBBON HERO | Clip | Netflix Anime",
            "A Reunion and a Reckoning. | Detective Conan ep 0259 shorts",
        ] {
            assert!(
                parse(input, &Options::default()).extra().is_some(),
                "{input:?}"
            );
        }
    }

    // Han runs with no word breaks, so these reach no rule as a token of
    // their own; only a pre-identified entry can cut them out. 次回予告 is
    // 予告 with "next time" in front of it, and the prefix match is what
    // catches both.
    #[test]
    fn a_han_word_for_something_beside_an_episode_says_so() {
        for input in [
            "『葬送のフリーレン』第1話「冒険の終わり」次回予告",
            "『薬屋のひとりごと』第1話「猫猫」予告",
            "2026年4月新番《黒貓與魔女的教室》預告【Ani-One Asia】",
            "《相反的你和我 第二季》第22話｜精華重溫【Ani-One Asia】",
        ] {
            assert!(
                parse(input, &Options::default()).extra().is_some(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn an_episode_of_a_show_says_nothing_of_the_kind() {
        for input in [
            "[TaigaSubs]_Toradora!_(2008)_-_01v2_-_Tiger_and_Dragon_[1280x720].mkv",
            "BLACK TORCH - Episode 12 [English Sub]",
            "《幼女戰記 2》#11 (繁中字幕 | 日語原聲)【Ani-One Asia】",
        ] {
            assert_eq!(parse(input, &Options::default()).extra(), None, "{input:?}");
        }
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

    fn span_text<'a>(input: &'a str, ignored: &str, kind: ElementKind) -> &'a str {
        let options = Options {
            ignored_strings: vec![ignored.to_owned()],
            ..Options::default()
        };
        let reading = parse(input, &options);
        let fact = reading
            .elements()
            .fact(kind)
            .unwrap_or_else(|| panic!("no {} in {input:?}", kind.label()));
        fact.span.slice(input)
    }

    #[test]
    fn a_string_cut_from_inside_a_word_leaves_its_parts_on_their_own_bytes() {
        let input = "Show - 05Enigmav2.mkv";
        assert_eq!(span_text(input, "Enigma", ElementKind::EpisodeNumber), "05");
        assert_eq!(
            span_text(input, "Enigma", ElementKind::ReleaseVersion),
            "v2"
        );
    }

    #[test]
    fn a_string_cut_right_after_a_word_stays_out_of_its_span() {
        let input = "[BDEnigma 1080p].mkv";
        assert_eq!(span_text(input, "Enigma", ElementKind::Source), "BD");
    }

    #[test]
    fn a_multibyte_string_cut_from_a_word_keeps_spans_on_char_boundaries() {
        let input = "Show - 0\u{f6}5v2.mkv";
        assert_eq!(
            span_text(input, "\u{f6}", ElementKind::EpisodeNumber),
            "0\u{f6}5"
        );
        assert_eq!(
            span_text(input, "\u{f6}", ElementKind::ReleaseVersion),
            "v2"
        );
    }

    #[test]
    fn ignored_strings_are_removed_before_tokenization() {
        let options = Options {
            ignored_strings: vec!["Enigma".to_owned()],
            ..Options::default()
        };
        let elements = elements("[EnigmaBD 1080p].mkv", &options);
        assert_eq!(elements.get(ElementKind::FileName), Some("[BD 1080p]"));
        assert_eq!(elements.get(ElementKind::Source), Some("BD"));
    }
}
