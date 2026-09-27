//! Terminology a translator must honour, and the notes that describe a work.
//!
//! A general-purpose model translates every balloon as if it had never seen the
//! book: it does not know who the characters are, how they address each other,
//! or which of several valid renderings the reader already met on page one. The
//! glossary fixes those choices once and hands the translator only the entries
//! that occur in the text it is about to translate, so a long list never crowds
//! out the page itself.

use std::path::Path;

/// One fixed rendering: `source` is always translated as `target`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlossaryEntry {
    pub source: String,
    pub target: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Glossary {
    pub entries: Vec<GlossaryEntry>,
}

impl Glossary {
    /// Reads a tab-separated file: `source<TAB>target[<TAB>note]`.
    ///
    /// Lines starting with `#` and lines without a target are ignored. A
    /// missing file is an empty glossary rather than an error, because every
    /// work starts without one.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .map(|line| line.trim_start_matches('\u{feff}'))
            .filter(|line| !line.trim_start().starts_with('#'))
            .filter_map(|line| {
                let mut fields = line.split('\t').map(str::trim);
                let source = fields.next()?.to_owned();
                let target = fields.next()?.to_owned();
                let note = fields.next().unwrap_or_default().to_owned();
                (!source.is_empty() && !target.is_empty()).then_some(GlossaryEntry {
                    source,
                    target,
                    note,
                })
            })
            .collect();
        Self { entries }
    }

    /// Serializes back to the file format, one entry per line.
    #[must_use]
    pub fn to_tsv(&self) -> String {
        self.entries
            .iter()
            .map(|entry| {
                if entry.note.is_empty() {
                    format!("{}\t{}\n", entry.source, entry.target)
                } else {
                    format!("{}\t{}\t{}\n", entry.source, entry.target, entry.note)
                }
            })
            .collect()
    }

    /// Adds `other`'s entries; on a repeated source term the later one wins,
    /// so a work's own glossary overrides the global one.
    pub fn merge(&mut self, other: Glossary) {
        for entry in other.entries {
            let key = entry.source.to_lowercase();
            self.entries
                .retain(|existing| existing.source.to_lowercase() != key);
            self.entries.push(entry);
        }
    }

    #[must_use]
    pub fn contains(&self, source: &str) -> bool {
        let key = source.trim().to_lowercase();
        self.entries
            .iter()
            .any(|entry| entry.source.to_lowercase() == key)
    }

    /// The entries whose source term occurs in any of `texts`.
    #[must_use]
    pub fn relevant<'a>(&'a self, texts: &[&str]) -> Vec<&'a GlossaryEntry> {
        let lowered: Vec<String> = texts.iter().map(|text| text.to_lowercase()).collect();
        self.entries
            .iter()
            .filter(|entry| {
                let term = entry.source.to_lowercase();
                lowered.iter().any(|text| occurs(text, &term))
            })
            .collect()
    }
}

/// Whether `term` appears in `text` as a word of its own.
///
/// Latin terms must not match inside longer words ("ass" in "class"), but may
/// carry an English plural ending ("cocks"). Scripts written without spaces
/// have no word boundaries to check, so any occurrence counts.
fn occurs(text: &str, term: &str) -> bool {
    if term.is_empty() {
        return false;
    }
    let spaced = term.chars().any(|c| c.is_ascii_alphabetic());
    let mut from = 0;
    while let Some(found) = text[from..].find(term) {
        let start = from + found;
        let end = start + term.len();
        if !spaced {
            return true;
        }
        let before = text[..start].chars().next_back();
        let rest = &text[end..];
        let rest = rest
            .strip_prefix("es")
            .filter(|tail| !starts_word(tail))
            .or_else(|| rest.strip_prefix('s').filter(|tail| !starts_word(tail)))
            .unwrap_or(rest);
        if !before.is_some_and(char::is_alphanumeric) && !starts_word(rest) {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn starts_word(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_alphanumeric)
}

/// Builds the block appended to a translator's or corrector's instructions:
/// the notes on the work, then the terms that occur in `texts`.
#[must_use]
pub fn instructions_block(notes: Option<&str>, glossary: &Glossary, texts: &[&str]) -> String {
    let mut block = String::new();
    if let Some(notes) = notes.map(str::trim).filter(|notes| !notes.is_empty()) {
        block.push_str(
            "FICHA DE LA OBRA\n\
             Describe los personajes, sus relaciones y el tono. Úsala para saber \
             quién habla, a quién se dirige y con qué registro; no la traduzcas.\n\n",
        );
        block.push_str(notes);
    }
    let terms = glossary.relevant(texts);
    if !terms.is_empty() {
        if !block.is_empty() {
            block.push_str("\n\n");
        }
        block.push_str(
            "GLOSARIO OBLIGATORIO\n\
             Estos términos aparecen en el texto. Tradúcelos SIEMPRE exactamente así, \
             ajustando solo género y número:\n",
        );
        for entry in terms {
            block.push_str(&format!("- \"{}\" -> \"{}\"", entry.source, entry.target));
            if !entry.note.is_empty() {
                block.push_str(&format!(" ({})", entry.note));
            }
            block.push('\n');
        }
    }
    block.trim_end().to_owned()
}

/// At most this many slang entries reach the model per page.
const SLANG_PER_PAGE: usize = 30;

/// What the sexual or vulgar words of the original mean, for the words that
/// occur in `texts`. Unlike the glossary these are not fixed renderings: the
/// translator picks the Spanish word that fits the work's register.
pub fn slang_block(slang: &Glossary, texts: &[&str]) -> String {
    let mut terms = slang.relevant(texts);
    if terms.is_empty() {
        return String::new();
    }
    // Longer terms say more ("中出し" over "出し"); keep those when trimming.
    terms.sort_by_key(|entry| std::cmp::Reverse(entry.source.chars().count()));
    terms.truncate(SLANG_PER_PAGE);
    let mut block = String::from(
        "JERGA DEL ORIGINAL\n\
         Significado de palabras sexuales o vulgares que aparecen en el texto. No es una \
         traducción obligatoria: úsalo para entender el sentido y elige la palabra según \
         el registro de la ficha. Si una palabra aparece solo como parte de otra, ignórala.\n",
    );
    for entry in terms {
        block.push_str(&format!("- \"{}\": {}", entry.source, entry.target));
        if !entry.note.is_empty() {
            block.push_str(&format!(" ({})", entry.note));
        }
        block.push('\n');
    }
    block.trim_end().to_owned()
}

/// The study of one page, framed for the translator or the corrector.
pub fn page_block(note: Option<&str>) -> String {
    match note.map(str::trim).filter(|note| !note.is_empty()) {
        Some(note) => format!(
            "CONTEXTO DE ESTA PÁGINA\n\
             Un editor miró la página: qué ocurre y quién dice cada globo. Úsalo \
             para el sentido, el sujeto omitido y el género gramatical; no lo traduzcas.\n\n\
             {note}"
        ),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn slang_reaches_the_model_only_when_it_occurs() {
        let slang = super::Glossary::parse("中出し\teyacular dentro\tvulgar\nおっぱい\ttetas\t\n");
        let block = super::slang_block(&slang, &["もう中出しして"]);
        assert!(block.contains("\"中出し\": eyacular dentro (vulgar)"));
        assert!(!block.contains("おっぱい"));
        assert!(super::slang_block(&slang, &["こんにちは"]).is_empty());
    }

    use super::*;

    #[test]
    fn parses_tabs_comments_and_notes() {
        let glossary = Glossary::parse("# comentario\ncock\tverga\ncum\tvenirse\tverbo\nsolo\n");
        assert_eq!(glossary.entries.len(), 2);
        assert_eq!(glossary.entries[1].note, "verbo");
    }

    #[test]
    fn matches_whole_words_and_plurals() {
        let glossary = Glossary::parse("ass\tculo\ncock\tverga\n");
        assert!(glossary.relevant(&["This class is boring"]).is_empty());
        assert_eq!(glossary.relevant(&["Two COCKS!"]).len(), 1);
        assert_eq!(glossary.relevant(&["nice ass."]).len(), 1);
    }

    #[test]
    fn matches_scripts_without_spaces() {
        let glossary = Glossary::parse("お兄ちゃん\tonii-chan\n");
        assert_eq!(glossary.relevant(&["お兄ちゃんだめ"]).len(), 1);
    }

    #[test]
    fn later_entries_override_earlier_ones() {
        let mut global = Glossary::parse("big sis\thermana mayor\n");
        global.merge(Glossary::parse("Big Sis\tnee-chan\n"));
        assert_eq!(global.entries.len(), 1);
        assert_eq!(global.entries[0].target, "nee-chan");
    }

    #[test]
    fn block_lists_only_terms_present() {
        let glossary = Glossary::parse("cock\tverga\npussy\tcoño\n");
        let block = instructions_block(Some("Kenta: hijo"), &glossary, &["my cock"]);
        assert!(block.contains("FICHA DE LA OBRA"));
        assert!(block.contains("\"cock\" -> \"verga\""));
        assert!(!block.contains("pussy"));
    }
}
