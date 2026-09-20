//! The word-level cursor the shapes share, and the two reads that have to
//! rewind when they fail.

use super::Part;

pub(super) struct Scanner<'a> {
    text: &'a str,
    /// Byte offset of every char, then the text's length.
    starts: Vec<usize>,
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub(super) fn new(text: &'a str) -> Scanner<'a> {
        Scanner {
            text,
            starts: text
                .char_indices()
                .map(|(offset, _)| offset)
                .chain(std::iter::once(text.len()))
                .collect(),
            pos: 0,
        }
    }

    pub(super) fn peek(&self) -> Option<char> {
        self.text[self.starts[self.pos]..].chars().next()
    }

    pub(super) fn offset(&self) -> usize {
        self.starts[self.pos]
    }

    pub(super) fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub(super) fn eat_any(&mut self, options: &[char]) -> bool {
        if self.peek().is_some_and(|c| options.contains(&c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub(super) fn digits(&mut self, max: usize) -> Option<Part> {
        let start = self.pos;
        while self.pos - start < max && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        (self.pos > start).then(|| Part {
            text: self.text[self.starts[start]..self.starts[self.pos]].to_owned(),
            at: self.starts[start]..self.starts[self.pos],
        })
    }

    pub(super) fn done(&self) -> bool {
        self.pos + 1 == self.starts.len()
    }

    /// Runs `parse` and rewinds if it fails, so a partial match leaves the
    /// cursor where it started.
    pub(super) fn attempt<T>(
        &mut self,
        parse: impl FnOnce(&mut Scanner<'a>) -> Option<T>,
    ) -> Option<T> {
        let saved = self.pos;
        let parsed = parse(self);
        if parsed.is_none() {
            self.pos = saved;
        }
        parsed
    }
}

/// `v2`: the version digit, spanning the `v` too so nothing of the suffix
/// is left for a title.
pub(super) fn version(scanner: &mut Scanner) -> Option<Part> {
    scanner.attempt(|scanner| {
        let start = scanner.offset();
        let digit = scanner
            .eat_any(&['v', 'V'])
            .then(|| scanner.digits(1))
            .flatten()?;
        Some(Part {
            text: digit.text,
            at: start..digit.at.end,
        })
    })
}

pub(super) fn eat_episode_separator(scanner: &mut Scanner) -> bool {
    // A colon separates the two only in a title a person wrote (`S1:E1`); no
    // release name spells it that way, and a colon never sits inside a word.
    let separated = scanner
        .attempt(|scanner| {
            (scanner.eat_any(&[' ', '.', '_', '-', ':', 'x', 'X']) && scanner.eat_any(&['E', 'e']))
                .then_some(())
        })
        .is_some();
    separated || scanner.eat_any(&['E', 'e', 'x', 'X'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_carry_byte_offsets_past_multibyte_chars() {
        let mut scanner = Scanner::new("第12話");
        assert!(scanner.eat('第'));
        let digits = scanner.digits(4).unwrap();
        assert_eq!(digits.text, "12");
        assert_eq!(digits.at, 3..5);
        assert!(scanner.eat('話'));
        assert!(scanner.done());
    }

    #[test]
    fn a_version_spans_its_letter() {
        let mut scanner = Scanner::new("v2");
        let part = version(&mut scanner).unwrap();
        assert_eq!(part.text, "2");
        assert_eq!(part.at, 0..2);
        let mut scanner = Scanner::new("vx");
        assert!(version(&mut scanner).is_none());
        assert_eq!(scanner.offset(), 0);
    }
}
