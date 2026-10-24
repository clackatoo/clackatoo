//! A searchable model chooser with a provider rail and durable favorites.
use super::{Canvas, Laid, Look, Measures, Place, TextPaint, draw_laid, intersection, lay_out};
use crate::chat::picker::{ChoiceRow, Filter, Row, SliderRow, Spot};
use crate::chat::{ChatView, Hit, Tone, picker};
use crate::fonts::Fonts;
use crate::render::{Fill, Paint, icons::Icon};

pub(super) fn icon_paint(icon: Icon) -> Paint {
    match icon {
        Icon::Claude => Paint::Claude,
        Icon::Gemini => Paint::Gemini,
        Icon::OpenAi => Paint::Ink,
        _ => Paint::Icon,
    }
}

fn icon(canvas: &mut Canvas<'_>, mark: Icon, rect: [f32; 4], side: f32, paint: Paint) {
    canvas.fill(Fill::icon(
        [
            rect[0] + (rect[2] - side) / 2.0,
            rect[1] + (rect[3] - side) / 2.0,
            side,
            side,
        ],
        mark,
        paint,
    ));
}

impl ChatView {
    /// Cache both the fitted text and its layout; unchanged frames do no shaping.
    pub(super) fn short_label(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        text: &str,
        wide: f32,
        tone: Tone,
    ) -> Laid {
        let wide = wide.max(m.em(0.8));
        let key = (text.to_owned(), wide.to_bits(), m.em.to_bits(), tone);
        let cached = self
            .cache
            .labels
            .shortened
            .get(&key)
            .or_else(|| self.cache.labels.previous_shortened.get(&key))
            .cloned();
        let fitted = cached.unwrap_or_else(|| {
            let full = self.box_label(fonts, m, text, tone, f32::INFINITY);
            let fits = full.width <= wide;
            self.keep_label(full);
            if fits {
                return text.to_owned();
            }
            let boundaries: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
            let (mut low, mut high) = (0, boundaries.len());
            let mut best = "…".to_owned();
            while low < high {
                let mid = low + (high - low) / 2;
                let candidate = format!("{}…", &text[..boundaries[mid]]);
                let laid = lay_out(
                    &mut self.layout_cx,
                    fonts,
                    m,
                    &candidate,
                    Look::Plain(tone),
                    f32::INFINITY,
                );
                if laid.width <= wide {
                    best = candidate;
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            best
        });
        self.cache.labels.shortened.insert(key, fitted.clone());
        self.box_label(fonts, m, &fitted, tone, f32::INFINITY)
    }

    pub(super) fn draw_models(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
        anchor: [f32; 4],
    ) {
        let Some(menu) = self.menu.clone() else {
            return;
        };
        let rows = picker::rows(&self.offer(), &menu);
        let choices: Vec<ChoiceRow> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Choice(choice) => Some(choice.clone()),
                _ => None,
            })
            .collect();
        let slider = rows.iter().find_map(|row| match row {
            Row::Slider(slider) => Some(slider.clone()),
            _ => None,
        });
        let width = m.em(29.0).min(place.size[0] - m.em(1.0)).max(m.em(8.0));
        let left = anchor[0].clamp(
            place.origin[0] + m.em(0.5),
            (place.origin[0] + place.size[0] - width - m.em(0.5)).max(place.origin[0] + m.em(0.5)),
        );
        let bottom = (anchor[1] - m.em(0.5))
            .max(place.origin[1] + m.em(17.0))
            .min(place.origin[1] + place.size[1] - m.em(0.5));
        let search_height = m.em(3.2);
        let footer = if slider.is_some() {
            m.em(4.1)
        } else {
            m.em(0.6)
        };
        let row_height = m.em(3.2);
        let room = (bottom - place.origin[1] - m.em(0.5) - search_height - footer).max(row_height);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let visible = ((room / row_height) as usize).clamp(1, 7);
        let count = choices.len().max(5).min(visible);
        #[allow(clippy::cast_precision_loss)]
        let height = search_height + count as f32 * row_height + footer;
        let top = (bottom - height).max(place.origin[1] + m.em(0.5));
        let bounds = [left, top, width, (bottom - top).max(0.0)];
        let rail = m.em(3.4);
        let content = left + rail + m.em(0.6);
        let content_width = width - rail - m.em(1.2);
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: bounds,
            em: m.em,
            over: true,
        };
        self.hits.push((Hit::MenuSurface, place.in_pane(bounds)));
        let hits_start = self.hits.len();
        canvas.fill(
            Fill::rounded(bounds, Paint::Palette, m.em(0.65)).bordered(1.0, Paint::PaletteEdge),
        );
        canvas.fill(Fill::solid(
            [left + rail, top + m.em(0.6), 1.0, bounds[3] - m.em(1.2)],
            Paint::RaisedEdge,
        ));
        self.model_rail(
            &mut canvas,
            m,
            place,
            [left, top, rail, bounds[3]],
            &menu.filter,
        );
        self.model_search(
            &mut canvas,
            m,
            place,
            [content, top, content_width, search_height],
            &menu,
        );
        self.model_list(
            &mut canvas,
            m,
            place,
            [content, top + search_height, content_width, row_height],
            &choices,
            (&menu, count),
        );
        if let Some(slider) = slider {
            self.model_effort(
                &mut canvas,
                m,
                place,
                [content, bottom - footer, content_width, footer],
                &slider,
            );
        }
        for (_, hit) in &mut self.hits[hits_start..] {
            *hit = intersection(*hit, place.in_pane(bounds));
        }
    }

    fn model_search(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        menu: &crate::chat::Menu,
    ) {
        let [content, top, content_width, search_height] = rect;
        let placeholder = match &menu.filter {
            Filter::All => "Search models…".to_owned(),
            Filter::Favorites => "Search favorites…".to_owned(),
            Filter::Provider(name) => format!("Search {name}…"),
        };
        icon(
            canvas,
            Icon::Search,
            [content, top + m.em(0.75), m.em(1.2), m.em(1.2)],
            m.em(1.05),
            Paint::Icon,
        );
        let query = if menu.query.is_empty() {
            &placeholder
        } else {
            &menu.query
        };
        let label = self.short_label(
            canvas.fonts,
            m,
            query,
            content_width - m.em(2.0),
            if menu.query.is_empty() {
                Tone::Muted
            } else {
                Tone::Plain
            },
        );
        draw_laid(
            canvas,
            m,
            &label,
            [content + m.em(1.7), top + m.em(0.8)],
            content_width,
        );
        let caret = [
            content
                + m.em(1.7)
                + if menu.query.is_empty() {
                    0.0
                } else {
                    label.width
                },
            top + m.em(0.8),
            1.5,
            m.em(1.2),
        ];
        canvas.paint.ime = Some(caret);
        canvas.fill(Fill::solid(caret, Paint::Accent));
        self.keep_label(label);
        canvas.fill(Fill::solid(
            [content, top + search_height - m.em(0.4), content_width, 1.0],
            Paint::Accent,
        ));
        self.hits.push((
            Hit::ModelSearch,
            place.in_pane([content, top, content_width, search_height]),
        ));
    }

    fn model_list(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        choices: &[ChoiceRow],
        options: (&crate::chat::Menu, usize),
    ) {
        let [content, list_top, content_width, row_height] = rect;
        let (menu, count) = options;
        let mut offset = menu.offset.min(choices.len().saturating_sub(count));
        if menu.reveal
            && let Some(at) = choices.iter().position(|row| {
                menu.highlighted
                    == Some(Spot::Choice {
                        setting: row.setting.clone(),
                        value: row.value.clone(),
                    })
            })
        {
            if at < offset {
                offset = at;
            } else if at >= offset + count {
                offset = at + 1 - count;
            }
        }
        if let Some(menu) = &mut self.menu {
            menu.offset = offset;
            menu.reveal = false;
        }
        for (index, choice) in choices.iter().skip(offset).take(count).enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let y = list_top + index as f32 * row_height;
            let rect = [content, y, content_width, row_height - m.em(0.2)];
            self.model_row(canvas, m, place, rect, choice, menu);
        }
        if choices.is_empty() {
            let empty = self.short_label(
                canvas.fonts,
                m,
                "No matching models",
                content_width,
                Tone::Muted,
            );
            draw_laid(
                canvas,
                m,
                &empty,
                [content + m.em(0.4), list_top + m.em(0.8)],
                content_width,
            );
            self.keep_label(empty);
        }
    }

    fn model_effort(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        slider: &SliderRow,
    ) {
        let [content, y, content_width, _] = rect;
        canvas.fill(Fill::solid(
            [content, y, content_width, 1.0],
            Paint::RaisedEdge,
        ));
        let small = Measures::of(m.em * 0.78);
        let label = self.box_label(
            canvas.fonts,
            small,
            "Reasoning effort",
            Tone::Muted,
            f32::INFINITY,
        );
        draw_laid(
            canvas,
            small,
            &label,
            [content + m.em(0.45), y + m.em(0.4)],
            content_width,
        );
        self.keep_label(label);
        let choices: Vec<_> = slider
            .steps
            .iter()
            .enumerate()
            .map(|(index, (value, name))| (value, name, slider.at == Some(index), false))
            .chain(
                slider
                    .reset
                    .as_ref()
                    .map(|(value, name, selected)| (value, name, *selected, true)),
            )
            .collect();
        #[allow(clippy::cast_precision_loss)]
        let width = content_width / choices.len().max(1) as f32;
        for (index, (value, name, selected, reset)) in choices.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let bounds = [
                content + index as f32 * width,
                y + m.em(1.65),
                width - m.em(0.2),
                m.em(1.8),
            ];
            if selected {
                canvas.fill(Fill::rounded(bounds, Paint::ChatChosen, m.em(0.4)));
            }
            let laid = self.short_label(
                canvas.fonts,
                small,
                name,
                bounds[2] - m.em(0.3),
                if selected { Tone::Plain } else { Tone::Muted },
            );
            draw_laid(
                canvas,
                small,
                &laid,
                [
                    bounds[0] + (bounds[2] - laid.width) / 2.0,
                    bounds[1] + (bounds[3] - laid.height) / 2.0,
                ],
                bounds[2],
            );
            self.keep_label(laid);
            let hit = if reset {
                Hit::Reset {
                    setting: slider.setting.clone(),
                    value: value.clone(),
                }
            } else {
                Hit::Step {
                    setting: slider.setting.clone(),
                    value: value.clone(),
                }
            };
            self.hits.push((hit, place.in_pane(bounds)));
        }
    }

    fn model_rail(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        selected: &Filter,
    ) {
        let mut entries = vec![
            (Filter::All, Icon::Grid),
            (Filter::Favorites, Icon::StarFilled),
        ];
        entries.push((
            Filter::Provider(self.agent().to_owned()),
            Icon::provider(self.agent()),
        ));
        for provider in &self.node.providers {
            let entry = Filter::Provider(provider.title.clone());
            if !entries.iter().any(|(filter, _)| *filter == entry) {
                entries.push((entry, Icon::provider(&provider.title)));
            }
        }
        for (index, (filter, mark)) in entries.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let y = rect[1] + m.em(0.5) + index as f32 * m.em(2.8);
            if y + m.em(2.5) > rect[1] + rect[3] {
                break;
            }
            let bounds = [rect[0] + m.em(0.35), y, rect[2] - m.em(0.7), m.em(2.5)];
            let active = filter == *selected;
            if active {
                canvas.fill(Fill::rounded(
                    [
                        rect[0] + rect[2] - m.em(0.12),
                        y + m.em(0.55),
                        m.em(0.12),
                        m.em(1.4),
                    ],
                    Paint::Accent,
                    m.em(0.06),
                ));
            }
            icon(
                canvas,
                mark,
                bounds,
                m.em(1.45),
                if active && matches!(mark, Icon::Grid | Icon::StarFilled) {
                    Paint::Ink
                } else {
                    icon_paint(mark)
                },
            );
            self.hits
                .push((Hit::ModelFilter(filter), place.in_pane(bounds)));
        }
    }

    fn model_provider(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        rect: [f32; 4],
        provider: &str,
        wide: f32,
    ) {
        let provider_icon = Icon::provider(provider);
        icon(
            canvas,
            provider_icon,
            [
                rect[0] + m.em(0.6),
                rect[1] + m.em(1.65),
                m.em(0.8),
                m.em(0.8),
            ],
            m.em(0.8),
            icon_paint(provider_icon),
        );
        let sub = Measures::of(m.em * 0.8);
        let laid = self.short_label(canvas.fonts, sub, provider, wide - m.em(1.1), Tone::Muted);
        draw_laid(
            canvas,
            sub,
            &laid,
            [rect[0] + m.em(1.7), rect[1] + m.em(1.45)],
            wide,
        );
        self.keep_label(laid);
    }

    fn model_row(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        choice: &ChoiceRow,
        menu: &crate::chat::Menu,
    ) {
        let provider = picker::provider_name(&self.offer(), choice).to_owned();
        let favorite = menu
            .favorites
            .contains(&(provider.clone(), choice.value.clone()));
        let spot = Spot::Choice {
            setting: choice.setting.clone(),
            value: choice.value.clone(),
        };
        if menu.highlighted.as_ref() == Some(&spot) || choice.current {
            canvas.fill(Fill::rounded(rect, Paint::ChatChosen, m.em(0.45)));
        }
        let star_width = m.em(2.1);
        let family_width = if choice.family.is_some() {
            m.em(1.4)
        } else {
            0.0
        };
        let check_width = if choice.current { m.em(1.2) } else { 0.0 };
        let wide = (rect[2] - star_width - family_width - check_width - m.em(1.0)).max(m.em(1.0));
        let name = format!(
            "{}{}",
            choice.name,
            choice
                .version
                .as_ref()
                .map_or(String::new(), |v| format!(" {v}"))
        );
        let title = Measures::of(m.em * 0.94);
        let laid = self.short_label(canvas.fonts, title, &name, wide, Tone::Plain);
        draw_laid(
            canvas,
            title,
            &laid,
            [rect[0] + m.em(0.6), rect[1] + m.em(0.26)],
            wide,
        );
        self.keep_label(laid);
        self.model_provider(canvas, m, rect, &provider, wide);
        self.hits.push((
            Hit::Pick {
                setting: choice.setting.clone(),
                value: choice.value.clone(),
            },
            place.in_pane(intersection(canvas.clip, rect)),
        ));
        let star = [rect[0] + rect[2] - star_width, rect[1], star_width, rect[3]];
        icon(
            canvas,
            if favorite {
                Icon::StarFilled
            } else {
                Icon::Star
            },
            star,
            m.em(1.0),
            if favorite { Paint::Accent } else { Paint::Icon },
        );
        self.hits.push((
            Hit::Favorite(provider, choice.value.clone()),
            place.in_pane(star),
        ));
        if choice.current {
            icon(
                canvas,
                Icon::Check,
                [
                    star[0] - family_width - check_width,
                    rect[1],
                    check_width,
                    rect[3],
                ],
                m.em(0.9),
                Paint::Icon,
            );
        }
        if let Some((family, expanded)) = &choice.family {
            let at = [star[0] - family_width, star[1], family_width, star[3]];
            icon(
                canvas,
                if *expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                at,
                m.em(0.9),
                Paint::Icon,
            );
            self.hits
                .push((Hit::Expand(family.clone()), place.in_pane(at)));
        }
    }
}
