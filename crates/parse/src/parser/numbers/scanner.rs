//! The word-level cursor the episode and volume matchers share, and the two
//! reads that have to rewind when they fail.

pub(super) struct Scanner<'a> {
    chars: &'a [char],
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub(super) fn new(chars: &'a [char]) -> Scanner<'a> {
        Scanner { chars, pos: 0 }
    }

    pub(super) fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
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

    pub(super) fn digits(&mut self, max: usize) -> Option<String> {
        let start = self.pos;
        while self.pos - start < max && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        (self.pos > start).then(|| self.chars[start..self.pos].iter().collect())
    }

    pub(super) fn done(&self) -> bool {
        self.pos == self.chars.len()
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

/// A digit run that overflows `u32` compares as larger than every bound, so
/// the validation checks reject it.
pub(super) fn version(scanner: &mut Scanner) -> Option<String> {
    scanner.attempt(|scanner| {
        scanner
            .eat_any(&['v', 'V'])
            .then(|| scanner.digits(1))
            .flatten()
    })
}
pub(super) fn eat_episode_separator(scanner: &mut Scanner) -> bool {
    let separated = scanner
        .attempt(|scanner| {
            (scanner.eat_any(&[' ', '.', '_', '-', 'x', 'X']) && scanner.eat_any(&['E', 'e']))
                .then_some(())
        })
        .is_some();
    separated || scanner.eat_any(&['E', 'e', 'x', 'X'])
}
