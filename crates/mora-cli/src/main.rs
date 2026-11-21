//! The `mora` executable (§17.1).

use std::ffi::{OsStr, OsString};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use serde::Serialize;

/// The exit codes of §17.1.
mod exit {
    /// A build or test failed, or the output could not be written.
    pub const FAILURE: u8 = 1;
    /// The command line is wrong, or the command does not exist yet.
    pub const USAGE: u8 = 2;
}

/// The Mora toolchain.
#[derive(Parser)]
#[command(name = "mora", version, propagate_version = true)]
struct Cli {
    /// When to color output.
    #[arg(long, global = true, value_enum, default_value_t = Color::Auto)]
    color: Color,
    /// How to print diagnostics: for people, or one JSON object per line.
    #[arg(long, global = true, value_enum, default_value_t = MessageFormat::Human)]
    message_format: MessageFormat,
    /// What to do.
    #[command(subcommand)]
    command: Command,
}

/// When diagnostics and messages are colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Color {
    /// Color when the output is a terminal and NO_COLOR is unset or empty.
    Auto,
    /// Always color.
    Always,
    /// Never color.
    Never,
}

impl From<Color> for clap::ColorChoice {
    fn from(color: Color) -> Self {
        match color {
            Color::Auto => Self::Auto,
            Color::Always => Self::Always,
            Color::Never => Self::Never,
        }
    }
}

/// How diagnostics and command results are printed.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MessageFormat {
    /// Rendered for a terminal (§17.6).
    Human,
    /// One JSON object per line (§17.6).
    Json,
}

/// A project template for `mora new`.
#[derive(Clone, Copy, ValueEnum)]
enum Template {
    /// The program of §2.2 trimmed to one endpoint.
    Web,
}

/// An intermediate form `mora build` can also write.
#[derive(Clone, Copy, ValueEnum)]
enum Emit {
    /// An object file.
    Obj,
    /// MIR after optimization.
    Mir,
    /// Cranelift IR.
    Clif,
}

#[derive(Subcommand)]
#[expect(dead_code, reason = "each command reads its arguments once it exists")]
enum Command {
    /// Create a project.
    New {
        /// The project's name and directory.
        name: String,
        /// Create a library instead of an application.
        #[arg(long)]
        lib: bool,
        /// Start from a template.
        #[arg(long, value_enum)]
        template: Option<Template>,
    },
    /// Build the project into its target directory.
    Build {
        /// Build with the release profile.
        #[arg(long)]
        release: bool,
        /// The target to build for: a built-in name or a description file.
        #[arg(long)]
        target: Option<String>,
        /// Also write an intermediate form.
        #[arg(long, value_enum)]
        emit: Option<Emit>,
    },
    /// Build and run the project.
    Run {
        /// Build with the release profile.
        #[arg(long)]
        release: bool,
        /// Arguments for the program, after `--`.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run every analysis and check, without generating code.
    Check,
    /// Build and run the project's tests.
    Test {
        /// Run only the tests whose name contains this.
        filter: Option<String>,
    },
    /// Format every .mr file in the project.
    Fmt {
        /// Report files that would change instead of changing them.
        #[arg(long)]
        check: bool,
    },
    /// Start the interactive loop.
    Repl,
    /// Explain an error code, with an example of the error and its fix.
    Explain {
        /// A code such as MR0310.
        code: String,
    },
    /// Run the language server on stdin and stdout.
    Lsp,
    /// Turn a mangled symbol back into a Mora name.
    Demangle {
        /// The mangled symbol.
        symbol: String,
    },
    /// Print the toolchain version, commit and installed runtime targets.
    Version,
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::New { .. } => "new",
            Self::Build { .. } => "build",
            Self::Run { .. } => "run",
            Self::Check => "check",
            Self::Test { .. } => "test",
            Self::Fmt { .. } => "fmt",
            Self::Repl => "repl",
            Self::Explain { .. } => "explain",
            Self::Lsp => "lsp",
            Self::Demangle { .. } => "demangle",
            Self::Version => "version",
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let color = color_option(&args);
    let mut command = Cli::command().color(color.into());
    let cli = match command
        .try_get_matches_from_mut(&args)
        .and_then(|matches| Cli::from_arg_matches(&matches))
    {
        Ok(cli) => cli,
        Err(error) => return clap_exit(&error.format(&mut command), color),
    };
    match &cli.command {
        Command::Version => version(cli.message_format, cli.color),
        command => {
            let message = format!("`mora {}` is not implemented yet", command.name());
            report_error(&message, cli.color);
            ExitCode::from(exit::USAGE)
        }
    }
}

/// The `--color` value on the command line, read before clap parses it, so
/// that clap's help and usage errors follow it too. The last one before `--`
/// counts; `auto` when there is none or its value is not valid, which clap
/// then reports.
fn color_option(args: &[OsString]) -> Color {
    let mut color = Color::Auto;
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.to_str() {
            Some("--") => break,
            Some("--color") => args.next().map(OsString::as_os_str),
            Some(arg) => arg.strip_prefix("--color=").map(OsStr::new),
            None => None,
        };
        if let Some(value) = value {
            color = value
                .to_str()
                .and_then(|value| Color::from_str(value, false).ok())
                .unwrap_or(Color::Auto);
        }
    }
    color
}

/// Prints what clap stopped on, in clap's colors as `--color` sets them: help
/// or the version on stdout, a usage error on stderr. `Error::exit` would
/// drop a failed write to stdout and exit 0.
fn clap_exit(error: &clap::Error, color: Color) -> ExitCode {
    let code = ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(exit::USAGE));
    let written = error.print();
    if error.use_stderr() {
        // As in `report_error`, a failed write to stderr cannot be reported.
        code
    } else {
        finish(written.and_then(|()| io::stdout().flush()), code, color)
    }
}

/// Writes `text` and a newline to stdout. Every command prints through here:
/// `println!` panics when the write fails, and a panic exits with 101.
fn write_stdout(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{text}")?;
    stdout.flush()
}

/// The exit code of a command that ends with `code` once its output is
/// written. A reader that closed the pipe early, as `head -1` does, is not a
/// failure; any other write error is one, and is reported on stderr.
fn finish(written: io::Result<()>, code: ExitCode, color: Color) -> ExitCode {
    match written {
        Ok(()) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => code,
        Err(error) => {
            report_error(&format!("cannot write to stdout: {error}"), color);
            ExitCode::from(exit::FAILURE)
        }
    }
}

/// Writes `error: <message>` to stderr. A failed write is dropped, since
/// there is nowhere left to report it.
fn report_error(message: &str, color: Color) {
    let error = paint("error", "1;31", colored(color));
    let _ = writeln!(io::stderr(), "{error}: {message}");
}

/// Whether stderr gets color.
fn colored(color: Color) -> bool {
    match color {
        Color::Always => true,
        Color::Never => false,
        // no-color.org: only a non-empty NO_COLOR turns color off.
        Color::Auto => {
            std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
                && io::stderr().is_terminal()
        }
    }
}

/// `text` in the SGR style `code` when `color` is on.
fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

/// What `mora version` prints as JSON, one object on one line.
#[derive(Serialize)]
struct VersionJson<'a> {
    /// The schema name and version, the first key of every JSON object the
    /// toolchain prints (§17.10).
    schema: &'static str,
    /// The toolchain version.
    version: &'a str,
    /// The commit the toolchain was built from.
    commit: &'a str,
    /// The targets whose runtime library is installed, sorted (§19.2).
    runtime_targets: &'a [String],
}

impl VersionJson<'_> {
    const SCHEMA: &'static str = "mora.version/1";
}

fn version(format: MessageFormat, color: Color) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    let commit = env!("MORA_COMMIT");
    let targets = runtime_targets();
    let text = if format == MessageFormat::Json {
        let json = VersionJson {
            schema: VersionJson::SCHEMA,
            version,
            commit,
            runtime_targets: &targets,
        };
        // Serializing a plain struct of strings cannot fail.
        serde_json::to_string(&json).unwrap_or_default()
    } else {
        let targets = if targets.is_empty() {
            "none".to_owned()
        } else {
            targets.join(", ")
        };
        format!("mora {version}\ncommit: {commit}\nruntime targets: {targets}")
    };
    finish(write_stdout(&text), ExitCode::SUCCESS, color)
}

/// The targets with a runtime library in the toolchain's `lib/rt/` (§19.2),
/// found from the executable's place in `bin/`.
fn runtime_targets() -> Vec<String> {
    let rt = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("lib/rt")))
        .unwrap_or_default();
    let mut targets: Vec<String> = std::fs::read_dir(rt)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("libmora_rt.a").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    targets.sort();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_json_escapes_quotes_backslashes_and_controls() {
        let json = VersionJson {
            schema: VersionJson::SCHEMA,
            version: "1\"2",
            commit: "a\\b",
            runtime_targets: &["t\tx".to_owned(), "\u{1}".to_owned()],
        };
        assert_eq!(
            serde_json::to_string(&json).unwrap(),
            concat!(
                r#"{"schema":"mora.version/1","version":"1\"2","commit":"a\\b","#,
                r#""runtime_targets":["t\tx","\u0001"]}"#
            )
        );
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_color_option_is_found_before_parsing() {
        let color = |args: &[&str]| {
            let args: Vec<OsString> = ["mora"].iter().chain(args).map(OsString::from).collect();
            color_option(&args)
        };
        assert_eq!(color(&[]), Color::Auto);
        assert_eq!(color(&["--color", "never", "--help"]), Color::Never);
        assert_eq!(color(&["--color=always", "check"]), Color::Always);
        assert_eq!(color(&["check", "--color", "never"]), Color::Never);
        assert_eq!(color(&["--color", "always", "--color=never"]), Color::Never);
        assert_eq!(color(&["--color", "purple"]), Color::Auto);
        assert_eq!(color(&["--color"]), Color::Auto);
        assert_eq!(color(&["run", "--", "--color", "always"]), Color::Auto);
        // The program's own name is not an option.
        let args = [OsString::from("--color=always")];
        assert_eq!(color_option(&args), Color::Auto);
    }

    #[cfg(unix)]
    #[test]
    fn an_argument_that_is_not_text_is_skipped() {
        use std::os::unix::ffi::OsStringExt;
        let args = [
            OsString::from("mora"),
            OsString::from("--color=never"),
            OsString::from_vec(vec![0xff, 0xfe]),
        ];
        assert_eq!(color_option(&args), Color::Never);
    }
}
