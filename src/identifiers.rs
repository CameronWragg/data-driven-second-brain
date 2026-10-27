//! Identifier splitting for keyword search (SPEC §6.2): `getUserById` →
//! `get user by id`.

/// Splits `text` into lower-case words at every identifier boundary: between
/// letters and anything else, at a lower-to-upper change (`getUser`), before
/// the last capital of an acronym (`HTTPServer`), and between letters and
/// digits. The words are joined by single spaces.
pub fn split(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut words = String::new();
    // Whether the next kept character starts a new word.
    let mut boundary = false;
    for (index, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            boundary = true;
            continue;
        }
        if let Some(&previous) = index.checked_sub(1).map(|before| &chars[before]) {
            let next = chars.get(index + 1);
            boundary |= previous.is_alphanumeric()
                && (previous.is_numeric() != c.is_numeric()
                    || (previous.is_lowercase() && upper(c))
                    || (upper(previous)
                        && upper(c)
                        && next.is_some_and(|next| next.is_lowercase())));
        }
        // Lower-casing can make a mark that isn't a letter, such as the dot
        // of `İ`; it is dropped so that splitting again changes nothing.
        for lower in c.to_lowercase().filter(|lower| lower.is_alphanumeric()) {
            if boundary && !words.is_empty() {
                words.push(' ');
            }
            boundary = false;
            words.push(lower);
        }
    }
    words
}

/// Whether `c` is a capital that lower-cases to something else. A capital
/// with no lower case, such as `𝒢`, is caseless here, so that splitting again
/// finds no new boundary before it.
fn upper(c: char) -> bool {
    c.is_uppercase() && !c.to_lowercase().eq([c])
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn identifiers_split_into_their_words() {
        for (text, words) in [
            ("getUserById", "get user by id"),
            ("HTTPServer", "http server"),
            ("snake_case and kebab-case", "snake case and kebab case"),
            ("parseV2Header", "parse v 2 header"),
            ("prisma/schema.prisma", "prisma schema prisma"),
            ("", ""),
        ] {
            assert_eq!(split(text), words, "{text}");
        }
    }

    /// Every alphanumeric character, lower-cased, as `split` should keep them.
    fn letters(text: &str) -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .filter(|c| c.is_alphanumeric())
            .collect()
    }

    proptest! {
        #[test]
        fn splitting_keeps_every_letter_and_digit_and_nothing_else(text in "\\PC*") {
            let words = split(&text);

            prop_assert_eq!(words.replace(' ', ""), letters(&text));
            prop_assert!(!words.starts_with(' ') && !words.ends_with(' ') && !words.contains("  "));
        }

        #[test]
        fn splitting_is_idempotent(text in "\\PC*") {
            let once = split(&text);

            prop_assert_eq!(split(&once), once);
        }

        /// One-letter words are left out: `aAA` could be `a a a` or `a aa`.
        #[test]
        fn a_camel_case_identifier_splits_into_the_words_it_was_built_from(
            words in prop::collection::vec("[a-z]{2,8}", 1..6)
        ) {
            let camel: String = words
                .iter()
                .enumerate()
                .map(|(index, word)| {
                    if index == 0 {
                        word.clone()
                    } else {
                        word[..1].to_uppercase() + &word[1..]
                    }
                })
                .collect();

            prop_assert_eq!(split(&camel), words.join(" "));
        }
    }
}
