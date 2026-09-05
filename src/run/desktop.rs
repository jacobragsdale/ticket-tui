//! What the run hands to the desktop: the clipboard and the browser.

use super::*;

pub(super) fn copied_status(content: CopiedContent) -> String {
    format!("Copied {} to clipboard!", content.label())
}

pub(super) fn copy_to_clipboard(text: &str) -> Result<()> {
    first_that_works(clipboard_commands(), text, "clipboard")
}

fn clipboard_commands() -> Vec<Command> {
    if cfg!(target_os = "macos") {
        vec![command("pbcopy", &[])]
    } else {
        vec![
            command("wl-copy", &["--trim-newline"]),
            command("xclip", &["-selection", "clipboard"]),
            command("xsel", &["--clipboard", "--input"]),
        ]
    }
}

fn command(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

/// Runs `commands` in turn, each fed `stdin`, until one exits cleanly. The
/// error reported is the last one's: the command nearest to working.
fn first_that_works(commands: Vec<Command>, stdin: &str, what: &str) -> Result<()> {
    let mut last_error = None;
    for command in commands {
        match write_to_command(command, stdin) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) => Err(error).with_context(|| format!("{what} command failed")),
        None => bail!("no {what} command available"),
    }
}

/// Nothing the child prints reaches the screen: the terminal is in raw mode
/// on the alternate screen, and a stray line would land on the table.
fn write_to_command(mut command: Command, text: &str) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {program}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .with_context(|| format!("failed to write to {program}"))?;
    }
    let status = child.wait().with_context(|| format!("{program} stopped"))?;
    if status.success() {
        Ok(())
    } else {
        bail!("{program} exited with {status}");
    }
}

/// Hands one work item's URL to `opener`, which is the system launcher outside
/// the tests. Only HTTPS goes out: a stored URL is data, and a `file:` or
/// `javascript:` one is not a ticket.
pub(super) fn open_https_url(raw_url: &str, opener: &dyn Fn(&Url) -> Result<()>) -> Result<()> {
    let url = Url::parse(raw_url).context("ticket URL is invalid")?;
    if url.scheme() != "https" {
        bail!("only HTTPS ticket URLs can be opened");
    }
    opener(&url).context("system URL launcher failed")
}

pub(super) fn open_in_browser(url: &Url) -> Result<()> {
    first_that_works(browser_commands(url, is_wsl()), "", "browser")
}

/// The launchers to try, in order. Under WSL the URL goes to Windows, whose
/// default browser is the one the user means; `xdg-open` there is usually
/// missing and, with no desktop behind it, can exec a terminal browser onto
/// our screen, so it comes last. `cmd.exe` gets the URL bare with its
/// operators escaped — `start "…"` reads a quoted first argument as a window
/// title — and PowerShell gets it single-quoted, where only `'` is special.
pub(super) fn browser_commands(url: &Url, wsl: bool) -> Vec<Command> {
    let mut commands = Vec::new();
    if wsl {
        commands.push(command(
            "cmd.exe",
            &["/c", "start", &cmd_escape(url.as_str())],
        ));
        commands.push(command(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Start-Process -FilePath '{}'",
                    url.as_str().replace('\'', "''")
                ),
            ],
        ));
    }
    let launcher = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    commands.push(command(launcher, &[url.as_str()]));
    commands
}

/// What cmd.exe would read as an operator gets its caret: `&` ends a command,
/// `|` pipes it, `<` and `>` redirect, and `^` is the escape itself. Pipeline
/// run URLs carry `&` in their query, so this is not a corner.
pub(super) fn cmd_escape(url: &str) -> String {
    url.chars()
        .fold(String::with_capacity(url.len()), |mut out, c| {
            if "^&|<>".contains(c) {
                out.push('^');
            }
            out.push(c);
            out
        })
}

/// WSL 1 reports `…-Microsoft`, WSL 2 `…-microsoft-standard-WSL2`. The file
/// is read rather than `WSL_DISTRO_NAME`, which an ssh session does not carry.
fn is_wsl() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"))
}
