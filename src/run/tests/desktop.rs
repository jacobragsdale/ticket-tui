//! The URL launcher and the pointer shape the terminal is told to draw.

use super::*;

#[test]
fn only_well_formed_https_urls_reach_the_launcher() {
    let error = open_https_url("file:///tmp/not-a-ticket", &failing_opener).unwrap_err();
    assert!(error.to_string().contains("only HTTPS"), "{error}");
    let error = open_https_url("not a url", &failing_opener).unwrap_err();
    assert!(error.to_string().contains("invalid"), "{error}");
    let error = open_https_url("https://dev.azure.com/demo", &failing_opener).unwrap_err();
    assert!(
        error.to_string().contains("system URL launcher failed"),
        "{error}"
    );
}

#[test]
fn mouse_pointer_sequences_set_and_reset_link_hover() {
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::OpenSelectedUrl), None),
        MousePointerShape::Link
    );
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::OpenInBrowser { index: 0 }), None),
        MousePointerShape::Link
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::EditField {
                field: ticket_tui::pointer::EditableField::State
            }),
            None
        ),
        MousePointerShape::Link,
        "an editable details field points the same way a link does"
    );
    assert_eq!(
        mouse_pointer_for_hover(Some(&PointerTarget::TableRow { index: 0 }), None),
        MousePointerShape::Default
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::PaneDivider {
                split: ticket_tui::pointer::PaneSplit::Workspace
            }),
            Some(DividerOrientation::Vertical)
        ),
        MousePointerShape::ColResize
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::PaneDivider {
                split: ticket_tui::pointer::PaneSplit::Workspace
            }),
            Some(DividerOrientation::Horizontal)
        ),
        MousePointerShape::RowResize
    );
    assert_eq!(
        mouse_pointer_for_hover(
            Some(&PointerTarget::PaneDivider {
                split: ticket_tui::pointer::PaneSplit::Workspace
            }),
            None
        ),
        MousePointerShape::Default,
        "the narrow layout has no divider to resize"
    );

    let mut output = Vec::new();
    write_mouse_pointer_shape(&mut output, MousePointerShape::Link).unwrap();
    write_mouse_pointer_shape(&mut output, MousePointerShape::ColResize).unwrap();
    write_mouse_pointer_shape(&mut output, MousePointerShape::RowResize).unwrap();
    write_mouse_pointer_shape(&mut output, MousePointerShape::Default).unwrap();

    assert_eq!(
        output,
        b"\x1b]22;pointer\x1b\\\x1b]22;col-resize\x1b\\\x1b]22;row-resize\x1b\\\x1b]22;\x1b\\"
    );
}

#[test]
fn cmd_start_escapes_shell_metacharacters() {
    assert_eq!(
        cmd_escape("https://x/?a=1&b=2^c|d<e>f"),
        "https://x/?a=1^&b=2^^c^|d^<e^>f"
    );
}

#[test]
fn wsl_hands_the_url_to_windows_first() {
    let url = Url::parse("https://dev.azure.com/demo/atlas/_build/results?buildId=7&view=results")
        .unwrap();
    let native = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let programs = |commands: &[Command]| -> Vec<String> {
        commands
            .iter()
            .map(|command| command.get_program().to_string_lossy().into_owned())
            .collect()
    };
    let args = |command: &Command| -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    };

    let wsl = browser_commands(&url, true);
    assert_eq!(programs(&wsl), ["cmd.exe", "powershell.exe", native]);
    assert_eq!(
        args(&wsl[0]),
        [
            "/c",
            "start",
            "https://dev.azure.com/demo/atlas/_build/results?buildId=7^&view=results"
        ],
        "cmd.exe gets the URL bare, with & escaped so start sees all of it"
    );
    assert_eq!(
        args(&wsl[1]),
        [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Process -FilePath 'https://dev.azure.com/demo/atlas/_build/results?buildId=7&view=results'"
        ]
    );
    assert_eq!(args(&wsl[2]), [url.as_str()]);

    assert_eq!(programs(&browser_commands(&url, false)), [native]);
}
