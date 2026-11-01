//! Quote-aware scan of one command string.
//!
//! [`classify`] is a scanner, not an evaluator. It never expands a variable,
//! never resolves an alias, and never decides that a command is safe to run.
//! It reads the command once and reports whether it is one literal command word
//! sequence, a small chain of literal segments, or something it does not
//! model.
//!
//! A scan that gets lost fails closed to [`CommandClass::Opaque`]. Too much
//! input is also opaque instead of an error: the caps are denial-of-service
//! limits for the scan, and the host may still run an opaque command after its
//! own decision.

use super::argv::ShellKind;

/// Most segments a [`CommandClass::Compound`] may hold.
///
/// A command with more segments is [`CommandClass::Opaque`].
pub const MAX_SEGMENTS: usize = 32;

/// Most bytes [`classify`] scans.
///
/// A longer command is [`CommandClass::Opaque`].
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;

/// What the scanner saw in one command string.
///
/// The class is a description, not an approval. The SDK never allows or denies
/// a command; the host decides what to do with the class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandClass {
    /// One command word sequence with no unquoted shell operator.
    ///
    /// Quotes are balanced. Nothing in the text is expanded.
    Literal,
    /// Two or more literal segments joined only by unquoted `&&`, `||`, `;`,
    /// or `|`.
    ///
    /// Every segment is itself literal: it contains no substitution, redirect,
    /// newline, parenthesis, brace, or backgrounding. A compound is not a safe
    /// command, and the host should look at every segment.
    Compound {
        /// The literal text of each segment, trimmed and non-empty.
        ///
        /// The text is a slice of the original command, not a rebuilt or
        /// normalised form.
        segments: Vec<String>,
    },
    /// Anything this scanner does not model, including a scan that got lost.
    ///
    /// This includes substitution, redirection, control characters, an
    /// unbalanced quote, an unfinished escape, and input over a cap.
    Opaque,
}

/// Classifies `command` under the shell family `kind`.
///
/// The scan is quote aware: a quoted operator is part of a word and never
/// splits a segment. The scan never expands variables, never runs a command,
/// and never touches the filesystem.
///
/// [`ShellKind::Posix`] can report [`CommandClass::Literal`] or
/// [`CommandClass::Compound`]. [`ShellKind::PowerShell`] and [`ShellKind::Cmd`]
/// report [`CommandClass::Literal`] only for a single line with no unquoted
/// `&`, `|`, `<`, `>`, or newline, and never report
/// [`CommandClass::Compound`]. An unterminated quote is
/// [`CommandClass::Opaque`] for every kind.
///
/// A command longer than [`MAX_COMMAND_BYTES`], or a compound with more than
/// [`MAX_SEGMENTS`] segments, is [`CommandClass::Opaque`]. The caps never
/// produce an error.
#[must_use]
pub fn classify(kind: ShellKind, command: &str) -> CommandClass {
    if command.len() > MAX_COMMAND_BYTES || command.trim().is_empty() {
        return CommandClass::Opaque;
    }

    match kind {
        ShellKind::Posix => classify_posix(command),
        ShellKind::PowerShell | ShellKind::Cmd => classify_flat(kind, command),
    }
}

/// Quote state of the scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// Scans a POSIX command for literal segments.
///
/// `#!` comments, globbing, and tilde expansion are ordinary characters here.
/// The scanner only refuses what it cannot prove literal.
fn classify_posix(command: &str) -> CommandClass {
    let bytes = command.as_bytes();
    let mut quote = Quote::None;
    let mut saw_operator = false;
    let mut segments: Vec<String> = Vec::new();
    let mut segment_start = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Quote::None => match byte {
                b'\'' => quote = Quote::Single,
                b'"' => quote = Quote::Double,
                b'\\' => {
                    // An escaped byte is part of the word. A backslash at the
                    // end has nothing to escape.
                    if index + 1 >= bytes.len() {
                        return CommandClass::Opaque;
                    }
                    index += 1;
                }
                b'$' => {
                    // A command substitution or a braced expansion hides
                    // arbitrary text. A plain variable reference does not.
                    if matches!(bytes.get(index + 1), Some(b'(' | b'{')) {
                        return CommandClass::Opaque;
                    }
                }
                b'`' | b'(' | b')' | b'{' | b'}' | b'<' | b'>' => {
                    return CommandClass::Opaque;
                }
                b'&' => {
                    if bytes.get(index + 1) != Some(&b'&') {
                        // `&` alone is backgrounding.
                        return CommandClass::Opaque;
                    }
                    if !push_segment(command, segment_start, index, &mut segments) {
                        return CommandClass::Opaque;
                    }
                    saw_operator = true;
                    segment_start = index + 2;
                    index += 1;
                }
                b'|' => {
                    if !push_segment(command, segment_start, index, &mut segments) {
                        return CommandClass::Opaque;
                    }
                    saw_operator = true;
                    if bytes.get(index + 1) == Some(&b'|') {
                        index += 1;
                    }
                    segment_start = index + 1;
                }
                b';' => {
                    if !push_segment(command, segment_start, index, &mut segments) {
                        return CommandClass::Opaque;
                    }
                    saw_operator = true;
                    segment_start = index + 1;
                }
                b'\r' | b'\n' => return CommandClass::Opaque,
                _ => {}
            },
            Quote::Single => {
                if byte == b'\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => match byte {
                b'"' => quote = Quote::None,
                b'\\' => {
                    if index + 1 >= bytes.len() {
                        return CommandClass::Opaque;
                    }
                    index += 1;
                }
                b'`' => return CommandClass::Opaque,
                b'$' if matches!(bytes.get(index + 1), Some(b'(' | b'{')) => {
                    return CommandClass::Opaque;
                }
                _ => {}
            },
        }
        index += 1;
    }

    if quote != Quote::None {
        return CommandClass::Opaque;
    }

    if !saw_operator {
        return CommandClass::Literal;
    }

    if !push_segment(command, segment_start, command.len(), &mut segments) {
        return CommandClass::Opaque;
    }

    CommandClass::Compound { segments }
}

/// Scans a PowerShell or `cmd` command as a single line.
///
/// These kinds have no tested separator set, so they never report
/// [`CommandClass::Compound`]. The escape characters are refused because they
/// can hide the quote that would decide whether an operator is inside a word,
/// which is exactly the case a fail-closed scan must not guess about.
fn classify_flat(kind: ShellKind, command: &str) -> CommandClass {
    // Only a single line can be literal for these families.
    if command.contains(['\r', '\n']) {
        return CommandClass::Opaque;
    }

    let bytes = command.as_bytes();
    let mut quote = Quote::None;
    let mut index = 0usize;

    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Quote::Single => {
                if byte == b'\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => {
                if byte == b'"' {
                    quote = Quote::None;
                } else if kind == ShellKind::PowerShell {
                    // PowerShell expands `$(...)` inside double quotes, and a
                    // backquote escapes the next byte, including the closing
                    // quote. Both hide text from this scan.
                    if byte == b'`'
                        || matches!(
                            (bytes.get(index), bytes.get(index + 1)),
                            (Some(b'$'), Some(b'('))
                        )
                    {
                        return CommandClass::Opaque;
                    }
                } else if kind == ShellKind::Cmd && byte == b'^' {
                    // `cmd` treats `^` inside a quoted word as an escape in
                    // some contexts that this scan does not model.
                    return CommandClass::Opaque;
                }
            }
            Quote::None => match (kind, byte) {
                // `cmd` has no single-quote syntax: `'` is an ordinary byte.
                (ShellKind::Cmd, b'"') => quote = Quote::Double,
                (ShellKind::PowerShell, b'\'') => quote = Quote::Single,
                (ShellKind::PowerShell, b'"') => quote = Quote::Double,
                // `cmd` escapes with `^`, and PowerShell with a backquote.
                (ShellKind::Cmd, b'^') => return CommandClass::Opaque,
                (ShellKind::PowerShell, b'`') => return CommandClass::Opaque,
                (ShellKind::PowerShell, b'$') if bytes.get(index + 1) == Some(&b'(') => {
                    return CommandClass::Opaque;
                }
                (_, b'&' | b'|' | b'<' | b'>') => return CommandClass::Opaque,
                _ => {}
            },
        }
        index += 1;
    }

    if quote == Quote::None {
        CommandClass::Literal
    } else {
        CommandClass::Opaque
    }
}

/// Pushes the trimmed text between `start` and `end` as the next segment.
///
/// Returns `false` when the text is empty, when the slice is not on a character
/// boundary, or when the segment cap would be exceeded. The caller turns
/// `false` into [`CommandClass::Opaque`].
fn push_segment(command: &str, start: usize, end: usize, segments: &mut Vec<String>) -> bool {
    if segments.len() >= MAX_SEGMENTS {
        return false;
    }
    let Some(text) = command.get(start..end) else {
        return false;
    };
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    segments.push(text.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(class: &CommandClass) -> &[String] {
        match class {
            CommandClass::Compound { segments } => segments,
            other => panic!("expected Compound, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_command_is_literal_for_every_shell() {
        for kind in [ShellKind::Posix, ShellKind::PowerShell, ShellKind::Cmd] {
            assert_eq!(classify(kind, "echo hi"), CommandClass::Literal);
        }
    }

    #[test]
    fn a_single_quoted_operator_is_hidden() {
        assert_eq!(
            classify(ShellKind::Posix, "echo \"a && b\""),
            CommandClass::Literal
        );
        assert_eq!(
            classify(ShellKind::Posix, "echo 'a && b'"),
            CommandClass::Literal
        );
        assert_eq!(
            classify(ShellKind::Posix, "grep 'x|y' file"),
            CommandClass::Literal
        );
    }

    #[test]
    fn two_literal_posix_segments_are_compound() {
        let class = classify(ShellKind::Posix, "echo a && echo b");
        assert_eq!(
            segments(&class),
            ["echo a".to_string(), "echo b".to_string()]
        );
    }

    #[test]
    fn each_posix_operator_splits_segments() {
        for command in ["echo a && echo b", "echo a || echo b", "echo a ; echo b"] {
            let class = classify(ShellKind::Posix, command);
            assert_eq!(segments(&class).len(), 2, "command: {command}");
        }

        let class = classify(ShellKind::Posix, "echo a | grep a");
        assert_eq!(
            segments(&class),
            ["echo a".to_string(), "grep a".to_string()]
        );
    }

    #[test]
    fn a_quoted_operator_does_not_split_a_segment() {
        let class = classify(ShellKind::Posix, "echo 'a && b' && true");
        assert_eq!(
            segments(&class),
            ["echo 'a && b'".to_string(), "true".to_string()]
        );

        let class = classify(ShellKind::Posix, "echo 'a; b|c' | cat");
        assert_eq!(
            segments(&class),
            ["echo 'a; b|c'".to_string(), "cat".to_string()]
        );
    }

    #[test]
    fn segments_are_trimmed_literal_text() {
        let class = classify(ShellKind::Posix, "  echo   a   &&   echo   b  ");
        assert_eq!(
            segments(&class),
            ["echo   a".to_string(), "echo   b".to_string()]
        );
    }

    #[test]
    fn an_unterminated_quote_is_opaque() {
        assert_eq!(
            classify(ShellKind::Posix, "echo \"unterminated"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::Posix, "echo 'unterminated"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::PowerShell, "Write-Output 'x"),
            CommandClass::Opaque
        );
        assert_eq!(classify(ShellKind::Cmd, "echo \"x"), CommandClass::Opaque);
    }

    #[test]
    fn substitution_is_opaque() {
        for command in [
            "echo $(whoami)",
            "echo \"$(whoami)\"",
            "echo `whoami`",
            "echo ${HOME}/bin",
            "cat <(echo hi)",
            "tee >(cat)",
        ] {
            assert_eq!(
                classify(ShellKind::Posix, command),
                CommandClass::Opaque,
                "command: {command}"
            );
        }
    }

    #[test]
    fn redirection_and_grouping_are_opaque() {
        for command in [
            "echo hi > out.txt",
            "echo hi >> out.txt",
            "cat < in.txt",
            "cat <<EOF",
            "(echo hi)",
            "echo hi &",
            "{ echo hi; }",
            "echo hi\necho bye",
        ] {
            assert_eq!(
                classify(ShellKind::Posix, command),
                CommandClass::Opaque,
                "command: {command}"
            );
        }
    }

    #[test]
    fn a_single_quoted_substitution_is_literal_text() {
        // POSIX single quotes do not expand, so the text is one word.
        assert_eq!(
            classify(ShellKind::Posix, "echo '$(whoami)'"),
            CommandClass::Literal
        );
    }

    #[test]
    fn a_trailing_odd_backslash_is_opaque() {
        assert_eq!(classify(ShellKind::Posix, "echo a\\"), CommandClass::Opaque);
        assert_eq!(
            classify(ShellKind::Posix, "echo a\\\\"),
            CommandClass::Literal
        );
    }

    #[test]
    fn more_than_the_segment_cap_is_opaque() {
        let under: Vec<String> = (0..MAX_SEGMENTS)
            .map(|index| format!("echo {index}"))
            .collect();
        let under = under.join(" && ");
        let class = classify(ShellKind::Posix, &under);
        assert_eq!(segments(&class).len(), MAX_SEGMENTS);

        let over: Vec<String> = (0..MAX_SEGMENTS + 1)
            .map(|index| format!("echo {index}"))
            .collect();
        let over = over.join(" && ");
        assert_eq!(classify(ShellKind::Posix, &over), CommandClass::Opaque);
    }

    #[test]
    fn more_than_the_command_cap_is_opaque() {
        let over = format!("echo {}", "a".repeat(MAX_COMMAND_BYTES));
        assert!(over.len() > MAX_COMMAND_BYTES);
        assert_eq!(classify(ShellKind::Posix, &over), CommandClass::Opaque);
        assert_eq!(classify(ShellKind::PowerShell, &over), CommandClass::Opaque);

        let at_cap = format!("echo {}", "a".repeat(MAX_COMMAND_BYTES - 5));
        assert_eq!(at_cap.len(), MAX_COMMAND_BYTES);
        assert_eq!(classify(ShellKind::Posix, &at_cap), CommandClass::Literal);
    }

    #[test]
    fn an_empty_command_is_opaque() {
        for kind in [ShellKind::Posix, ShellKind::PowerShell, ShellKind::Cmd] {
            assert_eq!(classify(kind, ""), CommandClass::Opaque);
            assert_eq!(classify(kind, "   \t"), CommandClass::Opaque);
        }
    }

    #[test]
    fn powershell_never_reports_compound() {
        let class = classify(ShellKind::PowerShell, "a && b");
        assert_eq!(class, CommandClass::Opaque);
        assert!(
            !matches!(class, CommandClass::Compound { .. }),
            "PowerShell must never report Compound"
        );

        assert_eq!(
            classify(ShellKind::PowerShell, "Get-ChildItem | Select-Object Name"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::PowerShell, "Write-Output a; Write-Output b"),
            CommandClass::Literal
        );
    }

    #[test]
    fn powershell_quoted_operators_are_hidden() {
        assert_eq!(
            classify(ShellKind::PowerShell, "Write-Output 'a && b'"),
            CommandClass::Literal
        );
        assert_eq!(
            classify(ShellKind::PowerShell, "Write-Output \"a | b\""),
            CommandClass::Literal
        );
    }

    #[test]
    fn a_powershell_subexpression_is_opaque() {
        assert_eq!(
            classify(ShellKind::PowerShell, "Write-Output $(Get-Date)"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::PowerShell, "Write-Output \"$(Get-Date)\""),
            CommandClass::Opaque
        );
    }

    #[test]
    fn cmd_with_a_pipe_is_opaque() {
        assert_eq!(
            classify(ShellKind::Cmd, "dir | findstr x"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::Cmd, "dir && echo done"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::Cmd, "dir > out.txt"),
            CommandClass::Opaque
        );
        assert_eq!(
            classify(ShellKind::Cmd, "echo a\ndir"),
            CommandClass::Opaque
        );
    }

    #[test]
    fn cmd_quoted_operators_are_hidden() {
        assert_eq!(
            classify(ShellKind::Cmd, "echo \"a | b\""),
            CommandClass::Literal
        );
        assert_eq!(
            classify(ShellKind::Cmd, "echo \"a && b\""),
            CommandClass::Literal
        );
    }

    #[test]
    fn cmd_does_not_treat_single_quotes_as_quoting() {
        // In `cmd` a single quote is an ordinary byte, so the operator after it
        // is still live. Reporting Literal here would hide a pipe.
        assert_eq!(
            classify(ShellKind::Cmd, "echo 'a | b'"),
            CommandClass::Opaque
        );
    }

    #[test]
    fn a_cmd_caret_escape_is_opaque() {
        assert_eq!(
            classify(ShellKind::Cmd, "echo a ^& b"),
            CommandClass::Opaque
        );
    }

    #[test]
    fn the_caps_apply_to_every_shell() {
        let over: Vec<String> = (0..MAX_SEGMENTS + 1)
            .map(|index| format!("echo {index}"))
            .collect();
        let over = over.join(" && ");
        assert_eq!(classify(ShellKind::PowerShell, &over), CommandClass::Opaque);
        assert_eq!(classify(ShellKind::Cmd, &over), CommandClass::Opaque);
    }
}
