//! The ACR tab: the subscription's registries on the left, and the one under
//! the cursor on the right. Both are empty until B3 reads ARM, so the details
//! pane says why there is nothing there.

use super::*;
use crate::app::acr::{AcrColumn, AcrMode, AcrScreen};
use crate::ui::table::{TableSpec, render_list_table, table_geometry};

/// The whole tab: the search box, the table, the details pane and the footer.
pub(crate) fn render(frame: &mut Frame<'_>, screen: &mut AcrScreen, shell: &mut Shell, area: Rect) {
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    render_search(frame, screen, shell, sections[0]);
    render_content(frame, screen, shell, sections[1]);
    render_status_bar(frame, shell, sections[2], screen.footer_hint(shell));
}

fn render_search(frame: &mut Frame<'_>, screen: &AcrScreen, shell: &mut Shell, area: Rect) {
    render_search_row(
        frame,
        shell,
        SearchRow {
            area,
            text: screen.query(),
            cursor: screen.query_cursor(),
            placeholder: "Type / to search registries",
            active: screen.mode == AcrMode::Search,
            pending: false,
            clearable: false,
            trailer: String::new(),
            layer: PointerLayer::Modal,
            selectable: SelectableSurface::Overlay,
        },
    );
}

fn render_content(frame: &mut Frame<'_>, screen: &mut AcrScreen, shell: &mut Shell, area: Rect) {
    struct Panes<'a>(&'a mut AcrScreen);
    impl PanePair for Panes<'_> {
        fn first(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_table(frame, self.0, shell, area);
        }

        fn second(&mut self, frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
            render_registry_details(frame, shell, area);
        }
    }
    render_workspace(
        frame,
        shell,
        area,
        &PaneNames {
            list: "Registries",
            details: "Registry",
        },
        &mut Panes(screen),
    );
}

fn render_table(frame: &mut Frame<'_>, screen: &mut AcrScreen, shell: &mut Shell, area: Rect) {
    let geometry = table_geometry(area, 1);
    screen.cursor.scroll.set_viewport(geometry.visible_rows, 0);
    let (sorted, descending) = screen.sort;
    let layout = screen.layout.clone();
    let mut cell = |_index: usize, _column: AcrColumn| Cell::from("");
    let mut spec = TableSpec {
        title: " Registries ".to_owned(),
        status: "0 registries".to_owned(),
        focused: shell.focus == Focus::Tickets,
        layout: &layout,
        sorted: Some((sorted, if descending { "\u{2193}" } else { "\u{2191}" })),
        count: 0,
        offset: screen.cursor.scroll.offset,
        selected: Some(screen.cursor.index),
        row_height: 1,
        layer: PointerLayer::Base,
        scroll: ScrollSurface::Table,
        selectable: SelectableSurface::Table,
        marker: None,
        cell: &mut cell,
    };
    render_list_table(frame, shell, area, &mut spec);
}

/// What the pane says with no registry to describe: why ARM cannot be reached,
/// or that nothing has been read from it yet.
fn render_registry_details(frame: &mut Frame<'_>, shell: &mut Shell, area: Rect) {
    let block =
        focused_block(" Registry ", shell.focus == Focus::Details).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(
            shell
                .arm_state()
                .unwrap_or("No registries read yet")
                .to_owned(),
        )
        .style(Style::default().fg(theme().muted))
        .wrap(Wrap { trim: false }),
        inner,
    );
}
