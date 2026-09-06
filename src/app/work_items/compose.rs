//! The composer: the details pane's own editor for what is long-form — the
//! acceptance criteria and a comment — typed where it is read rather than in
//! an overlay or `$EDITOR`. `Enter` is a newline, `Ctrl-S` saves, and `Esc`
//! keeps the draft for the next time the same thing is opened.

use super::*;
use crate::markdown;
use crate::text_input::wrap_with_cursor;

/// The open composer: what it is editing, on which work item, and the text.
#[derive(Clone, Debug)]
pub struct Composer {
    pub key: TicketKey,
    pub target: ComposeTarget,
    pub input: TextInput,
    /// The Markdown the composer opened on. Saving it back unchanged writes
    /// nothing, and closing on it keeps no draft.
    original: String,
    /// The width the rows were last wrapped to, which is what `↑`/`↓` move by.
    /// Zero until the pane has drawn it once.
    pub width: u16,
    /// Whether the next frame scrolls the pane to the caret: set by every key
    /// the composer takes and cleared by the frame that obeyed it, so the
    /// wheel can still scroll away from it in between.
    pub follow_cursor: bool,
}

impl Composer {
    /// Whether what is typed says anything the work item does not already.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        match self.target {
            ComposeTarget::NewComment => !self.input.text().trim().is_empty(),
            ComposeTarget::Description | ComposeTarget::AcceptanceCriteria => {
                markdown::saved_markdown(self.input.text())
                    != markdown::saved_markdown(&self.original)
            }
        }
    }

    /// What the footer says while the composer is open.
    #[must_use]
    pub const fn hint(&self) -> &'static str {
        match self.target {
            ComposeTarget::NewComment => "Enter newline  Ctrl-S post  Esc keep draft  Ctrl-U clear",
            ComposeTarget::Description | ComposeTarget::AcceptanceCriteria => {
                "Enter newline  Ctrl-S save  Esc keep draft  Ctrl-U clear"
            }
        }
    }
}

impl ComposeTarget {
    /// What the notifications call it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Description => "description",
            Self::AcceptanceCriteria => "acceptance criteria",
            Self::NewComment => "comment",
        }
    }
}

impl WorkItemsScreen {
    /// Opens the composer on one section of the selected work item: on the
    /// draft kept for it when there is one, and on what the work item says
    /// otherwise. Only the refusal worth making before somebody spends
    /// minutes typing is made here.
    pub(super) fn open_composer(&mut self, shell: &mut Shell, target: ComposeTarget) {
        let Some(ticket) = self.selected_ticket() else {
            shell.set_error("No work item is selected");
            return;
        };
        let key = ticket.key.clone();
        let original = match target {
            ComposeTarget::Description => markdown::description_document(&ticket.description_html),
            ComposeTarget::AcceptanceCriteria => {
                markdown::description_document(&ticket.acceptance_criteria_html)
            }
            ComposeTarget::NewComment => String::new(),
        };
        if let Some(reason) = shell.write_refusal() {
            let verb = match target {
                ComposeTarget::NewComment => "posted",
                ComposeTarget::Description | ComposeTarget::AcceptanceCriteria => "saved",
            };
            shell.set_error(format!(
                "#{} {} not {verb}: {reason}",
                key.id,
                target.label()
            ));
            return;
        }
        let text = self
            .drafts
            .remove(&(key.clone(), target))
            .unwrap_or_else(|| original.clone());
        self.composer = Some(Composer {
            key,
            target,
            input: TextInput::new(text),
            original,
            width: 0,
            follow_cursor: true,
        });
        self.mode = WorkItemMode::Compose;
    }

    pub(super) fn handle_compose_key(&mut self, shell: &mut Shell, key: KeyEvent) -> AppAction {
        match key.code {
            KeyCode::Esc => self.close_composer(shell),
            KeyCode::Tab => {
                self.close_composer(shell);
                shell.toggle_focus();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.submit_composer(shell);
            }
            _ => {
                if let Some(composer) = self.composer.as_mut() {
                    let width = usize::from(composer.width);
                    match key.code {
                        KeyCode::Enter => composer.input.insert_newline(),
                        KeyCode::Up => composer.input.move_up(width),
                        KeyCode::Down => composer.input.move_down(width),
                        _ => {
                            composer.input.handle_key(key);
                        }
                    }
                    composer.follow_cursor = true;
                }
            }
        }
        AppAction::None
    }

    /// Closes the composer, keeping what was typed for the next time the same
    /// section of the same work item is opened. A draft that says nothing new
    /// is not kept.
    pub(super) fn close_composer(&mut self, shell: &mut Shell) {
        if let Some(composer) = self.composer.take()
            && composer.is_dirty()
        {
            shell.set_status(format!("Draft kept on #{}", composer.key.id));
            let text = composer.input.text().to_owned();
            self.drafts.insert((composer.key, composer.target), text);
        }
        self.mode = WorkItemMode::Browse;
    }

    /// Saves what the composer holds: the acceptance criteria go down the
    /// field-edit path as HTML, a comment is posted. An empty comment is
    /// refused with the composer left open, criteria saved back as they were
    /// close without a write, and a write refused before it went out keeps
    /// the text as a draft rather than losing it.
    pub(super) fn submit_composer(&mut self, shell: &mut Shell) -> AppAction {
        let Some(composer) = self.composer.take() else {
            self.mode = WorkItemMode::Browse;
            return AppAction::None;
        };
        let text = composer.input.text().to_owned();
        if composer.target == ComposeTarget::NewComment && text.trim().is_empty() {
            shell.set_error(format!("#{} comment cannot be empty", composer.key.id));
            self.composer = Some(composer);
            return AppAction::None;
        }
        self.mode = WorkItemMode::Browse;
        let key = composer.key.clone();
        let action = match composer.target {
            ComposeTarget::NewComment => self.comment_on(shell, &key, text.trim().to_owned()),
            ComposeTarget::Description | ComposeTarget::AcceptanceCriteria => {
                let saved = markdown::saved_markdown(&text);
                if saved == markdown::saved_markdown(&composer.original) {
                    shell.set_status(format!("#{} {} unchanged", key.id, composer.target.label()));
                    return AppAction::None;
                }
                let html = markdown::markdown_to_html(&saved);
                let edit = match composer.target {
                    ComposeTarget::Description => FieldEdit::description(&html),
                    _ => FieldEdit::acceptance_criteria(&html),
                };
                self.edit_ticket(shell, &key, edit)
            }
        };
        if action == AppAction::None {
            self.drafts.insert((key, composer.target), text);
        }
        action
    }

    /// Puts the composer's caret where a click landed on one of its rows:
    /// `row` is the wrapped row, `column` the cell along it.
    pub(super) fn place_composer_caret(&mut self, row: usize, column: u16) {
        let Some(composer) = self.composer.as_mut() else {
            return;
        };
        let layout = wrap_with_cursor(
            composer.input.text(),
            composer.input.cursor(),
            usize::from(composer.width),
        );
        if let Some((start, text)) = layout.rows.get(row) {
            composer
                .input
                .set_cursor(start + usize::from(column).min(text.chars().count()));
            composer.follow_cursor = true;
        }
    }

    /// The details-pane section the pointer is resting on, which is what
    /// `Enter` opens the composer on while that pane is focused.
    #[must_use]
    pub(super) fn pointed_compose_target(&self, shell: &Shell) -> Option<ComposeTarget> {
        match shell.hovered_region().map(|region| &region.target) {
            Some(PointerTarget::Compose(target)) => Some(*target),
            _ => None,
        }
    }
}
