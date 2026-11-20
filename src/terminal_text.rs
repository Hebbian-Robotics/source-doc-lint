use std::fmt::{self, Display, Formatter, Write};

// Line breaks are escaped too, so a value cannot forge or overwrite report
// lines other than the one it is printed on.
pub struct EscapedTerminalText<'text>(pub &'text str);

impl Display for EscapedTerminalText<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        for character in self.0.chars() {
            if alters_terminal_display(character) {
                for escaped in character.escape_default() {
                    formatter.write_char(escaped)?;
                }
            } else {
                formatter.write_char(character)?;
            }
        }
        Ok(())
    }
}

fn alters_terminal_display(character: char) -> bool {
    // Beyond C0, DEL, and C1 controls: invisible formatting characters that
    // reorder, hide, or break visible text (Arabic letter mark, zero-width
    // characters, directional marks, embeddings, overrides, isolates, line and
    // paragraph separators, and the byte order mark).
    character.is_control()
        || matches!(
            character,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}
