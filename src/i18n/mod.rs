//! Translations of the interface into English, Russian, German, French and Spanish.
//!
//! Texts are written in English in the code and marked with the macro [`tr!`] (a plain text), [`trf!`] (with `{name}`
//! placeholders) or [`trn!`] (a count: "1 target|{n} targets"); [`n!`] marks a text that is translated later, where
//! it is shown (menu labels kept in tables). The English text is the key into each language's table; a text missing
//! from a table shows in English. The language lives in the egui context ([`set`], [`get`]), so drawing code asks
//! the `ui` it draws into; code without a `ui` gets the [`Language`] passed in.

mod de;
mod es;
mod fr;
mod ru;

use std::collections::HashMap;
use std::sync::LazyLock;

use eframe::egui;

/// An interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Language {
    /// English: the texts as written in the code.
    #[default]
    English,
    /// Russian.
    Russian,
    /// German.
    German,
    /// French.
    French,
    /// Spanish.
    Spanish,
}

impl Language {
    /// All languages in menu order.
    pub const ALL: [Self; 5] = [Self::English, Self::Russian, Self::German, Self::French, Self::Spanish];

    /// Stable key for storing the choice.
    pub fn key(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian => "ru",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
        }
    }

    /// The language stored under `key`, if it is one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|language| language.key() == key)
    }

    /// The language's own name, as shown in the menu.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
            Self::German => "Deutsch",
            Self::French => "Français",
            Self::Spanish => "Español",
        }
    }

    /// The language Windows shows its own interface in, if it is one of these; English otherwise.
    pub fn of_windows() -> Self {
        // SAFETY: GetUserDefaultUILanguage takes no arguments, has no preconditions and only returns a value.
        let language_id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
        // The low ten bits are the primary language.
        match language_id & 0x3ff {
            0x19 => Self::Russian,
            0x07 => Self::German,
            0x0c => Self::French,
            0x0a => Self::Spanish,
            _ => Self::English,
        }
    }

    /// `text` (an English text marked in the code) in this language; the English text if there is no translation.
    pub fn translate(self, text: &'static str) -> &'static str {
        let table: &HashMap<&'static str, &'static str> = match self {
            Self::English => return text,
            Self::Russian => &RU,
            Self::German => &DE,
            Self::French => &FR,
            Self::Spanish => &ES,
        };
        table.get(text).copied().unwrap_or(text)
    }

    /// `value` with one decimal, with this language's decimal separator: "50.3" in English, "50,3" in the others.
    pub fn decimal(self, value: f64) -> String {
        let text = format!("{value:.1}");
        match self {
            Self::English => text,
            Self::Russian | Self::German | Self::French | Self::Spanish => text.replace('.', ","),
        }
    }

    /// The form of `forms` (separated by `|`, in this language's order) for the count `n`.
    pub fn plural_form(self, forms: &str, n: u64) -> String {
        let forms: Vec<&str> = forms.split('|').collect();
        let index = match self {
            Self::Russian => {
                let (last, last_two) = (n % 10, n % 100);
                if last == 1 && last_two != 11 {
                    0
                } else if (2..=4).contains(&last) && !(12..=14).contains(&last_two) {
                    1
                } else {
                    2
                }
            }
            Self::French => usize::from(n > 1),
            Self::English | Self::German | Self::Spanish => usize::from(n != 1),
        };
        forms.get(index).or(forms.last()).copied().unwrap_or_default().to_owned()
    }
}

/// Anything that knows the interface language: the egui context, a `Ui`, or the language itself.
pub trait Lang {
    /// The language to show texts in.
    fn language(&self) -> Language;
}

impl Lang for Language {
    fn language(&self) -> Language {
        *self
    }
}

impl Lang for egui::Context {
    fn language(&self) -> Language {
        get(self)
    }
}

impl Lang for egui::Ui {
    fn language(&self) -> Language {
        get(self.ctx())
    }
}

impl<T: Lang + ?Sized> Lang for &T {
    fn language(&self) -> Language {
        (**self).language()
    }
}

impl<T: Lang + ?Sized> Lang for &mut T {
    fn language(&self) -> Language {
        (**self).language()
    }
}

/// Where the language is kept in the egui context.
fn id() -> egui::Id {
    egui::Id::NULL.with("interface-language")
}

/// Makes `ctx` show texts in `language`.
pub fn set(ctx: &egui::Context, language: Language) {
    ctx.data_mut(|data| data.insert_temp(id(), language));
}

/// The language `ctx` shows texts in (English until [`set`]).
pub fn get(ctx: &egui::Context) -> Language {
    ctx.data(|data| data.get_temp(id())).unwrap_or_default()
}

/// `text` with every `{name}` replaced by its value from `values`.
pub fn fill(text: &str, values: &[(&str, String)]) -> String {
    let mut filled = text.to_owned();
    for (name, value) in values {
        filled = filled.replace(&format!("{{{name}}}"), value);
    }
    filled
}

/// A marked text in the language of `$lang` (a context, `Ui` or [`Language`]).
macro_rules! tr {
    ($lang:expr, $text:literal) => {
        $crate::i18n::Lang::language(&$lang).translate($text)
    };
}

/// A marked text with `{name}` placeholders, filled with `name = value` pairs.
macro_rules! trf {
    ($lang:expr, $text:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::fill(
            $crate::i18n::Lang::language(&$lang).translate($text),
            &[$((stringify!($name), $value.to_string())),+],
        )
    };
}

/// A marked count text: English "one|other" forms (each may use `{n}`), in the language's own number of forms.
macro_rules! trn {
    ($lang:expr, $count:expr, $text:literal) => {{
        let language = $crate::i18n::Lang::language(&$lang);
        let count: u64 = $count;
        $crate::i18n::fill(&language.plural_form(language.translate($text), count), &[("n", count.to_string())])
    }};
}

/// Marks a text for translation where it is shown (labels kept in tables); evaluates to the text itself.
macro_rules! n {
    ($text:literal) => {
        $text
    };
}

pub(crate) use {n, tr, trf, trn};

/// The Russian table.
static RU: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| ru::ENTRIES.iter().copied().collect());
/// The German table.
static DE: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| de::ENTRIES.iter().copied().collect());
/// The French table.
static FR: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| fr::ENTRIES.iter().copied().collect());
/// The Spanish table.
static ES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| es::ENTRIES.iter().copied().collect());

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    /// Every text marked in the source with `tr!`, `trf!`, `trn!` or `n!`.
    fn marked_texts() -> BTreeSet<String> {
        let mut texts = BTreeSet::new();
        let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("source folder readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") && !path.starts_with(dir_of_tables()) {
                    let source = std::fs::read_to_string(&path).expect("source readable");
                    collect(&source, &mut texts);
                }
            }
        }
        texts
    }

    /// Folder of the translation tables (their texts are not marks).
    fn dir_of_tables() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("i18n")
    }

    /// Adds the literal of each macro call in `source` to `texts`.
    fn collect(source: &str, texts: &mut BTreeSet<String>) {
        for marker in ["tr!(", "trf!(", "trn!(", "n!("] {
            let mut rest = source;
            while let Some(at) = rest.find(marker) {
                rest = rest.get(at.saturating_add(marker.len())..).unwrap_or_default();
                // The pattern for `n!` also matches the end of `trn!`, whose texts are found already.
                let preceded_by_word = source.len() > rest.len() && {
                    let start = source.len().saturating_sub(rest.len()).saturating_sub(marker.len());
                    source
                        .get(..start)
                        .and_then(|before| before.chars().last())
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                };
                if preceded_by_word {
                    continue;
                }
                if let Some(literal) = first_literal(rest, marker == "n!(") {
                    texts.insert(literal);
                }
            }
        }
    }

    /// The first string literal in `text` (after the language argument unless `bare`), unescaped.
    fn first_literal(text: &str, bare: bool) -> Option<String> {
        let start = if bare {
            text.find('"')?
        } else {
            text.find(',')
                .and_then(|comma| text.get(comma..)?.find('"').map(|quote| comma.saturating_add(quote)))?
        };
        let body = text.get(start.saturating_add(1)..)?;
        let mut literal = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(literal),
                '\\' => match chars.next()? {
                    'n' => literal.push('\n'),
                    'u' => {
                        let code: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                        literal.push(char::from_u32(u32::from_str_radix(&code, 16).ok()?)?);
                    }
                    other => literal.push(other),
                },
                c => literal.push(c),
            }
        }
        None
    }

    /// The `{name}` placeholders of a text.
    fn placeholders(text: &str) -> BTreeSet<String> {
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}').map(|(name, _)| name.to_owned()))
            .collect()
    }

    /// A table under test: its name, entries and the number of plural forms of its language.
    type Table = (&'static str, &'static [(&'static str, &'static str)], usize);

    /// The tables to check, with the number of plural forms each language has.
    fn tables() -> [Table; 4] {
        [("ru", ru::ENTRIES, 3), ("de", de::ENTRIES, 2), ("fr", fr::ENTRIES, 2), ("es", es::ENTRIES, 2)]
    }

    #[test]
    fn every_marked_text_is_translated_and_nothing_else() {
        let marked = marked_texts();
        assert!(marked.len() > 50, "the marks are found: {}", marked.len());
        for (name, entries, _) in tables() {
            let keys: BTreeSet<String> = entries.iter().map(|(key, _)| (*key).to_owned()).collect();
            let missing: Vec<&String> = marked.difference(&keys).collect();
            let unused: Vec<&String> = keys.difference(&marked).collect();
            assert!(missing.is_empty(), "{name} misses {missing:#?}");
            assert!(unused.is_empty(), "{name} has texts nobody uses: {unused:#?}");
            assert_eq!(keys.len(), entries.len(), "{name} has a text twice");
        }
    }

    #[test]
    fn translations_keep_placeholders_and_plural_forms() {
        for (name, entries, forms) in tables() {
            for (original, translated) in entries {
                assert_eq!(placeholders(original), placeholders(translated), "{name}: {original} → {translated}");
                if original.contains('|') {
                    assert_eq!(translated.split('|').count(), forms, "{name}: {original} → {translated}");
                }
            }
        }
    }

    #[test]
    fn decimals_use_the_language_separator() {
        assert_eq!(Language::English.decimal(50.34), "50.3");
        assert_eq!(Language::Russian.decimal(50.34), "50,3");
        assert_eq!(Language::French.decimal(10.0), "10,0");
    }

    #[test]
    fn plural_rules() {
        let forms = "{n} цель|{n} цели|{n} целей";
        let pick = |n| Language::Russian.plural_form(forms, n);
        assert_eq!(
            [pick(1), pick(2), pick(5), pick(11), pick(21), pick(22), pick(112)],
            ["{n} цель", "{n} цели", "{n} целей", "{n} целей", "{n} цель", "{n} цели", "{n} целей"]
        );
        assert_eq!(Language::English.plural_form("one|other", 1), "one");
        assert_eq!(Language::English.plural_form("one|other", 0), "other");
        assert_eq!(Language::French.plural_form("un|plusieurs", 0), "un");
    }

    #[test]
    fn languages_round_trip_through_keys() {
        assert!(Language::ALL.into_iter().all(|language| Language::from_key(language.key()) == Some(language)));
    }
}
