//! Small vector marks, rasterised once per size into the existing glyph atlas.
//! Provider paths are vendored from Simple Icons; see `icons/README.md`.
use std::borrow::Cow;
use swash::zeno::{Cap, Join, Mask, Stroke, Transform};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Icon {
    OpenAi,
    Claude,
    Gemini,
    Agent,
    Bolt,
    Search,
    Star,
    StarFilled,
    ChevronDown,
    ChevronRight,
    Check,
    Send,
    Shield,
    Ring,
    Progress(u8),
}

impl Icon {
    pub(crate) fn provider(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("claude") {
            Self::Claude
        } else if name.contains("gemini") {
            Self::Gemini
        } else if name.contains("codex") || name.contains("openai") {
            Self::OpenAi
        } else {
            Self::Agent
        }
    }

    fn path(self) -> Cow<'static, str> {
        let svg = match self {
            Self::OpenAi => Some(include_str!("icons/openai.svg")),
            Self::Claude => Some(include_str!("icons/claude.svg")),
            Self::Gemini => Some(include_str!("icons/gemini.svg")),
            _ => None,
        };
        if let Some(svg) = svg {
            return svg
                .split("<path d=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .into();
        }
        match self {
            Self::Bolt => "M13 2L4 14H11L10 22L20 9H13Z".into(),
            Self::Search => "M17 10A7 7 0 1 1 3 10A7 7 0 1 1 17 10 M15 15L21 21".into(),
            Self::Star | Self::StarFilled => {
                "M12 3L14.8 8.7L21 9.6L16.5 14L17.6 20.2L12 17.3L6.4 20.2L7.5 14L3 9.6L9.2 8.7Z"
                    .into()
            }
            Self::ChevronDown => "M6 9L12 15L18 9".into(),
            Self::ChevronRight => "M9 6L15 12L9 18".into(),
            Self::Check => "M5 12L10 17L20 7".into(),
            Self::Send => "M12 20V4 M5 11L12 4L19 11".into(),
            Self::Shield => {
                "M12 3L20 6V11C20 16 17 19 12 22C7 19 4 16 4 11V6Z M8 12L11 15L16 10".into()
            }
            Self::Ring | Self::Progress(100..) => {
                "M12 1.5A10.5 10.5 0 1 1 12 22.5A10.5 10.5 0 1 1 12 1.5".into()
            }
            Self::Progress(0) => "".into(),
            Self::Progress(percent) => {
                let angle = f32::from(percent) / 100.0 * std::f32::consts::TAU;
                format!(
                    "M12 1.5A10.5 10.5 0 {} 1 {} {}",
                    u8::from(percent > 50),
                    12.0 + 10.5 * angle.sin(),
                    12.0 - 10.5 * angle.cos()
                )
                .into()
            }
            _ => "M5 7H19V20H5Z M9 12H9.01 M15 12H15.01 M9 16H15 M12 7V3".into(),
        }
    }

    pub(crate) fn rasterize(self, size: u16) -> Vec<u8> {
        let path = self.path();
        let mut mask = Mask::new(path.as_ref());
        let scale = f32::from(size) / 24.0;
        mask.size(u32::from(size), u32::from(size))
            .transform(Some(Transform::scale(scale, scale)));
        let mut stroke = Stroke::new(if matches!(self, Self::Ring | Self::Progress(_)) {
            1.3
        } else {
            1.7
        });
        stroke.cap(Cap::Round).join(Join::Round);
        if !matches!(
            self,
            Self::OpenAi | Self::Claude | Self::Gemini | Self::StarFilled
        ) {
            mask.style(stroke);
        }
        mask.render().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gauge_progress_fills_clockwise_and_leaves_its_centre_clear() {
        let counts: Vec<_> = [0, 25, 50, 100]
            .into_iter()
            .map(|percent| {
                let mask = Icon::Progress(percent).rasterize(48);
                assert_eq!(mask[24 * 48 + 24], 0);
                mask.iter().filter(|&&a| a > 128).count()
            })
            .collect();
        assert!(counts.windows(2).all(|pair| pair[0] < pair[1]));
        let quarter = Icon::Progress(25).rasterize(48);
        assert!(quarter[..24 * 48].iter().any(|&a| a > 128));
        assert!(quarter[27 * 48..].iter().all(|&a| a == 0));
    }

    #[test]
    fn vector_icons_render_at_both_display_scales() {
        for size in [18, 36] {
            for icon in [
                Icon::OpenAi,
                Icon::Claude,
                Icon::Gemini,
                Icon::Star,
                Icon::Search,
                Icon::Bolt,
                Icon::Progress(42),
            ] {
                assert_eq!(swash::zeno::validate_svg(&icon.path()), Ok(()), "{icon:?}");
                let mask = icon.rasterize(size);
                assert_eq!(mask.len(), usize::from(size).pow(2));
                assert!(mask.iter().any(|&a| a > 0));
                if matches!(icon, Icon::OpenAi | Icon::Claude | Icon::Gemini) {
                    assert!(
                        mask.iter().filter(|&&a| a > 128).count() > mask.len() / 10,
                        "{icon:?}: incomplete path"
                    );
                }
            }
            assert!(Icon::Progress(0).rasterize(size).iter().all(|&a| a == 0));
        }
    }
}
