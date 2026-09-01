//! The Key Vault tab: the subscription's vaults, or the contents of the one
//! chosen, on the left; and what the details pane says about the row under the
//! cursor on the right — a vault's fields, or one item's dates and the chips
//! that act on it.
//!
//! One line here reads a secret out: [`render_revealed`], which is the whole
//! point of the tab and the only place on screen a value ever appears.

use super::*;
use crate::app::key_vault::{
    Expiry, ItemColumn, ItemRow, KeyVaultScreen, Level, VaultColumn, VaultMode, VaultRow,
};
use crate::arm::ItemKind;
use crate::command::CommandId;
use crate::ui::details::section_line;
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// What the dots stand in for while a value is being read. Eight of them
/// whatever the secret's length, which is one thing less the screen gives away.
const MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    screen: &mut KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_screen_status_bar(frame, screen, shell, sections[2]);
}

fn render_search(frame: &mut Frame<'_>, screen: &KeyVaultScreen, shell: &mut Shell, area: Rect) {
    let (placeholder, trailer) = match screen.level() {
        Level::Vaults => ("Type / to search vaults, or rg:, location:", String::new()),
        // The one query worth spelling out: what is about to lapse.
        Level::Items(name) => (
            "Type / to search, or kind:cert, enabled:no, expires:<+30d",
            format!("\u{2022} {name}"),
        ),
    };
    render_search_row(
        frame,
        shell,
        SearchRow {
            area,
            text: screen.query(),
            cursor: screen.query_cursor(),
            placeholder,
            active: screen.mode == VaultMode::Search,
            pending: false,
            clearable: false,
            trailer,
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

fn render_content(
    frame: &mut Frame<'_>,
    screen: &mut KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
) {
    struct Panes<'a>(&'a mut KeyVaultScreen);
    impl PanePair for Panes<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            match self.0.level() {
                Level::Vaults => render_vault_table(frame, self.0, shell, area),
                Level::Items(_) => render_item_table(frame, self.0, shell, area),
            }
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_details(frame, self.0, shell, area);
        }
    }
    // The list is whichever level is open, and the chips the narrow layout
    // wears say which.
    let list = match screen.level() {
        Level::Vaults => "Vaults".to_owned(),
        Level::Items(name) => name.clone(),
    };
    let details = if screen.selected_item().is_some() {
        "Item"
    } else {
        "Vault"
    };
    render_workspace(
        frame,
        shell,
        area,
        &PaneNames {
            list: &list,
            details,
        },
        &mut Panes(screen),
    );
}

fn render_vault_table(
    frame: &mut Frame<'_>,
    screen: &mut KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let rows = screen.visible_vaults();
    let geometry = table_geometry(area, 1);
    screen
        .vault_cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.vault_cursor.scroll.offset;
    let (sorted, descending) = screen.vault_sort;
    let layout = screen.vaults_layout.clone();
    let status = format!("{} vaults", rows.len());
    let mut cell = |index: usize, column: VaultColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| vault_cell(row, column))
    };
    let mut spec = TableSpec {
        title: " Vaults ".to_owned(),
        status,
        focused: shell.focus == Focus::Tickets,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.vault_cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

fn render_item_table(
    frame: &mut Frame<'_>,
    screen: &mut KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let now = Timestamp::now();
    let rows = screen.visible_items();
    let geometry = table_geometry(area, 1);
    screen
        .item_cursor
        .scroll
        .set_viewport(geometry.visible_rows, rows.len());
    let offset = screen.item_cursor.scroll.offset;
    let (sorted, descending) = screen.item_sort;
    let layout = screen.items_layout.clone();
    let title = screen
        .open_vault()
        .map_or_else(|| " Items ".to_owned(), |vault| format!(" {} ", vault.name));
    let mut cell = |index: usize, column: ItemColumn| {
        rows.get(index)
            .map_or_else(|| Cell::from(""), |row| item_cell(row, column, now))
    };
    let mut spec = TableSpec {
        title,
        status: item_status(&rows),
        focused: shell.focus == Focus::Tickets,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: rows.len(),
        offset,
        selected: Some(screen.item_cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

/// What the bottom border says: how many rows, and how many of them are on
/// their way out.
fn item_status(rows: &[ItemRow]) -> String {
    let now = Timestamp::now();
    let expiring = rows
        .iter()
        .filter(|row| Expiry::of(row.item.expires, now).is_some())
        .count();
    if expiring == 0 {
        format!("{} items", rows.len())
    } else {
        format!("{} items \u{00b7} {expiring} expiring", rows.len())
    }
}

fn vault_cell(row: &VaultRow, column: VaultColumn) -> Cell<'static> {
    match column {
        VaultColumn::Name => Cell::from(row.vault.name.clone()),
        VaultColumn::ResourceGroup => Cell::from(row.vault.resource_group.clone()),
        VaultColumn::Location => Cell::from(row.vault.location.clone()),
        VaultColumn::Sku => Cell::from(row.vault.sku.clone()),
    }
}

/// One row of the item table. A disabled item is faded whole — it is not in
/// use, and its dates are nobody's problem — and an expiry the clock has
/// caught up with is coloured instead.
fn item_cell(row: &ItemRow, column: ItemColumn, now: Timestamp) -> Cell<'static> {
    let tone = if row.item.enabled {
        RowTone::Normal
    } else {
        RowTone::Muted
    };
    let text = match column {
        ItemColumn::Kind => row.item.kind.as_str().to_owned(),
        ItemColumn::Name => row.item.name.clone(),
        ItemColumn::Enabled => if row.item.enabled { "yes" } else { "no" }.to_owned(),
        ItemColumn::Updated => row
            .item
            .updated
            .map_or_else(String::new, |at| at.exact_utc()),
        ItemColumn::Expires => row
            .item
            .expires
            .map_or_else(|| "\u{2014}".to_owned(), |at| at.exact_utc()),
    };
    let mut style = Style::default();
    if column == ItemColumn::Expires
        && tone == RowTone::Normal
        && let Some(colour) = expiry_colour(row.item.expires, now)
    {
        style = style.fg(colour);
    }
    Cell::from(Line::styled(text, tone.apply(style)))
}

/// What an expiry is painted: red once it is past, amber inside the month
/// before that, and the row's own colour beyond it.
fn expiry_colour(expires: Option<Timestamp>, now: Timestamp) -> Option<Color> {
    match Expiry::of(expires, now)? {
        Expiry::Past => Some(theme().error),
        Expiry::Soon => Some(theme().warning),
    }
}

fn render_details(
    frame: &mut Frame<'_>,
    screen: &mut KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
) {
    match screen.selected_item() {
        Some(row) => render_item_details(frame, screen, shell, area, &row),
        None => render_vault_details(frame, screen, shell, area),
    }
}

/// The vault under the cursor: what it is, where it is, and the way into it in
/// the portal.
fn render_vault_details(
    frame: &mut Frame<'_>,
    screen: &KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
) {
    let block =
        focused_block(" Vault ", shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(row) = screen.selected_vault() else {
        frame.render_widget(
            Paragraph::new(nothing_selected(screen, shell))
                .style(Style::default().fg(theme().muted))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    };
    let mut lines = vec![
        Line::styled(
            row.vault.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(row.vault.uri.clone(), Style::default().fg(theme().accent)),
        Line::from(""),
        field_line("Group", row.vault.resource_group.clone()),
        field_line("Location", row.vault.location.clone()),
        field_line("SKU", row.vault.sku.clone()),
        field_line(
            "Items",
            row.items
                .map_or_else(|| "\u{2014}".to_owned(), |count| count.to_string()),
        ),
        Line::from(""),
    ];
    let portal = lines.len();
    lines.push(portal_line(&row.vault.id));
    push_problem(&mut lines, screen, inner.width);
    // A portal link wraps, so its target is placed by the row its line landed
    // on rather than by the line's index.
    let (rows, _) = wrapped_rows(&lines, inner.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    if let Some(y) = row_on_screen(inner, &rows, portal, 0) {
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::RunCommand(CommandId::Open),
            PointerLayer::Base,
            None,
            None,
        ));
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, true);
}

/// The item under the cursor: its dates, the chips that act on it, and — while
/// somebody has asked for it and the minute has not run out — its value.
// ponytail: no versions call; one more data-plane read per item if anyone asks.
fn render_item_details(
    frame: &mut Frame<'_>,
    screen: &KeyVaultScreen,
    shell: &mut Shell,
    area: Rect,
    row: &ItemRow,
) {
    let now = Timestamp::now();
    let block =
        focused_block(" Item ", shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut lines = vec![
        Line::styled(
            row.item.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            row.item.kind.as_str().to_owned(),
            Style::default().fg(theme().accent),
        ),
        Line::from(""),
        field_line("Enabled", if row.item.enabled { "yes" } else { "no" }),
        field_line(
            "Content type",
            row.item
                .content_type
                .clone()
                .unwrap_or_else(|| "\u{2014}".to_owned()),
        ),
        field_line("Created", stamp_label(row.item.created, now)),
        field_line("Updated", stamp_label(row.item.updated, now)),
        expires_line(row, now),
        field_line(
            "Recovery",
            row.item
                .recovery_level
                .clone()
                .unwrap_or_else(|| "\u{2014}".to_owned()),
        ),
        Line::from(""),
    ];
    // Only a secret has a value, so only a secret is offered the key that
    // shows one.
    let mut buttons: Vec<(&str, PointerTarget)> = Vec::new();
    if row.item.kind == ItemKind::Secret {
        buttons.push((
            " Reveal ",
            PointerTarget::RunCommand(CommandId::RevealSecret),
        ));
    }
    buttons.push((" Copy name ", PointerTarget::RunCommand(CommandId::CopyId)));
    buttons.push((" Open ", PointerTarget::RunCommand(CommandId::Open)));
    // The value shown is this row's secret and nobody else's: a key by the
    // same name has none.
    let shown = screen.revealed().filter(|revealed| {
        revealed.name == row.item.name
            && revealed.vault == row.vault
            && row.item.kind == ItemKind::Secret
    });
    if shown.is_some() {
        buttons.push((
            " Copy value ",
            PointerTarget::RunCommand(CommandId::CopyValue),
        ));
    }
    let buttons_index = lines.len();
    lines.push(button_row(&buttons));
    if screen.reveal_pending() {
        lines.push(Line::styled(MASK, Style::default().fg(theme().muted)));
    } else if let Some(revealed) = shown {
        lines.push(render_revealed(revealed));
    }
    push_problem(&mut lines, screen, inner.width);
    // A long name or a refusal wraps, so the chips are placed by the row their
    // line landed on rather than by the line's index.
    let (rows, _) = wrapped_rows(&lines, inner.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    if let Some(y) = row_on_screen(inner, &rows, buttons_index, 0) {
        register_buttons(shell, inner, y, PointerLayer::Base, &buttons);
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, true);
}

/// The one line that reads a value out, and it says how long it has left.
fn render_revealed(revealed: crate::app::key_vault::Revealed<'_>) -> Line<'static> {
    // Rounded up, so the count opens on the minute it was given rather than a
    // second short of it, and never reads nought while the value is still up.
    let seconds = revealed.clears_in.as_millis().div_ceil(1000);
    Line::from(vec![
        Span::styled(
            revealed.value.expose().to_owned(),
            Style::default().fg(theme().accent),
        ),
        Span::styled(
            format!("  clears in {seconds}s"),
            Style::default().fg(theme().muted),
        ),
    ])
}

/// `Expires`, coloured the way the table's own cell is.
fn expires_line(row: &ItemRow, now: Timestamp) -> Line<'static> {
    // A disabled item's running out is nobody's business, in the pane as
    // in the table: it reads muted, whatever the date.
    let value = Span::styled(
        stamp_label(row.item.expires, now),
        match expiry_colour(row.item.expires, now) {
            Some(colour) if row.item.enabled => Style::default().fg(colour),
            Some(_) => Style::default().fg(theme().muted),
            None => Style::default(),
        },
    );
    Line::from(vec![field_label("Expires"), value])
}

/// `2026-09-10 09:00:00 UTC (in 12 days)`, and a dash for a date nobody set.
fn stamp_label(at: Option<Timestamp>, now: Timestamp) -> String {
    at.map_or_else(
        || "\u{2014}".to_owned(),
        |at| format!("{} ({})", at.exact_utc(), relative_wording(at, now)),
    )
}

/// How far off an instant is, in words that say which side of now it falls:
/// `in 12 days`, `3 days ago`.
fn relative_wording(at: Timestamp, now: Timestamp) -> String {
    let ahead = now.seconds_until(at);
    if ahead > 0 {
        return format!("in {}", span_label(ahead));
    }
    let behind = at.seconds_until(now);
    if behind > 0 {
        format!("{} ago", span_label(behind))
    } else {
        "now".to_owned()
    }
}

/// A span in the largest unit that fits, with a plural that reads.
fn span_label(seconds: i64) -> String {
    let (count, unit) = match seconds {
        0..=3599 => (seconds / 60, "minute"),
        3600..=86_399 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

fn portal_line(id: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("Portal: ", Style::default().fg(theme().muted)),
        Span::styled(
            crate::arm::portal_url(id),
            Style::default()
                .fg(theme().link)
                .add_modifier(Modifier::UNDERLINED),
        ),
    ])
}

/// The refusal standing, under a rule of its own, on whichever pane is drawn.
fn push_problem(lines: &mut Vec<Line<'static>>, screen: &KeyVaultScreen, width: u16) {
    if let Some(error) = screen.arm_error() {
        lines.push(Line::from(""));
        lines.push(section_line("Problem", width));
        lines.push(Line::styled(
            error.to_owned(),
            Style::default().fg(theme().error),
        ));
    }
}

/// What the pane says with no vault under the cursor: why ARM cannot be
/// reached, what refused, or that nothing has come back yet.
fn nothing_selected(screen: &KeyVaultScreen, shell: &Shell) -> Vec<Line<'static>> {
    if let Some(reason) = shell.arm_state() {
        return vec![Line::from(reason.to_owned())];
    }
    if let Some(error) = screen.arm_error() {
        return vec![Line::from(error.to_owned())];
    }
    vec![Line::from("Reading the subscription\u{2026}")]
}
