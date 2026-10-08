// Exposed wide-glyph tail cells immediately right of an opaque overlay must be
// repainted as ordinary blanks so the original ANSI encoder emits a space and
// the real terminal shows the underlying pane background, not the overlay's.
use super::*;
use crate::config::ToastClipboardPosition;
use crate::ghostty::{RenderState, RgbColor, RowCells, RowIterator, Terminal};
use crate::protocol::render_ansi::BlitEncoder;
use crate::protocol::PaneSurfacePatchRow;
use ratatui::buffer::Cell;
use ratatui::style::Color;

const PANE_RGB: RgbColor = RgbColor { r: 0, g: 30, b: 39 };
const PANEL_RGB: Color = Color::Rgb(43, 54, 60);

fn palette() -> Palette {
    let mut palette = ClientShellConfig::from_config(&Config::default()).palette;
    palette.panel_bg = PANEL_RGB;
    palette
}

fn assert_bg(terminal: &Terminal, x: u16, y: u16, expected: RgbColor) {
    let mut rendered = RenderState::new().unwrap();
    rendered.update(terminal).unwrap();
    let mut iterator = RowIterator::new().unwrap();
    let mut rows = rendered.populate_row_iterator(&mut iterator).unwrap();
    for _ in 0..=y {
        assert!(rows.next());
    }
    let mut row_cells = RowCells::new().unwrap();
    let mut cells = rows.populate_cells(&mut row_cells).unwrap();
    for _ in 0..=x {
        assert!(cells.next());
    }
    assert_eq!(
        cells.bg_color().unwrap(),
        Some(expected),
        "background at ({x}, {y})"
    );
}

fn assert_symbol(terminal: &Terminal, x: u16, y: u16, expected: char) {
    assert_eq!(
        terminal.screen_cell(x, y.into()).unwrap().1,
        vec![u32::from(expected)],
        "symbol at ({x}, {y})"
    );
}

fn pane_buffer(area: Rect) -> Buffer {
    let mut blank = Cell::default();
    blank
        .set_symbol(" ")
        .set_style(Style::default().bg(Color::Rgb(0, 30, 39)));
    Buffer::filled(area, blank)
}

fn context_menu(x: u16, y: u16) -> ClientContextMenuOverlay {
    ClientContextMenuOverlay {
        target: ClientContextMenuTarget::Pane {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            source_pane_id: None,
            has_manual_label: false,
            right_click_passthrough: false,
        },
        x,
        y,
        highlighted: 0,
    }
}

/// Drive an opaque painter through the real frame-replacement → original
/// encoder → real Ghostty path and assert every exposed tail row repaints to
/// the pane background. `paint` receives the supplied `sub_area`, which must
/// be interior so the painted rect's right edge is exposed inside `area`.
/// Asserts the exposure precondition per seed: no coverage is claimed for
/// geometry that never exposes a tail.
fn assert_exposed_tail_repaint(
    area: Rect,
    sub_area: Rect,
    paint: impl Fn(&mut Buffer, Rect) -> Rect,
    probe_ys: impl Fn(Rect) -> Vec<u16>,
) {
    for trailing in ["", " "] {
        let mut scratch = Buffer::empty(area);
        let rect = paint(&mut scratch, sub_area);
        assert!(
            rect.right() < area.right(),
            "exposure precondition: {rect:?} must end inside {area:?}"
        );
        let ys = probe_ys(rect);
        assert!(!ys.is_empty(), "probe rows required for {rect:?}");
        let mut underlying = pane_buffer(area);
        for &y in &ys {
            underlying[(rect.right() - 1, y)].set_symbol("修");
            underlying[(rect.right(), y)].set_symbol(trailing);
        }

        let previous = FrameData::from_ratatui_buffer(&underlying, None);
        let mut terminal = Terminal::new(area.width, area.height, 0).unwrap();
        terminal.write(b"\x1b[?7l");
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&previous, false);
        terminal.write(&initial.bytes);
        encoder.commit(previous.clone(), initial);

        // Same frame-replacement path as compose().
        let mut composed = previous.to_ratatui_buffer().unwrap();
        paint(&mut composed, sub_area);
        let mut opened = previous.clone();
        opened.replace_from_ratatui_buffer_preserving_effects(&composed, None);
        let update = encoder.encode(&opened, false);
        terminal.write(&update.bytes);
        encoder.commit(opened, update);

        for &y in &ys {
            assert_bg(&terminal, rect.right(), y, PANE_RGB);
        }

        // Closing the overlay restores the underlying wide glyph.
        let closed = encoder.encode(&previous, false);
        terminal.write(&closed.bytes);
        encoder.commit(previous.clone(), closed);
        for &y in &ys {
            assert_symbol(&terminal, rect.right() - 1, y, '修');
        }
    }
}

#[test]
fn exposed_tail_normalization_preserves_other_cells_and_styles() {
    let mut original = pane_buffer(Rect::new(2, 1, 8, 4));
    // A covered wide tail and an unrelated wide tail must remain empty.
    for (head_x, tail_x) in [(4, 5), (8, 9)] {
        original[(head_x, 2)].set_symbol("中");
        original[(tail_x, 2)].set_symbol("");
    }
    original[(6, 2)].set_symbol("│");
    original[(7, 2)].set_symbol("").set_style(
        Style::default()
            .fg(Color::Yellow)
            .bg(Color::Blue)
            .add_modifier(ratatui::style::Modifier::BOLD),
    );
    original[(7, 3)].set_symbol("");

    for (rect, exposes_tail) in [
        (Rect::new(4, 2, 3, 1), true),
        (Rect::new(7, 2, 0, 1), false),
        (Rect::new(4, 2, 3, 0), false),
        (Rect::new(4, 2, 6, 1), false), // Buffer right edge.
        (Rect::new(4, 2, 9, 1), false), // Clipped beyond buffer right edge.
        (Rect::new(4, 2, 4, 1), false), // Non-empty neighbor.
        (Rect::new(4, 1, 3, 1), false), // Ordinary space neighbor.
    ] {
        let mut actual = original.clone();
        let mut expected = original.clone();
        if exposes_tail {
            expected[(7, 2)].set_symbol(" ");
        }
        crate::ui::normalize_exposed_tail_cells(&mut actual, rect);
        assert_eq!(actual, expected, "full-cell preservation for {rect:?}");
    }
}

#[test]
fn context_menu_right_edge_repaints_exposed_tail_background() {
    let area = Rect::new(0, 0, 80, 24);
    let palette = palette();
    for repaint in [false, true] {
        for menu_x in [0, 4, 80] {
            let menu = context_menu(menu_x, 2);
            let mut scratch = Buffer::empty(area);
            let rect = render::render_context_menu(&mut scratch, &menu, &palette)
                .expect("real menu geometry")
                .area;
            let y = rect.y + 1;
            let mut underlying = pane_buffer(area);
            if rect.right() < area.right() {
                underlying[(rect.right() - 1, y)].set_symbol("修");
                underlying[(rect.right(), y)].set_symbol("");
            }
            let text_x = if rect.right() + 6 < area.right() {
                rect.right() + 2
            } else {
                0
            };
            underlying[(text_x, y)].set_symbol("中");
            underlying[(text_x + 1, y)].set_symbol(" ");
            underlying[(text_x + 2, y)].set_symbol("p");
            underlying[(text_x + 3, y)].set_symbol("i");

            let previous = FrameData::from_ratatui_buffer(&underlying, None);
            let mut terminal = Terminal::new(area.width, area.height, 0).unwrap();
            terminal.write(b"\x1b[?7l");
            let mut encoder = BlitEncoder::new();
            let initial = encoder.encode(&previous, false);
            terminal.write(&initial.bytes);
            encoder.commit(previous.clone(), initial);

            let mut composed = previous.to_ratatui_buffer().unwrap();
            render::render_context_menu(&mut composed, &menu, &palette).unwrap();
            let mut opened = previous.clone();
            opened.replace_from_ratatui_buffer_preserving_effects(&composed, None);
            let update = encoder.encode(&opened, repaint);
            terminal.write(&update.bytes);
            encoder.commit(opened, update);

            if rect.right() < area.right() {
                assert_bg(&terminal, rect.right(), y, PANE_RGB);
                assert_symbol(&terminal, rect.right() - 1, y, '│');
            }
            // Left-edge remnant of a head whose tail is covered is a separate
            // seam handled by another patch; this baseline only owns the
            // right-edge exposure.
            assert_symbol(&terminal, text_x, y, '中');
            assert_symbol(&terminal, text_x + 2, y, 'p');
            assert_symbol(&terminal, text_x + 3, y, 'i');

            let closed = encoder.encode(&previous, false);
            terminal.write(&closed.bytes);
            encoder.commit(previous.clone(), closed);
            if rect.right() < area.right() {
                assert_symbol(&terminal, rect.right() - 1, y, '修');
            }

            // A real committed " " baseline cell updated to an empty tail must
            // stay on the sparse path; baseline and desired symbols differ.
            let start = usize::from(y) * usize::from(area.width) + usize::from(text_x + 1);
            assert_eq!(previous.cells[start].symbol, " ");
            let mut cells = previous.cells[start..start + 1].to_vec();
            cells[0].symbol.clear();
            assert_ne!(cells[0].symbol, previous.cells[start].symbol);
            let rows = vec![PaneSurfacePatchRow {
                x: text_x + 1,
                y,
                cells,
            }];
            let sparse = encoder
                .encode_patch(&rows, None, false)
                .expect("ordinary space->tail update must not require composed-frame fallback");
            terminal.write(&sparse.bytes);
            assert!(encoder.commit_patch(&rows, None, sparse));
            assert_symbol(&terminal, text_x, y, '中');
            assert_symbol(&terminal, text_x + 2, y, 'p');
            assert_symbol(&terminal, text_x + 3, y, 'i');
            assert_bg(&terminal, text_x + 1, y, PANE_RGB);
        }
    }
}

#[test]
fn notification_card_right_edge_repaints_exposed_tail_background() {
    let area = Rect::new(0, 0, 60, 12);
    let palette = palette();
    for position in [
        crate::config::ToastHerdrPosition::TopLeft,
        crate::config::ToastHerdrPosition::TopRight,
    ] {
        // Interior supplied area so the card's right edge is genuinely exposed.
        let sub_area = Rect::new(2, 1, 40, 8);
        assert_exposed_tail_repaint(
            area,
            sub_area,
            |buffer, sub_area| {
                let rect = notifications::render_notification_card(
                    buffer,
                    sub_area,
                    "agent done",
                    "body",
                    position,
                    1,
                    Color::Red,
                    &palette,
                );
                assert!(
                    rect.right() < area.right(),
                    "notification card must expose its right edge"
                );
                rect
            },
            |rect| (rect.y..rect.bottom()).collect(),
        );
    }
}

#[test]
fn mobile_notice_banner_right_edge_repaints_exposed_tail_background() {
    let area = Rect::new(0, 0, 44, 8);
    let palette = palette();
    // Interior supplied area: the banner spans its width, leaving its right
    // edge exposed inside the larger buffer.
    let sub_area = Rect::new(2, 0, 36, 8);
    assert_exposed_tail_repaint(
        area,
        sub_area,
        |buffer, sub_area| {
            notifications::render_mobile_notice_banner(
                buffer,
                sub_area,
                "waiting",
                None,
                Color::Red,
                false,
                &palette,
            )
        },
        |rect| vec![rect.y],
    );
}

#[test]
fn lifecycle_banner_right_edge_repaints_exposed_tail_background() {
    let area = Rect::new(0, 0, 60, 8);
    let palette = palette();
    // Interior supplied area: the banner is right-aligned within it, so its
    // right edge sits inside the larger buffer.
    let sub_area = Rect::new(4, 1, 44, 6);
    assert_exposed_tail_repaint(
        area,
        sub_area,
        |buffer, sub_area| {
            endpoint_notices::render_lifecycle_banner(
                buffer,
                sub_area,
                "remote",
                ClientEndpointStatus::Disabled,
                2,
                &palette,
            )
        },
        |rect| vec![rect.y],
    );
}

#[test]
fn copy_feedback_right_edge_repaints_exposed_tail_background() {
    let area = Rect::new(0, 0, 60, 12);
    let palette = palette();
    let feedback = crate::app::state::CopyFeedback {
        message: "copied".into(),
    };
    for position in [
        ToastClipboardPosition::TopLeft,
        ToastClipboardPosition::BottomRight,
    ] {
        // Interior supplied area: right-aligned positions still leave the
        // toast's right edge exposed inside the larger buffer.
        let sub_area = Rect::new(4, 1, 44, 9);
        assert_exposed_tail_repaint(
            area,
            sub_area,
            |buffer, sub_area| {
                crate::ui::render_copy_feedback_buffer(
                    buffer, sub_area, &feedback, 1, position, &palette,
                )
            },
            |rect| (rect.y..rect.bottom()).collect(),
        );
    }
}

#[test]
fn copy_feedback_short_height_right_edge_repaints_exposed_tail() {
    // Assigned small-height case: rect height 2 leaves no inner rows; the
    // early-return path must still normalize the exposed edge.
    let area = Rect::new(0, 0, 60, 6);
    let palette = palette();
    let feedback = crate::app::state::CopyFeedback {
        message: "copied".into(),
    };
    let sub_area = Rect::new(4, 0, 12, 2);
    assert_exposed_tail_repaint(
        area,
        sub_area,
        |buffer, sub_area| {
            let rect = crate::ui::render_copy_feedback_buffer(
                buffer,
                sub_area,
                &feedback,
                0,
                ToastClipboardPosition::TopLeft,
                &palette,
            );
            assert_eq!(rect.height, 2, "short-height early-return case");
            rect
        },
        |rect| (rect.y..rect.bottom()).collect(),
    );
}

#[test]
fn composed_popup_right_edge_repaints_exposed_tail() {
    // Real compose() path: popup Block + Clear over a pane surface, with a
    // wide glyph straddling the popup's right edge. The probe is seeded into
    // the full-surface frame at layout coordinates so it lands under the
    // popup after `blit_pane_surface` at `layout.pane_surface`.
    let mut config = ClientShellConfig::from_config(&Config::default());
    // Deterministic pane_surface: hidden collapsed sidebar, tab bar bottom.
    config.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
    config.sidebar_width = 0;
    config.tab_bar_position = TabBarPositionConfig::Bottom;
    config.palette.panel_bg = PANEL_RGB;
    let mut state = ClientShellState::new(config);
    state.sidebar_collapsed = true;
    state.set_snapshot(Box::new(snapshot()));
    let pane_surface = state.layout(80, 24).pane_surface;
    // Same size mapping as compose(): the fixture popup is Cells(12)xCells(5).
    let geometry = crate::popup_size::resolve_popup_geometry(
        Some(crate::popup_size::PopupSize::Cells(12)),
        Some(crate::popup_size::PopupSize::Cells(5)),
        pane_surface,
    )
    .expect("popup geometry");
    assert!(
        geometry.outer.right() < 80,
        "popup must expose its right edge"
    );
    let probe_y = geometry.outer.y + 1;

    let mut surface = surface_with_popup();
    // Write the probe glyph into the full-surface frame at the popup edge:
    // head just inside the right border, tail just outside it. The surface
    // frame is sized to `pane_surface` and blitted at its origin, so probe
    // coordinates are surface-local. Give the tail the pane background so
    // the assert detects an unrepainted overlay cell.
    surface.frame = FrameData::from_ratatui_buffer(
        &pane_buffer(Rect::new(0, 0, pane_surface.width, pane_surface.height)),
        None,
    );
    let mut full = surface.frame.to_ratatui_buffer().expect("surface frame");
    let head_x = geometry.outer.right() - 1 - pane_surface.x;
    let local_y = probe_y - pane_surface.y;
    full[(head_x, local_y)].set_symbol("修");
    full[(geometry.outer.right() - pane_surface.x, local_y)].set_symbol("");
    surface.frame = FrameData::from_ratatui_buffer(&full, None);
    state.set_pane_surface(surface);

    let composed = state.compose(80, 24).expect("composed frame");
    let mut terminal = Terminal::new(80, 24, 0).unwrap();
    terminal.write(b"\x1b[?7l");
    let encoder = BlitEncoder::new();
    let initial = encoder.encode(&composed.frame, false);
    terminal.write(&initial.bytes);

    // The exposed tail cell just outside the popup's right border must carry
    // the pane background, not the popup's panel background.
    assert_bg(&terminal, geometry.outer.right(), probe_y, PANE_RGB);
    assert_symbol(&terminal, geometry.outer.right() - 1, probe_y, '│');
}

#[test]
fn adjacent_width_transitions_stay_sparse() {
    for (before, after) in [("中", "x"), ("x", "中")] {
        for reverse in [false, true] {
            let mut buffer = Buffer::with_lines(["    "]);
            buffer[(0, 0)].set_symbol(before);
            buffer[(2, 0)].set_symbol("p");
            buffer[(3, 0)].set_symbol("i");
            let previous = FrameData::from_ratatui_buffer(&buffer, None);
            let mut rows = vec![
                PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![crate::protocol::CellData {
                        symbol: after.into(),
                        ..previous.cells[0].clone()
                    }],
                },
                PaneSurfacePatchRow {
                    x: 1,
                    y: 0,
                    cells: vec![crate::protocol::CellData {
                        symbol: String::new(),
                        ..previous.cells[1].clone()
                    }],
                },
            ];
            if reverse {
                rows.reverse();
            }
            let mut terminal = Terminal::new(4, 1, 0).unwrap();
            terminal.write(b"\x1b[?7l");
            let mut encoder = BlitEncoder::new();
            let initial = encoder.encode(&previous, false);
            terminal.write(&initial.bytes);
            encoder.commit(previous, initial);
            let update = encoder
                .encode_patch(&rows, None, false)
                .expect("no new fallback");
            terminal.write(&update.bytes);
            assert_eq!(
                terminal.screen_cell(0, 0).unwrap().1,
                after.chars().map(u32::from).collect::<Vec<_>>()
            );
            assert_symbol(&terminal, 2, 0, 'p');
            assert_symbol(&terminal, 3, 0, 'i');
        }
    }
}
