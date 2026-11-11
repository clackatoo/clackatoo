//! The specialist column keeps its own scroll position and selection.
use super::{
    Canvas, ChatView, ChoiceRow, Fill, Hit, Icon, Measures, Paint, Place, Spot, Tone, divider,
    draw_laid, icon,
};

impl ChatView {
    pub(super) fn model_specialists(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        choices: &[ChoiceRow],
        options: (&crate::chat::Menu, usize),
    ) {
        let [left, top, width, height] = rect;
        divider(canvas, m, [left, top, 0.0, height]);
        self.hits.push((Hit::SpecialistColumn, place.in_pane(rect)));
        let small = Measures::of(m.em * 0.82);
        let heading = self.short_label(
            canvas.fonts,
            small,
            "Specialists",
            width - m.em(3.0),
            Tone::Muted,
        );
        draw_laid(
            canvas,
            small,
            &heading,
            [left + m.em(0.6), top + m.em(0.9)],
            width,
        );
        self.keep_label(heading);
        let cog = [
            left + width - m.em(2.0),
            top + m.em(0.55),
            m.em(1.5),
            m.em(1.5),
        ];
        if self
            .mouse
            .is_some_and(|at| crate::chat::contains(place.in_pane(cog), at))
        {
            canvas.fill(Fill::rounded(cog, Paint::ChatHover, m.em(0.3)));
        }
        icon(canvas, Icon::Cog, cog, m.em(1.0), Paint::Icon);
        self.hits
            .push((Hit::SpecialistSettings, place.in_pane(cog)));
        self.model_list(
            canvas,
            m,
            place,
            [
                left + m.em(0.35),
                top + m.em(3.2),
                width - m.em(0.7),
                m.em(3.2),
            ],
            choices,
            (options.0, options.1, true),
        );
        if !self.node.turns.is_empty() {
            let hint = self.short_label(
                canvas.fonts,
                small,
                "New chats only",
                width - m.em(1.2),
                Tone::Muted,
            );
            draw_laid(
                canvas,
                small,
                &hint,
                [left + m.em(0.6), top + height - m.em(2.1)],
                width,
            );
            self.keep_label(hint);
        }
    }

    pub(super) fn specialist_row(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        choice: &ChoiceRow,
        menu: &crate::chat::Menu,
    ) {
        let enabled = self.node.turns.is_empty();
        let spot = Spot::Choice {
            setting: choice.setting.clone(),
            value: choice.value.clone(),
        };
        if choice.current || (enabled && menu.highlighted.as_ref() == Some(&spot)) {
            canvas.fill(Fill::rounded(
                rect,
                if choice.current {
                    Paint::ChatChosen
                } else {
                    Paint::ChatHover
                },
                m.em(0.45),
            ));
        }
        let width = rect[2] - m.em(if choice.current { 1.9 } else { 1.2 });
        let title = Measures::of(m.em * 0.94);
        let name = self.short_label(
            canvas.fonts,
            title,
            &choice.name,
            width,
            if enabled || choice.current {
                Tone::Plain
            } else {
                Tone::Muted
            },
        );
        draw_laid(
            canvas,
            title,
            &name,
            [rect[0] + m.em(0.6), rect[1] + m.em(0.26)],
            width,
        );
        self.keep_label(name);
        let sub = Measures::of(m.em * 0.76);
        let description = self.short_label(
            canvas.fonts,
            sub,
            choice.description.as_deref().unwrap_or(""),
            rect[2] - m.em(1.2),
            Tone::Muted,
        );
        draw_laid(
            canvas,
            sub,
            &description,
            [rect[0] + m.em(0.6), rect[1] + m.em(1.6)],
            rect[2],
        );
        self.keep_label(description);
        if choice.current {
            icon(
                canvas,
                Icon::Check,
                [
                    rect[0] + rect[2] - m.em(1.25),
                    rect[1],
                    m.em(0.9),
                    m.em(1.7),
                ],
                m.em(0.85),
                Paint::Icon,
            );
        }
        if enabled {
            self.hits.push((
                Hit::Pick {
                    setting: choice.setting.clone(),
                    value: choice.value.clone(),
                },
                place.in_pane(rect),
            ));
        }
    }
}
