//! A compact file-change summary, with Undo and Redo inside the same surface.
use super::{
    Canvas, ChatView, Drawn, Fill, Fonts, Hit, Icon, Laid, Look, Measures, Paint, Tone, TurnFiles,
    draw_laid, lay_out,
};
use clackatoo_protocol::TurnFilesState;

pub(super) struct FilesLaid {
    label: Laid,
    label_at: [f32; 2],
    buttons: Vec<(Hit, Laid, [f32; 4])>,
    pub(super) width: f32,
    pub(super) height: f32,
}

impl ChatView {
    pub(super) fn lay_out_files(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        column: f32,
        (turn, files): (usize, TurnFiles),
    ) -> FilesLaid {
        let small = Measures::of(m.em(0.85));
        let pad = m.em(0.35);
        let icon = m.em(0.95);
        let gap = m.em(0.35);
        let row = m.em(1.9);
        let (label, buttons) = files_line(turn, files);
        let label = lay_out(
            &mut self.layout_cx,
            fonts,
            small,
            &label,
            Look::Plain(Tone::Muted),
            (column - 2.0 * pad - icon - gap).max(m.em(1.0)),
        );
        let label_height = label.height + 2.0 * pad;
        let label_width = label.width + 2.0 * pad + icon + gap;
        let mut x = label_width;
        let mut y = 0.0;
        let first_row = row.max(label_height);
        let mut height = first_row;
        let mut width = label_width;
        let mut laid_buttons = Vec::new();
        for (hit, name) in buttons {
            let text = lay_out(
                &mut self.layout_cx,
                fonts,
                small,
                name,
                Look::Plain(Tone::Plain),
                (column - 2.0 * pad - icon - gap).max(m.em(1.0)),
            );
            let wide = text.width + 2.0 * pad + icon + gap;
            if x + wide > column {
                x = 0.0;
                y = height;
                height += row;
            }
            let tall = if y == 0.0 { first_row } else { row };
            laid_buttons.push((hit, text, [x, y, wide, tall]));
            x += wide;
            width = width.max(x);
        }
        let label_y = (first_row - label.height) / 2.0;
        FilesLaid {
            label,
            label_at: [pad + icon + gap, label_y],
            buttons: laid_buttons,
            width,
            height,
        }
    }
}

pub(super) fn draw(
    canvas: &mut Canvas<'_>,
    m: Measures,
    files: &FilesLaid,
    origin: [f32; 2],
    mouse: Option<[f32; 2]>,
    drawn: &mut Drawn<'_>,
) {
    let small = Measures::of(m.em(0.85));
    let pad = m.em(0.35);
    let side = m.em(0.95);
    let gap = m.em(0.35);
    let radius = m.em(0.45);
    canvas.fill(
        Fill::rounded(
            [origin[0], origin[1], files.width, files.height],
            Paint::Raised,
            radius,
        )
        .bordered(1.0, Paint::RaisedEdge),
    );
    let label_at = [origin[0] + files.label_at[0], origin[1] + files.label_at[1]];
    canvas.fill(Fill::icon(
        [
            origin[0] + pad,
            label_at[1] + (files.label.height.min(m.em(1.4)) - side) / 2.0,
            side,
            side,
        ],
        Icon::File,
        Paint::Icon,
    ));
    draw_laid(canvas, small, &files.label, label_at, files.label.width);
    for (hit, label, rect) in &files.buttons {
        let rect = [origin[0] + rect[0], origin[1] + rect[1], rect[2], rect[3]];
        if rect[0] > origin[0] {
            canvas.fill(Fill::solid(
                [rect[0], rect[1] + pad, 1.0, rect[3] - 2.0 * pad],
                Paint::RaisedEdge,
            ));
        }
        let hovered = mouse.is_some_and(|at| {
            super::super::contains(rect, [at[0] + drawn.pane[0], at[1] + drawn.pane[1]])
        });
        if hovered {
            canvas.fill(Fill::rounded(
                [rect[0] + 2.0, rect[1] + 2.0, rect[2] - 4.0, rect[3] - 4.0],
                Paint::ChatHover,
                radius - 2.0,
            ));
        }
        let icon = if matches!(hit, Hit::Undo(_)) {
            Icon::Undo
        } else {
            Icon::Redo
        };
        canvas.fill(Fill::icon(
            [rect[0] + pad, rect[1] + (rect[3] - side) / 2.0, side, side],
            icon,
            Paint::Icon,
        ));
        draw_laid(
            canvas,
            small,
            label,
            [
                rect[0] + pad + side + gap,
                rect[1] + (rect[3] - label.height) / 2.0,
            ],
            label.width,
        );
        drawn.hit(canvas.clip, hit.clone(), rect);
    }
}
/// What's said of what a turn changed in files, as `files` says, and the
/// buttons beside it: "2 files changed", then "Undo".
fn files_line(turn: usize, files: TurnFiles) -> (String, Vec<(Hit, &'static str)>) {
    let count = match files.count {
        1 => "1 file changed".to_owned(),
        count => format!("{count} files changed"),
    };
    let (label, buttons) = match files.state {
        TurnFilesState::Made => (count, vec![(Hit::Undo(turn), "Undo")]),
        TurnFilesState::Undone => (format!("{count}, undone"), vec![(Hit::Redo(turn), "Redo")]),
        TurnFilesState::Partly => (
            format!("{count}, partly undone"),
            vec![(Hit::Undo(turn), "Undo"), (Hit::Redo(turn), "Redo")],
        ),
        TurnFilesState::TooOld => (format!("{count}, too long ago to undo"), Vec::new()),
    };
    (label, buttons)
}
