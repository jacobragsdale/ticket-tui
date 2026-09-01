//! The details pane: the fields of the selected work item, its family
//! tree, its history and its comments.

use super::*;

/// The details pane is one scrolling document: the heading, the family tree,
/// Planning, Description, History, and Comments are lines of a single
/// paragraph, so the title scrolls away with everything under it and the
/// scrollbar measures the whole pane.
pub(super) fn render_details(
    frame: &mut Frame<'_>,
    screen: &mut WorkItemsScreen,
    shell: &mut Shell,
    area: Rect,
) {
    // `pane` is everything inside the border — what the wheel scrolls and
    // where the scrollbar's own column is; `inner` is the padded column the
    // text is laid in, one in from each side.
    let block =
        focused_block(" Details ", shell.focus.is_details_pane()).padding(Padding::horizontal(1));
    let pane = inside_border(area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    shell.hit_regions.push(region(
        pane,
        PointerTarget::FocusDetails,
        PointerLayer::Base,
        Some(SelectableSurface::Details),
        Some(ScrollSurface::Details),
    ));

    let Some(ticket) = screen.selected_ticket().cloned() else {
        // Nothing scrollable this frame; keep the measured height.
        let viewport = screen.details.viewport;
        screen.details.set_viewport(viewport, 0);
        frame.render_widget(
            Paragraph::new("Select a ticket to view details")
                .style(Style::default().fg(theme().muted)),
            inner,
        );
        return;
    };
    if inner.width == 0 || inner.height == 0 {
        // Nothing scrollable this frame; keep the measured height.
        let viewport = screen.details.viewport;
        screen.details.set_viewport(viewport, 0);
        return;
    }

    let family = screen.family_of(&ticket.key);
    let has_family = family.has_family();
    let width = inner.width;
    let cursor = screen.family_cursor.clone();
    let family_focused = shell.focus == Focus::Family;
    let mut highlighter = QueryHighlighter::new(screen.query());
    let title_style = Style::default()
        .fg(theme().text)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::new();
    let mut family_hits: Vec<FamilyHit> = Vec::new();
    let mut line_links: Vec<(u16, TicketKey)> = Vec::new();
    // The artifact lines that go somewhere: the line and where.
    let mut artifact_links: Vec<(u16, Jump)> = Vec::new();
    // Every click target is a line of the one paragraph, so each is recorded
    // against its logical line and placed once the scroll offset is known.
    let mut field_hits: Vec<(u16, EditableField, u16, u16)> = Vec::new();
    let mut tree_start: Option<u16> = None;

    lines.push(highlight_line(
        ticket.title.clone(),
        &highlighter.indices(&ticket.title),
        title_style,
        search_match_style(title_style),
    ));
    lines.push(ticket_badge_line(
        &ticket,
        shell.is_mine(&ticket),
        &mut highlighter,
    ));
    if has_family {
        lines.push(family_breadcrumb_line(screen, &family));
    }
    lines.push(tags_field_line(&ticket.tags, &mut highlighter));
    lines.push(field_line(
        "Project",
        format!(
            "{} / {} · r{}",
            ticket.key.organization, ticket.project, ticket.revision
        ),
    ));
    for span in metadata_field_spans(&ticket, has_family) {
        field_hits.push((span.line, span.field, span.x, span.width));
    }
    // Below the editable fields, so nothing a click aims at moves when a
    // parent gains this line and a childless work item does without it.
    if let Some(progress) = screen.child_progress(&ticket.key) {
        lines.push(child_progress_line(progress));
    }
    let url_line = u16::try_from(lines.len()).ok();
    lines.push(link_line(ticket.web_url.clone()));
    // The chip that stands for `g`: what carried this work item, when
    // anything did. It follows on a click like the artifact lines below.
    if let Some((line, jump)) = follow_chip(&*screen, shell)
        && let Ok(index) = u16::try_from(lines.len())
    {
        artifact_links.push((index, jump));
        lines.push(line);
    }
    lines.push(Line::default());

    if has_family {
        lines.push(family_section_line(
            screen.child_progress(&ticket.key),
            family_focused,
            width,
        ));
        for entry in screen.visible_family_tree() {
            let related = screen.ticket_by_key(&entry.key);
            let is_cursor = family_focused && cursor.as_ref() == Some(&entry.key);
            let progress = screen.child_progress(&entry.key);
            let line = family_tree_line(&entry, related, progress, is_cursor, width);
            if let Ok(index) = u16::try_from(lines.len()) {
                tree_start.get_or_insert(index);
                family_hits.push(FamilyHit {
                    line: index,
                    key: entry.key.clone(),
                    jumpable: related.is_some(),
                });
            }
            lines.push(line);
        }
        for parent in &family.extra_parents {
            let related = screen.ticket_by_key(parent);
            let is_cursor = family_focused && cursor.as_ref() == Some(parent);
            if let Ok(index) = u16::try_from(lines.len())
                && related.is_some()
            {
                line_links.push((index, parent.clone()));
            }
            lines.push(family_member_line(
                "  also ", parent, related, false, is_cursor, width,
            ));
        }
        lines.push(Line::default());
    }

    // What the work item was worked on with. A link the database holds is a
    // jump; one it does not is a plain line, because there is nothing here to
    // open — `o` still opens the work item, and the browser has the rest.
    let artifacts = screen.artifacts_for(&ticket.key);
    if !artifacts.is_empty() {
        lines.push(section_line("Related", width));
        for artifact in &artifacts {
            let (line, jump) = artifact_line(artifact, shell);
            if let (Ok(index), Some(jump)) = (u16::try_from(lines.len()), jump.clone()) {
                artifact_links.push((index, jump));
            }
            lines.push(line);
        }
        lines.push(Line::default());
    }

    lines.push(section_line("Planning", width));
    if let Ok(line) = u16::try_from(lines.len()) {
        field_hits.push((
            line,
            EditableField::Area,
            FIELD_VALUE_COLUMN,
            columns(&ticket.area_path),
        ));
    }
    lines.push(highlighted_field_line(
        "Area",
        &ticket.area_path,
        &mut highlighter,
    ));
    if let Ok(line) = u16::try_from(lines.len()) {
        field_hits.push((
            line,
            EditableField::Iteration,
            FIELD_VALUE_COLUMN,
            columns(&ticket.iteration_path),
        ));
    }
    lines.push(highlighted_field_line(
        "Iteration",
        &ticket.iteration_path,
        &mut highlighter,
    ));
    lines.push(field_line("Created", ticket.created_at.exact_utc()));
    lines.push(changed_field_line(&ticket, screen.stale_age_days(&ticket)));

    lines.push(Line::default());
    lines.push(section_line("Description", width));
    if let Some(reason) = ticket.reason.as_deref() {
        lines.push(field_line("Reason", reason));
        lines.push(Line::default());
    }
    if ticket.description.is_empty() {
        lines.push(Line::styled(
            "No description",
            Style::default().fg(theme().muted),
        ));
    } else {
        lines.extend(
            ticket
                .description
                .lines()
                .map(|line| Line::from(line.to_owned())),
        );
    }

    let history = screen.history_for(&ticket.key);
    let loading_details = screen.details_pending.as_ref() == Some(&ticket.key);
    if loading_details || !history.is_empty() {
        let now = OffsetDateTime::now_utc();
        lines.push(Line::default());
        lines.push(section_line("History", width));
        if loading_details {
            lines.push(Line::styled(
                format!("  {} Loading comments and history", spinner_frame()),
                Style::default().fg(theme().muted),
            ));
        }
        lines.extend(history.into_iter().map(|entry| history_line(entry, now)));
    }
    let comments = screen.comments_for(&ticket.key);
    if !comments.is_empty() {
        lines.push(Line::default());
        lines.push(section_line("Comments", width));
        for comment in comments {
            let who = comment.author.as_deref().unwrap_or("unknown");
            lines.push(Line::from(format!(
                "  {who} · {}",
                comment.created_at.exact_utc()
            )));
            lines.extend(comment.text.lines().map(|line| {
                Line::styled(format!("    {line}"), Style::default().fg(theme().body))
            }));
        }
    }

    // Wrapping moves every line under a long one down, so the click targets
    // are placed on the rows the paragraph actually draws them on.
    let last_hit = field_hits
        .iter()
        .map(|(line, ..)| *line)
        .chain(family_hits.iter().map(|hit| hit.line))
        .chain(line_links.iter().map(|(line, _)| *line))
        .chain(artifact_links.iter().map(|(line, _)| *line))
        .chain(url_line)
        .max();
    let rows = last_hit.map_or_else(Vec::new, |last| {
        wrapped_row_starts(&lines, width, usize::from(last).saturating_add(1))
    });
    screen.details_family_row = tree_start
        .and_then(|line| rows.get(usize::from(line)).copied())
        .map_or(0, usize::from);

    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(theme().body));
    let line_count = paragraph.line_count(width);
    let viewport = usize::from(inner.height);
    screen.details.set_viewport(viewport, line_count);
    let scroll = screen.details.offset;
    let scroll_rows = u16::try_from(scroll).unwrap_or(u16::MAX);
    frame.render_widget(paragraph.scroll((scroll_rows, 0)), inner);

    let row_of = |logical: u16| -> Option<u16> {
        let row = rows.get(usize::from(logical)).copied()?;
        visible_row_y(inner, row, scroll_rows)
    };
    if let Some(y) = url_line.and_then(row_of) {
        shell.hit_regions.push(region(
            Rect::new(inner.x, y, inner.width, 1),
            PointerTarget::OpenSelectedUrl,
            PointerLayer::Base,
            Some(SelectableSurface::Details),
            Some(ScrollSurface::Details),
        ));
    }
    for hit in family_hits {
        if !hit.jumpable {
            continue;
        }
        if let Some(y) = row_of(hit.line) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::Follow(Jump::WorkItem(hit.key.clone())),
                PointerLayer::Base,
                Some(SelectableSurface::Details),
                Some(ScrollSurface::Details),
            ));
        }
    }
    for (logical, key) in line_links {
        if let Some(y) = row_of(logical) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::Follow(Jump::WorkItem(key)),
                PointerLayer::Base,
                Some(SelectableSurface::Details),
                Some(ScrollSurface::Details),
            ));
        }
    }
    for (logical, jump) in artifact_links {
        if let Some(y) = row_of(logical) {
            shell.hit_regions.push(region(
                Rect::new(inner.x, y, inner.width, 1),
                PointerTarget::Follow(jump),
                PointerLayer::Base,
                Some(SelectableSurface::Details),
                Some(ScrollSurface::Details),
            ));
        }
    }
    for (logical, field, x, span_width) in field_hits {
        if let Some(y) = row_of(logical) {
            register_edit_field(shell, inner, field, y, x, span_width);
        }
    }
    let overflow = line_count > viewport;
    if overflow {
        render_scrollbar(
            frame,
            current_layer(screen),
            shell,
            pane,
            ScrollSurface::Details,
            ScrollState {
                offset: scroll,
                content: line_count,
                viewport,
            },
        );
    }
    capture_selectable(frame, shell, SelectableSurface::Details, inner, overflow);
}

/// The first row each of the leading `upto` lines is drawn on once the details
/// paragraph wraps them at `width`. Click targets are placed by this rather
/// than by their line number, so a title that takes two rows carries the
/// fields under it down with it.
pub(super) fn wrapped_row_starts(lines: &[Line<'_>], width: u16, upto: usize) -> Vec<u16> {
    let width = width.max(1);
    let mut starts = Vec::with_capacity(upto.min(lines.len()));
    let mut row = 0u16;
    for line in lines.iter().take(upto) {
        starts.push(row);
        let height = Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(width)
            .max(1);
        row = row.saturating_add(u16::try_from(height).unwrap_or(u16::MAX));
    }
    starts
}

pub(super) struct FamilyHit {
    line: u16,
    key: TicketKey,
    jumpable: bool,
}

/// The family's own heading: the same rule every section wears, with how far
/// its children have got written into it, and the cursor's ground on the whole
/// row while the tree has the keyboard.
pub(super) fn family_section_line(
    progress: Option<ChildProgress>,
    focused: bool,
    width: u16,
) -> Line<'static> {
    let Some(progress) = progress else {
        return section_line("Family", width);
    };
    let count = format!("{} closed", progress.ratio());
    let rule = Style::default().fg(theme().header);
    let tail = usize::from(width)
        .saturating_sub(3)
        .saturating_sub("Family · ".chars().count())
        .saturating_sub(count.chars().count())
        .saturating_sub(1);
    let mut line = Line::from(vec![
        Span::styled("── ", rule),
        Span::styled("Family", rule.add_modifier(Modifier::BOLD)),
        Span::styled(" · ", rule),
        Span::styled(
            count,
            Style::default().fg(state_color(StateCategory::Completed)),
        ),
        Span::styled(format!(" {}", "─".repeat(tail)), rule),
    ]);
    if focused {
        line = line.style(with_cursor_style(Style::default()));
    }
    line
}

pub(super) fn family_row_style(is_current: bool, is_cursor: bool) -> Style {
    let mut style = Style::default();
    if is_current {
        style = style.fg(theme().text).add_modifier(Modifier::BOLD);
    }
    if is_cursor {
        style = with_cursor_style(style);
    }
    style
}

pub(crate) fn with_cursor_style(style: Style) -> Style {
    let style = style.bg(theme().selected_background);
    if theme().selected_background == Color::Reset {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

pub(super) fn family_connector(prefix: &str) -> String {
    if prefix.chars().all(char::is_whitespace) {
        prefix.to_owned()
    } else {
        format!("{prefix} ")
    }
}

/// A one-character state marker for the family tree, where there is no room to
/// spell the state out.
pub(super) const fn state_glyph(category: StateCategory) -> &'static str {
    match category {
        StateCategory::Proposed => "\u{25cb}",
        StateCategory::InProgress => "\u{25d0}",
        StateCategory::Resolved => "\u{25cf}",
        StateCategory::Completed => "\u{2713}",
        StateCategory::Removed => "\u{2717}",
        StateCategory::Unknown => "",
    }
}

/// Colour the glyph like the table's State cell; monochrome leans on weight.
pub(super) fn state_glyph_style(base: Style, category: StateCategory) -> Style {
    let color = state_color(category);
    let style = base.fg(color).remove_modifier(Modifier::UNDERLINED);
    if color == Color::Reset {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

pub(super) fn family_tree_line(
    entry: &FamilyTreeEntry,
    ticket: Option<&Ticket>,
    progress: Option<ChildProgress>,
    is_cursor: bool,
    width: u16,
) -> Line<'static> {
    let connector = family_connector(&entry.prefix);
    let id = entry.key.id.to_string();
    let type_label = ticket.map_or("?", |ticket| ticket.work_item_type.as_str());
    let title = ticket.map_or("missing ticket", |ticket| ticket.title.as_str());
    let category = ticket.map(|ticket| StateCategory::of(&ticket.state));
    // A parent says how far its own children have got; a leaf trails nothing.
    let trailer = format!(
        "{}{}",
        progress.map_or_else(String::new, |progress| format!(" {}", progress.ratio())),
        if entry.is_current { " current" } else { "" }
    );
    let packed = pack_family_row(
        usize::from(width),
        &format!("{connector}{id}"),
        type_label,
        category.map_or("", state_glyph),
        title,
        &trailer,
    );
    let base = family_row_style(entry.is_current, is_cursor);
    let id_style = if entry.is_current || ticket.is_none() {
        base.fg(theme().muted)
    } else {
        base.fg(theme().link).add_modifier(Modifier::UNDERLINED)
    };
    // The row you are reading keeps its weight; finished relatives fade.
    let tone = if entry.is_current {
        RowTone::Normal
    } else {
        ticket.map_or(RowTone::Normal, |ticket| RowTone::of(&ticket.state))
    };
    let rest_style = tone
        .apply(if entry.is_current {
            base
        } else {
            base.fg(theme().body)
        })
        .remove_modifier(Modifier::UNDERLINED);
    let head_len = connector.chars().count() + id.chars().count();
    let mut spans = vec![Span::styled(connector, base), Span::styled(id, id_style)];
    if let Some(at) = packed.glyph_at {
        let lead: String = packed
            .text
            .chars()
            .skip(head_len)
            .take(at.saturating_sub(head_len))
            .collect();
        let glyph: String = packed.text.chars().skip(at).take(1).collect();
        let tail: String = packed.text.chars().skip(at + 1).collect();
        spans.push(Span::styled(lead, rest_style));
        spans.push(Span::styled(
            glyph,
            state_glyph_style(base, category.unwrap_or(StateCategory::Unknown)),
        ));
        spans.push(Span::styled(tail, rest_style));
    } else {
        let rest: String = packed.text.chars().skip(head_len).collect();
        spans.push(Span::styled(rest, rest_style));
    }
    Line::from(spans)
}

/// A family row packed to its width, with the char offset of the state glyph
/// when one survived the fit.
pub(super) struct PackedFamilyRow {
    text: String,
    glyph_at: Option<usize>,
}

impl PackedFamilyRow {
    fn plain(text: String) -> Self {
        Self {
            text,
            glyph_at: None,
        }
    }
}

/// Packs one family row into `width`. The trailer is what follows the title —
/// the child ratio, the `current` marker, or both — and is the first thing
/// dropped after the title has been truncated as far as it goes.
pub(super) fn pack_family_row(
    width: usize,
    head: &str,
    type_label: &str,
    glyph: &str,
    title: &str,
    trailer: &str,
) -> PackedFamilyRow {
    let assemble = |include_type: bool, include_glyph: bool, include_current: bool, title: &str| {
        let mut text = head.to_owned();
        if include_type {
            text.push_str("  ");
            text.push_str(type_label);
        }
        let mut glyph_at = None;
        if include_glyph && !glyph.is_empty() {
            text.push_str("  ");
            glyph_at = Some(text.chars().count());
            text.push_str(glyph);
        }
        if !title.is_empty() {
            text.push_str(if glyph_at.is_some() { " " } else { "  " });
            text.push_str(title);
        }
        if include_current {
            text.push_str(trailer);
        }
        PackedFamilyRow { text, glyph_at }
    };
    let include_current = !trailer.is_empty();
    let fit = |row: PackedFamilyRow| (row.text.chars().count() <= width).then_some(row);
    let budget = |include_type: bool, include_current: bool| {
        width.saturating_sub(
            assemble(include_type, false, include_current, "")
                .text
                .chars()
                .count()
                .saturating_add(2),
        )
    };
    if let Some(row) = fit(assemble(true, true, include_current, title)) {
        return row;
    }
    // Shed one thing at a time so the connector and id always survive: the
    // glyph goes before the title is truncated, then the type, then the
    // trailer.
    if let Some(row) = fit(assemble(true, false, include_current, title)) {
        return row;
    }
    for (with_type, with_current) in [
        (true, include_current),
        (false, include_current),
        (false, false),
    ] {
        let truncated = take_chars(title, budget(with_type, with_current));
        if let Some(row) = fit(assemble(with_type, false, with_current, &truncated)) {
            return row;
        }
    }
    let without_title = assemble(false, false, false, "");
    if without_title.text.chars().count() <= width {
        return without_title;
    }
    if head.chars().count() <= width {
        return PackedFamilyRow::plain(head.to_owned());
    }
    PackedFamilyRow::plain(take_chars(head, width))
}

pub(super) fn family_breadcrumb_line(
    screen: &WorkItemsScreen,
    family: &FamilySnapshot,
) -> Line<'static> {
    let mut spans = vec![field_label("Family")];
    if let Some(parent) = family.parent() {
        let ticket = screen.ticket_by_key(parent);
        let type_label = ticket.map_or("?", |ticket| ticket.work_item_type.as_str());
        let title = ticket.map_or("missing ticket", |ticket| ticket.title.as_str());
        spans.push(Span::raw(format!("{type_label} ")));
        spans.push(Span::raw(parent.id.to_string()));
        spans.push(Span::raw(format!("  {title} › this")));
    } else {
        spans.push(Span::raw("this"));
    }
    Line::from(spans)
}

pub(super) fn family_member_line(
    prefix: &str,
    key: &TicketKey,
    ticket: Option<&Ticket>,
    is_current: bool,
    is_focused: bool,
    width: u16,
) -> Line<'static> {
    let id = key.id.to_string();
    let work_item_type = ticket.map_or("?", |ticket| ticket.work_item_type.as_str());
    let title = ticket.map_or("missing ticket", |ticket| ticket.title.as_str());
    let marker = if is_current { " ←" } else { "" };
    let used = prefix.chars().count()
        + id.chars().count()
        + 1
        + work_item_type.chars().count()
        + 2
        + marker.chars().count();
    let title = take_chars(title, usize::from(width).saturating_sub(used));
    let base = family_row_style(is_current, is_focused);
    let id_style = if is_current || ticket.is_none() {
        base.fg(theme().muted)
    } else {
        base.fg(theme().link).add_modifier(Modifier::UNDERLINED)
    };
    Line::from(vec![
        Span::styled(prefix.to_owned(), base),
        Span::styled(id, id_style),
        Span::styled(" ", base.remove_modifier(Modifier::UNDERLINED)),
        Span::styled(
            format!("{work_item_type}  {title}{marker}"),
            if is_current {
                base
            } else {
                base.fg(theme().body)
            },
        ),
    ])
}

pub(super) fn take_chars(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

pub(super) fn visible_row_y(area: Rect, logical: u16, scroll: u16) -> Option<u16> {
    if logical < scroll {
        return None;
    }
    let offset = logical - scroll;
    if offset >= area.height {
        return None;
    }
    Some(area.y.saturating_add(offset))
}

/// The badge row under the title: what the work item is, where it has got to,
/// how urgent it is and whose it is, in the colours the table's own cells use,
/// so the two read as one thing seen twice.
pub(super) fn ticket_badge_line(
    ticket: &Ticket,
    mine: bool,
    highlighter: &mut QueryHighlighter,
) -> Line<'static> {
    let dot = || Span::styled(" · ", Style::default().fg(theme().muted));
    let id = ticket.key.id.to_string();
    let state = state_style(&ticket.state);
    let mut spans = vec![Span::styled("#", Style::default().fg(theme().muted))];
    spans.extend(highlight_searchable(&id, Style::default().fg(theme().muted), highlighter).spans);
    spans.push(dot());
    spans.extend(type_badge_spans(
        &ticket.work_item_type,
        RowTone::Normal,
        highlighter,
    ));
    spans.push(dot());
    spans.push(Span::styled(
        format!("{} ", state_glyph(StateCategory::of(&ticket.state))),
        state,
    ));
    spans.extend(highlight_searchable(&ticket.state, state, highlighter).spans);
    spans.push(dot());
    spans.push(Span::styled(
        priority_badge(ticket.priority),
        priority_style(ticket.priority),
    ));
    spans.push(dot());
    spans.extend(match ticket.assigned_to.as_deref() {
        Some(name) if mine => highlight_searchable(name, assigned_to_me_style(), highlighter).spans,
        Some(name) => highlight_searchable(name, Style::default(), highlighter).spans,
        None => Line::styled(UNASSIGNED_LABEL, Style::default().fg(theme().muted)).spans,
    });
    Line::from(spans)
}

/// `P1`, or the dash that stands for a work item nobody has ranked — the same
/// cell the table's Pri column paints.
pub(super) fn priority_badge(priority: Option<i64>) -> String {
    priority.map_or_else(|| "—".to_owned(), |priority| format!("P{priority}"))
}

/// The signed-in user's own work. Bold carries the emphasis where NO_COLOR
/// resets the accent, so "mine" reads either way.
pub(super) fn assigned_to_me_style() -> Style {
    Style::default()
        .fg(theme().accent)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn highlighted_field_line(
    label: &'static str,
    value: &str,
    highlighter: &mut QueryHighlighter,
) -> Line<'static> {
    let mut spans = vec![field_label(label)];
    spans.extend(highlight_searchable(value, Style::default(), highlighter).spans);
    Line::from(spans)
}

pub(super) fn tags_field_line(
    tags: &[String],
    highlighter: &mut QueryHighlighter,
) -> Line<'static> {
    let mut spans = vec![field_label("Tags")];
    if tags.is_empty() {
        spans.push(Span::styled("—", Style::default().fg(theme().muted)));
    } else {
        spans.extend(tag_badge_spans(tags, RowTone::Normal, highlighter));
    }
    Line::from(spans)
}

/// One editable value on the details pane's heading: which of the pane's
/// leading lines it sits on, the column its value starts at, and how wide that
/// value is drawn.
pub(super) struct FieldSpan {
    field: EditableField,
    line: u16,
    x: u16,
    width: u16,
}

/// How many columns a piece of text takes on the line that carries it.
pub(super) fn columns(text: &str) -> u16 {
    u16::try_from(Span::raw(text).width()).unwrap_or(u16::MAX)
}

/// Where each editable value sits on the pane's heading, measured from the same
/// text [`ticket_identity_line`], [`ticket_assignment_line`], and
/// [`tags_field_line`] build their lines out of, so a click lands on the value
/// rather than anywhere on its line. The heading opens the pane's one scrolling
/// paragraph, so these are the content's first lines. Assignee and Priority
/// share a line and are two separate spans on it.
pub(super) fn metadata_field_spans(ticket: &Ticket, has_family: bool) -> Vec<FieldSpan> {
    let separator = columns(" \u{b7} ");
    let state = &ticket.state;
    let glyph = columns(state_glyph(StateCategory::of(state))).saturating_add(1);
    // Along the badge row: `#600 · [Issue] · ✓ Done · P1 · Jacob Ragsdale`.
    let state_x = columns("#")
        .saturating_add(columns(&ticket.key.id.to_string()))
        .saturating_add(separator)
        .saturating_add(columns(&ticket.work_item_type))
        .saturating_add(2)
        .saturating_add(separator)
        .saturating_add(glyph);
    let priority = priority_badge(ticket.priority);
    let priority_x = state_x
        .saturating_add(columns(state))
        .saturating_add(separator);
    let assignee = ticket.assigned_to.as_deref().unwrap_or(UNASSIGNED_LABEL);
    let assignee_x = priority_x
        .saturating_add(columns(&priority))
        .saturating_add(separator);
    // The breadcrumb sits under the badge row whenever the work item has a
    // family, and the tags under whichever of the two came last.
    let tags = 2u16.saturating_add(has_family.into());
    vec![
        FieldSpan {
            field: EditableField::Title,
            line: 0,
            x: 0,
            width: columns(&ticket.title),
        },
        FieldSpan {
            field: EditableField::State,
            line: 1,
            x: state_x,
            width: columns(state),
        },
        FieldSpan {
            field: EditableField::Priority,
            line: 1,
            x: priority_x,
            width: columns(&priority),
        },
        FieldSpan {
            field: EditableField::Assignee,
            line: 1,
            x: assignee_x,
            width: columns(assignee),
        },
        FieldSpan {
            field: EditableField::Tags,
            line: tags,
            x: FIELD_VALUE_COLUMN,
            width: tags_run_width(&ticket.tags),
        },
    ]
}

/// The columns the tag badges take together, brackets and the spaces between
/// them included, or the width of the dash that stands in for an empty list.
pub(super) fn tags_run_width(tags: &[String]) -> u16 {
    if tags.is_empty() {
        return columns("\u{2014}");
    }
    tags.iter().fold(0u16, |total, tag| {
        let badge = columns(tag).saturating_add(2);
        let gap = u16::from(total > 0);
        total.saturating_add(badge).saturating_add(gap)
    })
}

/// One editable value's hit region on a row already on screen, clipped to the
/// pane and dropped when the value starts past its right edge. It stays part of
/// the details text surface, so dragging across it still selects and copies.
pub(super) fn register_edit_field(
    shell: &mut Shell,
    area: Rect,
    field: EditableField,
    y: u16,
    x: u16,
    width: u16,
) {
    if x >= area.width || width == 0 {
        return;
    }
    shell.hit_regions.push(region(
        Rect::new(
            area.x.saturating_add(x),
            y,
            width.min(area.width.saturating_sub(x)),
            1,
        ),
        PointerTarget::EditField { field },
        PointerLayer::Base,
        Some(SelectableSurface::Details),
        Some(ScrollSurface::Details),
    ));
}

/// The details pane's `Changed` line: the exact instant, and — when nobody has
/// touched the work item past the threshold — how many whole days it has been
/// sitting, in the same warning colour the column uses.
pub(super) fn changed_field_line(ticket: &Ticket, stale_for: Option<i64>) -> Line<'static> {
    let mut line = field_line("Changed", ticket.changed_at.exact_utc());
    if let Some(days) = stale_for {
        line.spans.push(Span::styled(
            format!(" (stale {days}d)"),
            Style::default()
                .fg(theme().warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    line
}

pub(super) fn field_line<'a>(label: &'a str, value: impl Into<String>) -> Line<'a> {
    Line::from(vec![field_label(label), Span::raw(value.into())])
}

/// One field's name, muted and padded to the column its value starts in. A
/// label as wide as the column, or wider, keeps its own width and pushes its
/// value along — behind one space, so a name and its value never run together
/// the way `Default branch` and `main` once did.
pub(super) fn field_label(label: &str) -> Span<'static> {
    let width = usize::from(FIELD_VALUE_COLUMN);
    let padded = if label.chars().count() >= width {
        format!("{label} ")
    } else {
        format!("{label:<width$}")
    };
    Span::styled(padded, Style::default().fg(theme().muted))
}

/// One change on one revision: how long ago it landed, who made it, and what
/// moved. The relative age is the wording the Changed column uses, with the
/// exact instant beside it in muted text for anyone who needs one.
pub(super) fn history_line(entry: &HistoryRecord, now: OffsetDateTime) -> Line<'static> {
    let who = entry.changed_by.as_deref().unwrap_or("unknown");
    let old = entry.old_value.as_deref().unwrap_or("—");
    let new = entry.new_value.as_deref().unwrap_or("—");
    Line::from(vec![
        Span::styled(
            format!(
                "  {} · {who} · {}: {old} → {new}",
                entry.changed_at.relative_to(now),
                entry.field_name
            ),
            Style::default().fg(theme().body),
        ),
        Span::styled(
            format!("  {}", entry.changed_at.exact_utc()),
            Style::default().fg(theme().muted),
        ),
    ])
}

/// One artifact link as the Related section draws it, and where it goes.
///
/// A pull request or a build the database holds is named and underlined; one
/// it does not hold reads as its number alone, in the muted style, because
/// nothing here can show it. A commit is never a jump — there is no commit
/// screen — so it reads as its short sha and the repository it is in.
fn artifact_line(artifact: &ArtifactLink, shell: &Shell) -> (Line<'static>, Option<Jump>) {
    let muted = Style::default().fg(theme().muted);
    let link = Style::default()
        .fg(theme().link)
        .add_modifier(Modifier::UNDERLINED);
    let label = |text: String| Span::styled(format!("  {text:<14}"), muted);
    match &artifact.kind {
        ArtifactKind::PullRequest { repo_id, id } => match shell.pull_request_label(*id) {
            Some((title, status)) => (
                terminate_underline(Line::from(vec![
                    label("Pull request".to_owned()),
                    Span::styled(format!("!{id}  {title}"), link),
                    Span::styled(format!("  \u{00b7} {}", status.as_str()), muted),
                ])),
                Some(Jump::PullRequest {
                    repo: shell.repo_name(repo_id),
                    id: *id,
                }),
            ),
            None => (
                Line::from(vec![
                    label("Pull request".to_owned()),
                    Span::styled(format!("!{id}  not in this database"), muted),
                ]),
                None,
            ),
        },
        ArtifactKind::Build(id) => match shell.run_label(*id) {
            Some((number, status, result)) => (
                terminate_underline(Line::from(vec![
                    label("Build".to_owned()),
                    Span::styled(format!("Build {number}"), link),
                    Span::styled(
                        format!(
                            "  {}",
                            crate::app::pipelines::rows::run_glyph(status, result)
                        ),
                        muted,
                    ),
                ])),
                Some(Jump::Run(*id)),
            ),
            None => (
                Line::from(vec![
                    label("Build".to_owned()),
                    Span::styled(format!("Build {id} \u{2014} not in this database"), muted),
                ]),
                None,
            ),
        },
        // A commit has no screen of its own, so it says what it is and where.
        ArtifactKind::Commit { repo_id, sha } => (
            Line::from(vec![
                label("Commit".to_owned()),
                Span::raw(short_sha(sha)),
                Span::styled(format!("  in {}", shell.repo_name(repo_id)), muted),
            ]),
            None,
        ),
    }
}

/// The seven characters a git log reads by.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// A section heading drawn as a rule across the pane — `── Planning ────` —
/// so a heading is told from a pane title and from a bold field label by its
/// shape rather than by its colour, which is what `NO_COLOR` leaves.
pub(super) fn section_line(title: &'static str, width: u16) -> Line<'static> {
    let rule = Style::default().fg(theme().header);
    let head = "── ";
    let tail = usize::from(width)
        .saturating_sub(head.chars().count())
        .saturating_sub(title.chars().count())
        .saturating_sub(1);
    Line::from(vec![
        Span::styled(head, rule),
        Span::styled(title, rule.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {}", "─".repeat(tail)), rule),
    ])
}

/// The column a field's value starts in. Labels and values line up down the
/// pane rather than every value starting wherever its own label ended.
pub(super) const FIELD_VALUE_COLUMN: u16 = 11;
