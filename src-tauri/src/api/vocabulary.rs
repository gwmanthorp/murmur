//! Custom vocabulary: user-supplied names, jargon, and product terms that ASR
//! keeps getting wrong.
//!
//! One entry per line (commas and semicolons also separate). Two forms:
//!   `Manthorp`              — a term: biases Whisper and is pinned as the
//!                             canonical spelling during cleanup.
//!   `man thorpe -> Manthorp` — a correction: applied deterministically to the
//!                             transcript, and its target is also a term.
//!
//! Corrections run before cleanup so the LLM only ever sees correct spellings.

/// A `misheard -> correct` mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correction {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vocabulary {
    /// Canonical spellings, including every correction target.
    pub terms: Vec<String>,
    pub corrections: Vec<Correction>,
}

/// Whisper's `prompt` is capped near 224 tokens; stay well inside it.
const MAX_PROMPT_CHARS: usize = 800;

const ARROWS: &[&str] = &["->", "→", "=>"];

impl Vocabulary {
    /// Decoding hint for the transcription endpoint. Empty when there are no
    /// terms, in which case the caller must omit the field entirely.
    pub fn whisper_prompt(&self) -> String {
        let mut prompt = String::new();
        for term in &self.terms {
            let addition = if prompt.is_empty() {
                term.len()
            } else {
                term.len() + 2
            };
            if prompt.len() + addition > MAX_PROMPT_CHARS {
                break;
            }
            if !prompt.is_empty() {
                prompt.push_str(", ");
            }
            prompt.push_str(term);
        }
        prompt
    }

    /// Apply every correction to `text`, longest `from` first so a specific
    /// mapping wins over a shorter one that is a prefix of it.
    pub fn apply_corrections(&self, text: &str) -> String {
        if self.corrections.is_empty() || text.is_empty() {
            return text.to_string();
        }
        let mut ordered: Vec<&Correction> = self.corrections.iter().collect();
        ordered.sort_by_key(|c| std::cmp::Reverse(c.from.chars().count()));
        let mut result = text.to_string();
        for correction in ordered {
            result = replace_word_ci(&result, &correction.from, &correction.to);
        }
        result
    }
}

/// Split raw settings input into terms and corrections, dropping
/// case-insensitive duplicates (supersedes FreeFlow's mergedVocabularyTerms).
pub fn parse(raw: &str) -> Vocabulary {
    let mut vocabulary = Vocabulary::default();
    let mut seen_terms = std::collections::HashSet::new();
    let mut seen_corrections = std::collections::HashSet::new();

    let mut push_term = |vocabulary: &mut Vocabulary, term: &str| {
        if !term.is_empty() && seen_terms.insert(term.to_lowercase()) {
            vocabulary.terms.push(term.to_string());
        }
    };

    for entry in raw.split(['\n', ',', ';']).map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        match split_correction(entry) {
            // A correction target is also a term worth hinting to Whisper.
            Some((from, to)) => {
                if seen_corrections.insert(from.to_lowercase()) {
                    vocabulary.corrections.push(Correction {
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                }
                push_term(&mut vocabulary, to);
            }
            None => push_term(&mut vocabulary, entry),
        }
    }
    vocabulary
}

/// Split `from -> to` on the first arrow. Returns None for a plain term, or
/// when either side is blank (`-> x`, `x ->`) so typos degrade to terms.
fn split_correction(entry: &str) -> Option<(&str, &str)> {
    let (index, arrow) = ARROWS
        .iter()
        .filter_map(|arrow| entry.find(arrow).map(|i| (i, *arrow)))
        .min_by_key(|(i, _)| *i)?;
    let from = entry[..index].trim();
    let to = entry[index + arrow.len()..].trim();
    (!from.is_empty() && !to.is_empty()).then_some((from, to))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn chars_eq_ci(a: char, b: char) -> bool {
    // Whitespace in a multi-word term matches any whitespace run of one.
    a == b || (a.is_whitespace() && b.is_whitespace()) || a.to_lowercase().eq(b.to_lowercase())
}

/// Byte index just past a case-insensitive match of `needle` at `start`.
fn match_at(hay: &str, start: usize, needle: &str) -> Option<usize> {
    let mut chars = hay[start..].char_indices();
    let mut end = start;
    for needle_char in needle.chars() {
        let (offset, hay_char) = chars.next()?;
        if !chars_eq_ci(hay_char, needle_char) {
            return None;
        }
        end = start + offset + hay_char.len_utf8();
    }
    Some(end)
}

/// Case-insensitive whole-word replace. Word boundaries keep "man" from
/// rewriting the middle of "manual".
fn replace_word_ci(hay: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return hay.to_string();
    }
    let mut out = String::with_capacity(hay.len());
    let mut index = 0;
    while index < hay.len() {
        if let Some(end) = match_at(hay, index, needle) {
            let boundary_before = hay[..index]
                .chars()
                .next_back()
                .is_none_or(|c| !is_word_char(c));
            let boundary_after = hay[end..].chars().next().is_none_or(|c| !is_word_char(c));
            if boundary_before && boundary_after {
                out.push_str(replacement);
                index = end;
                continue;
            }
        }
        let next = hay[index..].chars().next().expect("index on a boundary");
        out.push(next);
        index += next.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(raw: &str) -> Vec<String> {
        parse(raw).terms
    }

    #[test]
    fn dedupes_terms_case_insensitively() {
        assert_eq!(
            terms("Groq, tauri\nGROQ; WASAPI ,,  "),
            vec!["Groq", "tauri", "WASAPI"]
        );
    }

    #[test]
    fn parses_corrections_and_registers_their_targets() {
        let vocabulary = parse("Groq\nman thorpe -> Manthorp\ntowery => Tauri\nfoo → Bar");
        assert_eq!(vocabulary.terms, vec!["Groq", "Manthorp", "Tauri", "Bar"]);
        assert_eq!(
            vocabulary.corrections,
            vec![
                Correction {
                    from: "man thorpe".into(),
                    to: "Manthorp".into()
                },
                Correction {
                    from: "towery".into(),
                    to: "Tauri".into()
                },
                Correction {
                    from: "foo".into(),
                    to: "Bar".into()
                },
            ]
        );
    }

    #[test]
    fn half_written_arrows_fall_back_to_terms() {
        let vocabulary = parse("-> Manthorp\nGroq ->");
        assert!(vocabulary.corrections.is_empty());
        assert_eq!(vocabulary.terms, vec!["-> Manthorp", "Groq ->"]);
    }

    #[test]
    fn duplicate_sources_keep_the_first_mapping() {
        let vocabulary = parse("towery -> Tauri\nTOWERY -> Torrey");
        assert_eq!(vocabulary.corrections.len(), 1);
        assert_eq!(vocabulary.corrections[0].to, "Tauri");
    }

    #[test]
    fn corrections_respect_word_boundaries() {
        let vocabulary = parse("man -> Manthorp");
        assert_eq!(
            vocabulary.apply_corrections("the manual man"),
            "the manual Manthorp"
        );
        assert_eq!(
            vocabulary.apply_corrections("Man, hello"),
            "Manthorp, hello"
        );
    }

    #[test]
    fn corrections_are_case_insensitive_and_multi_word() {
        let vocabulary = parse("man thorpe -> Manthorp");
        assert_eq!(
            vocabulary.apply_corrections("Hi MAN THORPE and man thorpe."),
            "Hi Manthorp and Manthorp."
        );
    }

    #[test]
    fn longest_source_wins() {
        let vocabulary = parse("man -> Person\nman thorpe -> Manthorp");
        assert_eq!(vocabulary.apply_corrections("man thorpe"), "Manthorp");
    }

    #[test]
    fn corrections_do_not_cascade_into_each_other() {
        // "Tauri" must not be re-matched by a later rule targeting its source.
        let vocabulary = parse("towery -> Tauri");
        assert_eq!(vocabulary.apply_corrections("towery app"), "Tauri app");
        assert_eq!(
            vocabulary.apply_corrections("no match here"),
            "no match here"
        );
    }

    #[test]
    fn empty_vocabulary_is_a_passthrough() {
        let vocabulary = parse("  \n , ; ");
        assert_eq!(vocabulary, Vocabulary::default());
        assert_eq!(vocabulary.apply_corrections("untouched"), "untouched");
        assert_eq!(vocabulary.whisper_prompt(), "");
    }

    #[test]
    fn whisper_prompt_joins_and_caps_terms() {
        let vocabulary = parse("Groq\nWASAPI\nDPAPI");
        assert_eq!(vocabulary.whisper_prompt(), "Groq, WASAPI, DPAPI");

        let long: String = (0..200)
            .map(|i| format!("term{i:04}\n"))
            .collect::<String>();
        let prompt = parse(&long).whisper_prompt();
        assert!(prompt.len() <= MAX_PROMPT_CHARS);
        assert!(prompt.starts_with("term0000, term0001"));
    }

    #[test]
    fn non_ascii_terms_survive_replacement() {
        let vocabulary = parse("mama -> Mămăligă");
        assert_eq!(vocabulary.apply_corrections("the mama"), "the Mămăligă");
        // Already-correct text is left alone rather than re-matched.
        assert_eq!(vocabulary.apply_corrections("Mămăligă"), "Mămăligă");
    }
}
