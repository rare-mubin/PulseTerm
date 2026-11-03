//! The command line: what a launch asks the first window to open.
//!
//! Every launch is its own process and its own window -- nothing here looks
//! for a running copy -- so a dock's "New Window", a second click in the menu
//! and `pulseterm &` in a shell all just open another one.
//!
//! The two arguments are the ones the desktop hands a terminal: `-e` from
//! `xdg-terminal-exec` and anything that runs a program "in a terminal",
//! `--working-directory` from a file manager's "open terminal here". Both are
//! declared in the `.desktop` file (`X-TerminalArgExec`, `X-TerminalArgDir`).

use crate::pty::PtyProgram;

pub const USAGE: &str = "\
usage: pulseterm [--working-directory DIR] [-e COMMAND [ARGS...]]

  -e, -x, --command, --   run COMMAND (the rest of the line) in a local tab
  --working-directory DIR start the shell (or COMMAND) in DIR; also -d DIR
  -h, --help              this text
  -V, --version           the version

With no arguments, a new window opens as the startup setting says.";

#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// No arguments: the startup setting decides (home, local, resume).
    Default,
    /// A local tab running this, instead of whatever the setting says.
    Local(PtyProgram),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let mut dir: Option<String> = None;
    let mut command: Option<Vec<String>> = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Launch::Help),
            "-V" | "--version" => return Ok(Launch::Version),
            // Everything after the flag is the command and its own
            // arguments, untouched -- `-e ssh -p 2222 host` must not have its
            // `-p` read as one of ours.
            "-e" | "-x" | "--command" | "--" => {
                let rest: Vec<String> = args.by_ref().collect();
                if rest.is_empty() {
                    return Err(format!("{arg} needs a command after it"));
                }
                command = Some(rest);
            }
            "--working-directory" | "-d" => {
                dir = Some(
                    args.next()
                        .ok_or_else(|| format!("{arg} needs a directory after it"))?,
                );
            }
            other => {
                if let Some(d) = other.strip_prefix("--working-directory=") {
                    dir = Some(d.to_string());
                } else {
                    return Err(format!("unknown argument: {other}"));
                }
            }
        }
    }
    Ok(match (dir, command) {
        (None, None) => Launch::Default,
        (Some(dir), None) => Launch::Local(PtyProgram::ShellIn(dir)),
        (None, Some(cmd)) => Launch::Local(PtyProgram::Command(join(&cmd))),
        // A local command runs in the home directory; with a directory as
        // well, it is changed into first.
        (Some(dir), Some(cmd)) => Launch::Local(PtyProgram::Command(format!(
            "cd {} && exec {}",
            quote(&dir),
            join(&cmd)
        ))),
    })
}

/// The command as one `/bin/sh -c` line, each word quoted, so a file name
/// with spaces arrives as one argument and nothing in it is expanded.
fn join(words: &[String]) -> String {
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

fn quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Launch, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_leave_it_to_the_setting() {
        assert_eq!(p(&[]), Ok(Launch::Default));
    }

    #[test]
    fn a_command_keeps_its_own_arguments_and_quoting() {
        assert_eq!(
            p(&["-e", "ssh", "-p", "2222", "host"]),
            Ok(Launch::Local(PtyProgram::Command(
                "ssh -p 2222 host".into()
            )))
        );
        assert_eq!(
            p(&["-e", "less", "my file's.txt"]),
            Ok(Launch::Local(PtyProgram::Command(
                r"less 'my file'\''s.txt'".into()
            )))
        );
        assert!(p(&["-e"]).is_err());
    }

    #[test]
    fn a_directory_opens_a_shell_there_or_runs_the_command_there() {
        assert_eq!(
            p(&["--working-directory", "/tmp"]),
            Ok(Launch::Local(PtyProgram::ShellIn("/tmp".into())))
        );
        assert_eq!(
            p(&["--working-directory=/tmp"]),
            Ok(Launch::Local(PtyProgram::ShellIn("/tmp".into())))
        );
        assert_eq!(
            p(&["-d", "/a b", "-e", "htop"]),
            Ok(Launch::Local(PtyProgram::Command(
                "cd '/a b' && exec htop".into()
            )))
        );
    }

    #[test]
    fn unknown_arguments_are_refused() {
        assert!(p(&["--frobnicate"]).is_err());
    }
}
