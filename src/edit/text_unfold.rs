//! The 1:1 character fold the lenient and fuzzy tiers search through.
//!
//! Every entry maps one character to one ASCII character, so a fold never
//! changes the character count of the text and the offset map only has to
//! account for the shorter byte length of the replacement. Nothing is deleted,
//! nothing expands to two characters, and no compatibility normalization runs:
//! a fold that moved a block would make the match land in the wrong place.

/// Folds one character to the ASCII character it usually stands for.
///
/// Curly quotes, en and em dashes, and the non-breaking spaces are folded.
/// Every other character is returned unchanged, and its bytes are copied as
/// they are.
pub(crate) fn fold_char(character: char) -> char {
    match character {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2013}' | '\u{2014}' => '-',
        '\u{00A0}' | '\u{202F}' => ' ',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::fold_char;

    #[test]
    fn every_folded_character_becomes_one_ascii_character() {
        for (source, expected) in [
            ('\u{2018}', '\''),
            ('\u{2019}', '\''),
            ('\u{201A}', '\''),
            ('\u{201B}', '\''),
            ('\u{201C}', '"'),
            ('\u{201D}', '"'),
            ('\u{201E}', '"'),
            ('\u{201F}', '"'),
            ('\u{2013}', '-'),
            ('\u{2014}', '-'),
            ('\u{00A0}', ' '),
            ('\u{202F}', ' '),
        ] {
            let folded = fold_char(source);
            assert_eq!(folded, expected);
            assert!(folded.is_ascii());
        }
    }

    #[test]
    fn a_character_outside_the_table_is_returned_unchanged() {
        // A guillemet, a full-width quote, and plain ASCII are not folded.
        for character in ['\u{00AB}', '\u{300C}', 'a', 'é', '\n'] {
            assert_eq!(fold_char(character), character);
        }
    }
}
