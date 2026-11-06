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
       pulseterm PATH

  -e, -x, --command, --   run COMMAND (the rest of the line) in a local tab
  --working-directory DIR start the shell (or COMMAND) in DIR; also -d DIR
  PATH                    a file opens in the editor, a folder in a shell there
                          (what File Explorer's 'Open with PulseTerm' passes)
  -h, --help              this text
  -V, --version           the version

With no arguments, a new window opens as the startup setting says.";

#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// No arguments: the startup setting decides (home, local, resume).
    Default,
    /// A local tab running this, instead of whatever the setting says.
    Local(PtyProgram),
    /// A file to open in the editor, on this computer.
    OpenFile(String),
    Help,
    Version,
}

/// What the first window starts with, when the command line said.
#[derive(Clone, Debug)]
pub enum Start {
    Program(PtyProgram),
    File(String),
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let mut dir: Option<String> = None;
    let mut command: Option<Vec<String>> = None;
    let mut path: Option<String> = None;
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
                } else if !other.starts_with('-') && path.is_none() {
                    // A bare path: what File Explorer's "Open with PulseTerm"
                    // and dropping a file on the exe both pass.
                    path = Some(other.to_string());
                } else {
                    return Err(format!("unknown argument: {other}"));
                }
            }
        }
    }
    if let Some(path) = path {
        if dir.is_some() || command.is_some() {
            return Err("a path cannot be combined with -e or --working-directory".into());
        }
        // A folder is somewhere to be; a file is something to edit.
        return Ok(if std::path::Path::new(&path).is_dir() {
            Launch::Local(PtyProgram::ShellIn(path))
        } else {
            Launch::OpenFile(path)
        });
    }
    Ok(match (dir, command) {
        (None, None) => Launch::Default,
        (Some(dir), None) => Launch::Local(PtyProgram::ShellIn(dir)),
        (None, Some(cmd)) => Launch::Local(PtyProgram::Command(join(&cmd))),
        // A local command runs in the home directory; with a directory as
        // well it starts there -- passed apart, so ConPTY can use it as the
        // working directory rather than parse a POSIX `cd`.
        (Some(dir), Some(cmd)) => Launch::Local(PtyProgram::CommandIn { command: join(&cmd), dir }),
    })
}

/// The command as one command line, each word quoted so a file name with
/// spaces arrives as one argument: for `/bin/sh -c` on Unix (nothing in it
/// is expanded), for CreateProcess's own parsing on Windows.
fn join(words: &[String]) -> String {
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

/// Windows has no shell in the way: CreateProcess hands the line to the
/// program, which splits it by the MSVC runtime's rules -- double quotes, and
/// backslashes that only escape when they precede a quote. Single quotes mean
/// nothing there, which is why the POSIX quoting below broke `-e` with any
/// argument that had a space in it.
#[cfg(windows)]
fn quote(word: &str) -> String {
    if !word.is_empty() && !word.contains([' ', '\t', '"']) {
        return word.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in word.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, then the quote escaped.
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    // Trailing backslashes would escape the closing quote; double them.
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(not(windows))]
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
        #[cfg(not(windows))]
        assert_eq!(
            p(&["-e", "less", "my file's.txt"]),
            Ok(Launch::Local(PtyProgram::Command(
                r"less 'my file'\''s.txt'".into()
            )))
        );
        assert!(p(&["-e"]).is_err());
    }

    /// CreateProcess's rules, not the shell's: double quotes, and backslashes
    /// that only matter before a quote or at the end of a quoted word.
    #[cfg(windows)]
    #[test]
    fn windows_quotes_for_createprocess() {
        assert_eq!(
            p(&["-e", "notepad", r"C:\My Files\a.txt"]),
            Ok(Launch::Local(PtyProgram::Command(r#"notepad "C:\My Files\a.txt""#.into())))
        );
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote(r"C:\plain\path"), r"C:\plain\path");
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
            Ok(Launch::Local(PtyProgram::CommandIn {
                command: "htop".into(),
                dir: "/a b".into()
            }))
        );
    }

    #[test]
    fn a_bare_path_opens_a_file_or_a_shell_in_a_folder() {
        let here = std::env::temp_dir().to_string_lossy().to_string();
        assert_eq!(p(&[&here]), Ok(Launch::Local(PtyProgram::ShellIn(here.clone()))));
        assert_eq!(p(&["notes.txt"]), Ok(Launch::OpenFile("notes.txt".into())));
        assert!(p(&["notes.txt", "-e", "htop"]).is_err());
    }

    #[test]
    fn unknown_arguments_are_refused() {
        assert!(p(&["--frobnicate"]).is_err());
    }
}
