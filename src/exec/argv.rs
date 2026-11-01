//! Shell families and the argv they build for one command string.
//!
//! The model writes one command string. The host picks the shell family by
//! passing a [`ShellSpec`] it already resolved, and [`argv`] turns that pair
//! into the process arguments. This module performs no I/O, does not search
//! `PATH`, and does not read the environment. It never uses a login shell, so
//! `bash -lc` and its rc files are not used.

use crate::ToolError;

/// The shell family a [`ShellSpec`] runs.
///
/// The family decides the argv shape. A POSIX shell receives `-c`, PowerShell
/// receives its non-interactive flags, and `cmd` receives `/c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellKind {
    /// A POSIX shell, such as `sh`, `bash`, or `zsh`.
    ///
    /// The command is passed with `-c`, never `-lc`, so no login rc file runs.
    Posix,
    /// PowerShell, such as `pwsh` or `powershell`.
    ///
    /// The command is passed with `-NoLogo -NoProfile -NonInteractive
    /// -Command`. No execution policy is changed.
    PowerShell,
    /// The Windows command interpreter, `cmd.exe`.
    ///
    /// The command is passed with `/c`.
    Cmd,
}

/// A resolved shell executable and the family it speaks.
///
/// The host resolves the executable and stores it here. This type does not
/// search `PATH`, does not inspect `$SHELL`, and does not check that the path
/// exists. Build one with [`ShellSpec::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShellSpec {
    executable: String,
    kind: ShellKind,
}

impl ShellSpec {
    /// Builds a shell spec from an executable and its family.
    ///
    /// The executable is stored as given and is not trimmed or joined to a
    /// directory.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `executable` is empty or
    /// whitespace only. The value is not repaired.
    pub fn new(executable: impl Into<String>, kind: ShellKind) -> Result<Self, ToolError> {
        let executable = executable.into();
        if executable.trim().is_empty() {
            return Err(ToolError::spec_invalid(
                "shell executable must not be empty or whitespace only",
            ));
        }
        Ok(Self { executable, kind })
    }

    /// The executable exactly as the host supplied it.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The shell family that decides the argv shape.
    #[must_use]
    pub const fn kind(&self) -> ShellKind {
        self.kind
    }
}

/// Builds the process arguments for `command` under `shell`.
///
/// The first element is always [`ShellSpec::executable`]. The command is the
/// last element and is passed verbatim; it is not escaped, split, or expanded.
///
/// The shapes are:
///
/// - [`ShellKind::Posix`]: `[executable, "-c", command]`
/// - [`ShellKind::PowerShell`]: `[executable, "-NoLogo", "-NoProfile",
///   "-NonInteractive", "-Command", command]`
/// - [`ShellKind::Cmd`]: `[executable, "/c", command]`
#[must_use]
pub fn argv(shell: &ShellSpec, command: &str) -> Vec<String> {
    let executable = shell.executable().to_string();
    match shell.kind() {
        ShellKind::Posix => vec![executable, "-c".to_string(), command.to_string()],
        ShellKind::PowerShell => vec![
            executable,
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ],
        ShellKind::Cmd => vec![executable, "/c".to_string(), command.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn spec(executable: &str, kind: ShellKind) -> ShellSpec {
        ShellSpec::new(executable, kind).expect("a non-empty executable is valid")
    }

    fn assert_spec_invalid(error: &ToolError) {
        assert_eq!(error.code(), ErrorCode::SpecInvalid);
        assert!(
            error.to_string().starts_with("AGT_SPEC_INVALID:"),
            "unexpected display: {error}"
        );
    }

    #[test]
    fn posix_argv_uses_dash_c() {
        let shell = spec("/bin/sh", ShellKind::Posix);
        assert_eq!(argv(&shell, "echo hi"), vec!["/bin/sh", "-c", "echo hi"]);
    }

    #[test]
    fn powershell_argv_uses_the_non_interactive_flags() {
        let shell = spec("/usr/local/bin/pwsh", ShellKind::PowerShell);
        assert_eq!(
            argv(&shell, "Get-ChildItem"),
            vec![
                "/usr/local/bin/pwsh",
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Get-ChildItem"
            ]
        );
    }

    #[test]
    fn cmd_argv_uses_slash_c() {
        let shell = spec("cmd.exe", ShellKind::Cmd);
        assert_eq!(argv(&shell, "dir"), vec!["cmd.exe", "/c", "dir"]);
    }

    #[test]
    fn never_uses_a_login_shell_or_an_execution_policy_flag() {
        let posix = spec("bash", ShellKind::Posix);
        let argument = argv(&posix, "true");
        assert!(!argument.iter().any(|part| part == "-lc"));
        assert!(!argument.iter().any(|part| part == "-l"));

        let powershell = spec("pwsh", ShellKind::PowerShell);
        let argument = argv(&powershell, "true");
        assert!(
            !argument
                .iter()
                .any(|part| part.eq_ignore_ascii_case("Bypass"))
        );
    }

    #[test]
    fn the_command_is_passed_verbatim_and_last() {
        let shell = spec("sh", ShellKind::Posix);
        let command = "printf '%s' \"a && b\"";
        let argument = argv(&shell, command);
        assert_eq!(argument.first().map(String::as_str), Some("sh"));
        assert_eq!(argument.last().map(String::as_str), Some(command));
        assert_eq!(argument.len(), 3);
    }

    #[test]
    fn an_empty_command_still_builds_three_arguments() {
        let shell = spec("sh", ShellKind::Posix);
        assert_eq!(argv(&shell, ""), vec!["sh", "-c", ""]);
    }

    #[test]
    fn accessors_return_what_was_stored() {
        let shell = spec("./scripts/zsh", ShellKind::Posix);
        assert_eq!(shell.executable(), "./scripts/zsh");
        assert_eq!(shell.kind(), ShellKind::Posix);
    }

    #[test]
    fn an_empty_executable_is_rejected() {
        let error =
            ShellSpec::new("", ShellKind::Posix).expect_err("an empty executable is not usable");
        assert_spec_invalid(&error);
    }

    #[test]
    fn a_whitespace_only_executable_is_rejected() {
        let error = ShellSpec::new("  \t\n", ShellKind::PowerShell)
            .expect_err("whitespace is not an executable");
        assert_spec_invalid(&error);
    }

    #[test]
    fn an_executable_with_spaces_is_kept_as_given() {
        // The host already resolved the path; this crate does not trim it.
        let shell = ShellSpec::new("/opt/my shell/bin/sh", ShellKind::Posix)
            .expect("a path with spaces is the host's business");
        assert_eq!(shell.executable(), "/opt/my shell/bin/sh");
        assert_eq!(
            argv(&shell, "true"),
            vec!["/opt/my shell/bin/sh", "-c", "true"]
        );
    }
}
