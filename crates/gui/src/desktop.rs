//! What the desktop says about how windows should look: the font for
//! interface text, and where the window's buttons go. Read from GNOME's
//! settings with `gsettings`, on the font thread so it costs startup
//! nothing; elsewhere, or if that fails, the defaults stand.

use std::process::Command;

/// The desktop's preferences for windows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Desktop {
    /// The interface font's family and size in points, if the desktop
    /// names one.
    pub(crate) font: Option<(String, f32)>,
    pub(crate) buttons: ButtonLayout,
}

/// A window button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Minimise,
    Maximise,
    Close,
}

/// Which buttons go at each end of the top bar, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ButtonLayout {
    pub(crate) left: Vec<Button>,
    pub(crate) right: Vec<Button>,
}

impl Default for ButtonLayout {
    /// GNOME's own default, as most Linux desktops have it: every button,
    /// on the right.
    fn default() -> Self {
        Self {
            left: Vec::new(),
            right: vec![Button::Minimise, Button::Maximise, Button::Close],
        }
    }
}

impl Desktop {
    /// Asks the desktop, taking the defaults for anything it doesn't say.
    pub(crate) fn read() -> Self {
        Self {
            font: gsetting("org.gnome.desktop.interface", "font-name")
                .as_deref()
                .and_then(parse_font),
            buttons: gsetting("org.gnome.desktop.wm.preferences", "button-layout")
                .as_deref()
                .map(parse_buttons)
                .unwrap_or_default(),
        }
    }
}

/// Opens `url` with the desktop's opener, `xdg-open`, as clicking a link
/// elsewhere would. It runs on a thread of its own, which waits for it so
/// it doesn't linger as a zombie, so the window never waits; it gets no
/// terminal, and what it says goes nowhere. Callers only pass URLs with a
/// known scheme (see [`crate::links`]), so one can't be taken for an
/// option.
pub(crate) fn open(url: &str) {
    let url = url.to_owned();
    let spawned = std::thread::Builder::new()
        .name("xdg-open".into())
        .spawn(move || {
            let status = Command::new("xdg-open")
                .arg(&url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            match status {
                Ok(status) if status.success() => {}
                Ok(status) => log::warn!("xdg-open {url} failed: {status}"),
                Err(err) => log::warn!("couldn't run xdg-open for {url}: {err}"),
            }
        });
    if let Err(err) = spawned {
        log::warn!("couldn't start a thread to open a link: {err}");
    }
}

/// A GNOME setting's value, without the quotes it's printed in.
fn gsetting(schema: &str, key: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    Some(value.trim().trim_matches('\'').to_owned())
}

/// A font named as GNOME names them, `Family Name 11`.
fn parse_font(name: &str) -> Option<(String, f32)> {
    let (family, size) = name.rsplit_once(' ')?;
    let size: f32 = size.parse().ok()?;
    (!family.is_empty() && size > 0.0).then(|| (family.to_owned(), size))
}

/// A button layout as GNOME writes it: buttons for the left, a colon,
/// buttons for the right, each list separated by commas. Names it doesn't
/// draw, such as `appmenu`, are left out.
fn parse_buttons(layout: &str) -> ButtonLayout {
    let buttons = |names: &str| {
        names
            .split(',')
            .filter_map(|name| match name.trim() {
                "minimize" => Some(Button::Minimise),
                "maximize" => Some(Button::Maximise),
                "close" => Some(Button::Close),
                _ => None,
            })
            .collect()
    };
    let (left, right) = layout.split_once(':').unwrap_or(("", layout));
    ButtonLayout {
        left: buttons(left),
        right: buttons(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_are_a_family_and_a_size() {
        assert_eq!(
            parse_font("Ubuntu Sans 11"),
            Some(("Ubuntu Sans".into(), 11.0))
        );
        assert_eq!(
            parse_font("Cantarell 10.5"),
            Some(("Cantarell".into(), 10.5))
        );
        assert_eq!(parse_font("Cantarell"), None);
    }

    #[test]
    fn buttons_go_where_the_layout_says() {
        assert_eq!(
            parse_buttons(":minimize,maximize,close"),
            ButtonLayout::default()
        );
        assert_eq!(
            parse_buttons("close,maximize:appmenu"),
            ButtonLayout {
                left: vec![Button::Close, Button::Maximise],
                right: Vec::new(),
            }
        );
        assert_eq!(
            parse_buttons("appmenu:close"),
            ButtonLayout {
                left: Vec::new(),
                right: vec![Button::Close],
            }
        );
    }
}
