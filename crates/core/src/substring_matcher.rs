//! Streaming equivalent of `haystack.to_lowercase().contains(query.to_lowercase())`.
//!
//! The caller must bound the query and feed the entire logical row, including its
//! separator, in order. State grows only with the lowered query, never the row.

#[derive(Clone, Copy)]
struct MatchState {
    index: usize,
    matched: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Cased,
    Ignorable,
    Uncased,
}

// Probe the same string lowercase implementation as the predicate. Each probe
// contains four scalars, including at most one variable-width scalar. Skipping
// ignorable characters before testing Cased also handles overlapping properties.
fn context(c: char) -> Context {
    let probe = |suffix: char| {
        format!("AΣ{c}{suffix}")
            .to_lowercase()
            .chars()
            .nth(1)
            .expect("lowercasing retains the probe's first two scalars")
    };
    match (probe('A'), probe(' ')) {
        ('σ', 'σ') => Context::Cased,
        ('σ', 'ς') => Context::Ignorable,
        ('ς', 'ς') => Context::Uncased,
        pair => panic!("unexpected lowercase sigma probe: {pair:?}"),
    }
}

pub(crate) struct SubstringMatcher {
    pattern: Vec<u8>,
    prefix: Vec<usize>,
    current: MatchState,
    // Alternative states after a sigma with unresolved following context:
    // first is medial σ, second is final ς.
    pending: Option<(MatchState, MatchState)>,
    preceding_cased: bool,
}

impl SubstringMatcher {
    pub(crate) fn new(query: &str) -> Self {
        let pattern = query.to_lowercase().into_bytes();
        let mut prefix = vec![0; pattern.len()];
        let mut k = 0;
        for i in 1..pattern.len() {
            while k > 0 && pattern[k] != pattern[i] {
                k = prefix[k - 1];
            }
            if pattern[k] == pattern[i] {
                k += 1;
            }
            prefix[i] = k;
        }
        let matched = pattern.is_empty();
        Self {
            pattern,
            prefix,
            current: MatchState { index: 0, matched },
            pending: None,
            preceding_cased: false,
        }
    }

    fn advance(pattern: &[u8], prefix: &[usize], state: &mut MatchState, c: char) {
        if state.matched {
            return;
        }
        let mut buffer = [0; 4];
        for &byte in c.encode_utf8(&mut buffer).as_bytes() {
            while state.index > 0 && pattern[state.index] != byte {
                state.index = prefix[state.index - 1];
            }
            if pattern[state.index] == byte {
                state.index += 1;
                if state.index == pattern.len() {
                    state.matched = true;
                    return;
                }
            }
        }
    }

    /// Feed one complete Unicode scalar; calls may be split at any scalar boundary.
    pub(crate) fn feed(&mut self, c: char) {
        let class = context(c);
        if c == 'Σ' {
            assert!(class == Context::Cased, "uppercase sigma must be cased");
        }
        if let Some((medial, final_sigma)) = self.pending.take() {
            if class == Context::Ignorable {
                self.pending = Some((medial, final_sigma));
            } else {
                self.current = if class == Context::Cased {
                    medial
                } else {
                    final_sigma
                };
            }
        }
        if c == 'Σ' && self.preceding_cased {
            let mut medial = self.current;
            let mut final_sigma = self.current;
            Self::advance(&self.pattern, &self.prefix, &mut medial, 'σ');
            Self::advance(&self.pattern, &self.prefix, &mut final_sigma, 'ς');
            self.pending = Some((medial, final_sigma));
        } else {
            for lower in c.to_lowercase() {
                if let Some((ref mut medial, ref mut final_sigma)) = self.pending {
                    Self::advance(&self.pattern, &self.prefix, medial, lower);
                    Self::advance(&self.pattern, &self.prefix, final_sigma, lower);
                } else {
                    Self::advance(&self.pattern, &self.prefix, &mut self.current, lower);
                }
            }
        }
        match class {
            Context::Cased => self.preceding_cased = true,
            Context::Uncased => self.preceding_cased = false,
            Context::Ignorable => {}
        }
    }

    /// True only if every still-possible lowercase stream has matched.
    #[cfg(test)]
    pub(crate) fn is_match(&self) -> bool {
        match self.pending {
            Some((medial, final_sigma)) => medial.matched && final_sigma.matched,
            None => self.current.matched,
        }
    }

    /// Resolve any trailing sigma at the true end of the logical row.
    pub(crate) fn finish(self) -> bool {
        self.pending
            .map_or(self.current.matched, |(_, final_sigma)| final_sigma.matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(row: &str, query: &str, width: usize) {
        let expected = row.to_lowercase().contains(&query.to_lowercase());
        let mut matcher = SubstringMatcher::new(query);
        let scalars: Vec<_> = row.chars().collect();
        for chunk in scalars.chunks(width) {
            for &c in chunk {
                matcher.feed(c);
                if matcher.is_match() {
                    assert!(expected, "premature match: {row:?} {query:?}");
                }
            }
        }
        assert_eq!(
            matcher.finish(),
            expected,
            "row={row:?} query={query:?} width={width}"
        );
    }

    #[test]
    fn streaming_substring_cases() {
        let rows = [
            "",
            "Hello, WORLD!\0",
            "İSTANBUL",
            "AΣ",
            "AΣA",
            "AΣ\u{301}A",
            "AΣ\u{301} ",
            "AΣΣ",
            "AΣ\nA",
            "abababac",
            "AΣ\u{345}A",
        ];
        let queries = [
            "",
            "world",
            "!\0",
            "i\u{307}",
            "σ",
            "ς",
            "σ\u{301}",
            "ς\u{301}",
            "σς",
            "ς\n",
            "\nA",
            "ababac",
            "missing",
        ];
        for row in rows {
            for query in queries {
                for width in [1, 2, 7] {
                    check(row, query, width);
                }
            }
        }
        let row = format!("AΣ{} ", "\u{301}".repeat(20_000));
        check(&row, "ς\u{301}\u{301}", 31);
        check(&row, "σ\u{301}\u{301}", 31);
    }

    #[test]
    fn tentative_and_definite_branches() {
        let mut m = SubstringMatcher::new("σ");
        for c in "AΣ".chars() {
            m.feed(c);
        }
        assert!(!m.is_match());
        m.feed(' ');
        assert!(!m.finish());
        let mut m = SubstringMatcher::new("\u{301}");
        for c in "AΣ\u{301}".chars() {
            m.feed(c);
        }
        assert!(m.is_match());
        assert!(m.finish());
    }

    #[test]
    fn deterministic_adversarial_differential() {
        let alphabet = [
            'A', 'Σ', 'σ', 'ς', '\u{301}', '\u{345}', 'İ', '\0', '\n', ' ', 'a', 'b', '\u{200d}',
        ];
        let mut seed = 0x9e37_79b9_u64;
        for _ in 0..1200 {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            let row: String = (0..(next() % 35))
                .map(|_| {
                    alphabet
                        [usize::try_from(next() % u64::try_from(alphabet.len()).unwrap()).unwrap()]
                })
                .collect();
            let query: String = (0..(next() % 6))
                .map(|_| {
                    alphabet
                        [usize::try_from(next() % u64::try_from(alphabet.len()).unwrap()).unwrap()]
                })
                .collect();
            for width in [1, 3, 11] {
                check(&row, &query, width);
            }
        }
    }

    #[test]
    fn all_scalars_sigma_context_differential() {
        for value in 0..=0x0010_ffff {
            let Some(c) = char::from_u32(value) else {
                continue;
            };
            for row in [format!("AΣ{c}A"), format!("AΣ{c} "), format!("A{c}Σ ")] {
                check(&row, "ς", 4);
            }
        }
    }
}
