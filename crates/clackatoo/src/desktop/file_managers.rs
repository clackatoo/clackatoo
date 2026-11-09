//! "Open in clackatoo" in file managers' menus, beside the desktop entry
//! that puts clackatoo in every file manager's "Open With".
//!
//! Each file manager has its own way of adding to its menus, and each is
//! added only when that file manager is installed: Dolphin's service menu
//! (KDE), Nemo's actions (Cinnamon), Thunar's custom actions (Xfce), and
//! for GNOME's Files, an extension, if nautilus-python is there to run it;
//! without it, Files has only "Open With". Each runs `clackatoo open --`
//! with the paths chosen, or with the folder shown for a click on its
//! background, and they're given as each file manager gives them: never
//! through a shell, but for Thunar, which runs every command with `sh -c`,
//! quoting each path itself.
//!
//! A file clackatoo writes says so, as the entry does, and only those are
//! replaced or removed without `--force`. Thunar keeps every custom action
//! in one file, `uca.xml`, the user's own among them, so clackatoo's is
//! added to it, marked by its ID, leaving the rest byte for byte; a file
//! it can't be sure of reading right is left alone, and said so.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use anyhow::Context as _;

use super::{MANAGED, escape_value, exec_argument, mime_types, write, write_executable};

/// A file manager clackatoo adds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileManager {
    Dolphin,
    Nemo,
    Thunar,
    /// GNOME's Files (Nautilus), through nautilus-python.
    Files,
}

impl FileManager {
    pub(crate) const ALL: [Self; 4] = [Self::Dolphin, Self::Nemo, Self::Thunar, Self::Files];

    /// How it comes to show what's added, if it's running now and doesn't
    /// see it by itself.
    pub(crate) fn loads_it(self) -> Option<&'static str> {
        match self {
            Self::Dolphin | Self::Nemo => None,
            Self::Thunar => Some(
                "Thunar shows it from its next start: `thunar -q` stops one running, as one does \
                 under `thunar --daemon`. Saving Thunar's custom actions in a window opened before \
                 then takes it out again.",
            ),
            Self::Files => Some(
                "GNOME Files shows it from its next start: `nautilus -q` stops the one running.",
            ),
        }
    }

    /// Its name, to say what was added.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Dolphin => "Dolphin",
            Self::Nemo => "Nemo",
            Self::Thunar => "Thunar",
            Self::Files => "GNOME Files",
        }
    }
}

/// The file managers installed, which clackatoo adds to: each found as a
/// program on `PATH`; GNOME's Files only with nautilus-python's loader in
/// its extensions.
pub(crate) fn installed() -> Vec<FileManager> {
    let mut found: Vec<FileManager> = [
        (FileManager::Dolphin, "dolphin"),
        (FileManager::Nemo, "nemo"),
        (FileManager::Thunar, "thunar"),
    ]
    .into_iter()
    .filter(|(_, program)| on_path(program))
    .map(|(manager, _)| manager)
    .collect();
    if on_path("nautilus") && nautilus_python() {
        found.push(FileManager::Files);
    }
    found
}

/// Whether `program` is an executable file in a directory on `PATH`.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        fs::metadata(dir.join(program))
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

/// Whether nautilus-python is installed: its loader, in the directory
/// Nautilus loads extensions from, which is `extensions-4` from Nautilus
/// 43 on, `extensions-3.0` before, in the system's library directory, or
/// a multiarch one inside it.
fn nautilus_python() -> bool {
    let roots = ["/usr/lib", "/usr/lib64", "/usr/local/lib"];
    let mut dirs: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
    for root in roots {
        if let Ok(entries) = fs::read_dir(root) {
            dirs.extend(entries.filter_map(|entry| Some(entry.ok()?.path())));
        }
    }
    dirs.iter().any(|dir| {
        ["extensions-4", "extensions-3.0"].iter().any(|version| {
            dir.join("nautilus")
                .join(version)
                .join("libnautilus-python.so")
                .exists()
        })
    })
}

/// Where the files go.
#[derive(Debug)]
pub(crate) struct Places {
    /// `$XDG_DATA_HOME`, or `~/.local/share`.
    pub(crate) data: PathBuf,
    /// `$XDG_CONFIG_HOME`, or `~/.config`.
    pub(crate) config: PathBuf,
    /// `$XDG_CONFIG_DIRS`, or `/etc/xdg`: where Thunar finds the custom
    /// actions it starts with, while the user has none of their own.
    pub(crate) system_config: Vec<PathBuf>,
}

impl Places {
    fn dolphin(&self) -> PathBuf {
        self.data.join("kio/servicemenus/clackatoo.desktop")
    }

    /// The action for what's selected, and the one for the folder shown.
    fn nemo(&self) -> [PathBuf; 2] {
        let dir = self.data.join("nemo/actions");
        [
            dir.join("clackatoo.nemo_action"),
            dir.join("clackatoo-here.nemo_action"),
        ]
    }

    fn thunar(&self) -> PathBuf {
        self.config.join("Thunar/uca.xml")
    }

    fn files(&self) -> PathBuf {
        self.data.join("nautilus-python/extensions/clackatoo.py")
    }

    /// The files clackatoo writes whole, for `manager`.
    fn whole(&self, manager: FileManager) -> Vec<PathBuf> {
        match manager {
            FileManager::Dolphin => vec![self.dolphin()],
            FileManager::Nemo => self.nemo().to_vec(),
            FileManager::Thunar => Vec::new(),
            FileManager::Files => vec![self.files()],
        }
    }
}

/// What the hooks for `managers` are, written, starting `program`: each
/// file and its text, and whether it's to be executable, as Dolphin
/// wants its service menus.
fn whole_files(
    places: &Places,
    program: &str,
    managers: &[FileManager],
) -> Vec<(PathBuf, String, bool)> {
    let mut files = Vec::new();
    for &manager in managers {
        match manager {
            FileManager::Dolphin => files.push((places.dolphin(), dolphin(program), true)),
            FileManager::Nemo => {
                let [selected, here] = places.nemo();
                files.push((selected, nemo(program, Nemo::Selected), false));
                files.push((here, nemo(program, Nemo::Here), false));
            }
            FileManager::Thunar => {}
            FileManager::Files => files.push((places.files(), nautilus(program), false)),
        }
    }
    files
}

/// Whether a hook clackatoo writes whole is clackatoo's: marked, as the
/// entry is, or as the extension is.
fn is_ours(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim() == MANAGED || line == PYTHON_MANAGED)
}

/// The hooks for `managers` that are there but aren't clackatoo's.
pub(crate) fn theirs(places: &Places, managers: &[FileManager]) -> anyhow::Result<Vec<PathBuf>> {
    let mut theirs = Vec::new();
    for &manager in managers {
        for path in places.whole(manager) {
            match fs::read(&path) {
                Ok(bytes) if !is_ours(&String::from_utf8_lossy(&bytes)) => theirs.push(path),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(anyhow::Error::new(err)
                        .context(format!("couldn't read {}", path.display())));
                }
            }
        }
    }
    Ok(theirs)
}

/// What adding to file managers did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Added {
    /// The files written, or changed.
    pub(crate) written: Vec<PathBuf>,
    /// The file managers added to.
    pub(crate) to: Vec<FileManager>,
    /// Why some weren't.
    pub(crate) not: Vec<String>,
}

/// Adds "Open in clackatoo", starting `program`, to `managers`. The caller
/// has checked there's nothing in the way that isn't clackatoo's (see
/// [`theirs`]), or was told to replace it.
pub(crate) fn install(
    places: &Places,
    program: &str,
    managers: &[FileManager],
) -> anyhow::Result<Added> {
    let mut added = Added::default();
    for (path, text, executable) in whole_files(places, program, managers) {
        if executable {
            write_executable(&path, &text)?;
        } else {
            write(&path, &text)?;
        }
        added.written.push(path);
    }
    for &manager in managers {
        if manager != FileManager::Thunar {
            added.to.push(manager);
            continue;
        }
        match add_to_thunar(places, program)? {
            Ok(Some(path)) => {
                added.written.push(path);
                added.to.push(manager);
            }
            Ok(None) => added.to.push(manager),
            Err(why) => added.not.push(why),
        }
    }
    Ok(added)
}

/// Rewrites clackatoo's hooks that are there to start `program`, as after
/// an update, and returns the files changed. Adds none that aren't: a
/// file manager installed since, or a hook taken away, waits for
/// `clackatoo desktop install`.
pub(crate) fn refresh(places: &Places, program: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (path, text, executable) in whole_files(places, program, &FileManager::ALL) {
        match fs::read_to_string(&path) {
            Ok(old) if is_ours(&old) && old != text => {}
            _ => continue,
        }
        if executable {
            write_executable(&path, &text)?;
        } else {
            write(&path, &text)?;
        }
        written.push(path);
    }
    let has_ours = fs::read_to_string(places.thunar())
        .is_ok_and(|old| Uca::read(&old).is_ok_and(|uca| !uca.ours.is_empty()));
    if has_ours && let Ok(Some(path)) = add_to_thunar(places, program)? {
        written.push(path);
    }
    Ok(written)
}

/// Removes clackatoo's hooks, and with `force` any file where one goes,
/// whoever wrote it; returns the files removed or changed, and why any
/// was left.
pub(crate) fn remove(places: &Places, force: bool) -> anyhow::Result<(Vec<PathBuf>, Vec<String>)> {
    let mut removed = Vec::new();
    for manager in FileManager::ALL {
        for path in places.whole(manager) {
            match fs::read(&path) {
                Ok(bytes) if force || is_ours(&String::from_utf8_lossy(&bytes)) => {}
                Ok(_) => continue,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => {
                    return Err(anyhow::Error::new(err)
                        .context(format!("couldn't read {}", path.display())));
                }
            }
            fs::remove_file(&path)
                .with_context(|| format!("couldn't remove {}", path.display()))?;
            removed.push(path);
        }
    }
    let mut left = Vec::new();
    let path = places.thunar();
    match fs::read_to_string(&path) {
        Ok(text) => match Uca::read(&text) {
            Ok(uca) if uca.ours.is_empty() => {}
            Ok(uca) => {
                let kept = uca.without_ours();
                // All that's left is what clackatoo started the file with:
                // without it, Thunar shows the system's actions again.
                let system = system_uca(places).and_then(|(_, text)| text.ok());
                if kept == UCA_EMPTY || system.is_some_and(|system| kept == system) {
                    fs::remove_file(&path)
                        .with_context(|| format!("couldn't remove {}", path.display()))?;
                } else {
                    write(&path, &kept)?;
                }
                removed.push(path);
            }
            Err(why) => left.push(format!(
                "{} was left as it is, as {why}: take out the action with the ID {UCA_ID} yourself",
                path.display()
            )),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        // Not UTF-8: it can't have clackatoo's action.
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {}
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("couldn't read {}", path.display()))
            );
        }
    }
    Ok((removed, left))
}

/// The menu item's words.
const LABEL: &str = "Open in clackatoo";

/// What it does, for a tooltip.
const DESCRIPTION: &str = "A folder in a terminal, a file in an editor, one tab each";

/// Dolphin's service menu: a desktop entry, so the program is quoted as
/// the desktop entry's is, and KIO makes each path an argument of its own
/// (`%F`). On a click on a folder's background, the folder is the one
/// file. Service menus of the user's own must be executable.
fn dolphin(program: &str) -> String {
    let exec = escape_value(&exec_argument(program));
    format!(
        "[Desktop Entry]\n\
         Type=Service\n\
         MimeType={}\n\
         Actions=open;\n\
         X-KDE-ServiceTypes=KonqPopupMenu/Plugin\n\
         X-KDE-Priority=TopLevel\n\
         {MANAGED}\n\
         \n\
         [Desktop Action open]\n\
         Name={LABEL}\n\
         Icon=clackatoo\n\
         Exec={exec} open -- %F\n",
        folders_and_text()
    )
}

/// The kinds of file the hooks are offered for: folders, which the desktop
/// entry doesn't claim, so that the desktop never opens folders with
/// clackatoo, and the kinds of text it does.
fn folders_and_text() -> String {
    format!("inode/directory;{}", mime_types())
}

/// Which of Nemo's two actions.
#[derive(Clone, Copy)]
enum Nemo {
    /// For what's selected.
    Selected,
    /// For the folder shown, on a click on its background.
    Here,
}

/// A Nemo action. Nemo splits the command into arguments as a shell would,
/// without running one (`g_shell_parse_argv`), with each path in single
/// quotes (`Quote=single`), inside which nothing is special but a single
/// quote, which Nemo closes and reopens the quotes around: so the
/// program is quoted alike. `%` begins Nemo's own codes, so a `%` in it is
/// doubled.
fn nemo(program: &str, which: Nemo) -> String {
    let program = escape_value(&single_quoted(program).replace('%', "%%"));
    let (selection, mime, paths) = match which {
        Nemo::Selected => ("notnone", folders_and_text(), "%F"),
        Nemo::Here => ("none", "inode/directory;".to_owned(), "%P"),
    };
    format!(
        "[Nemo Action]\n\
         Name={LABEL}\n\
         Comment={DESCRIPTION}\n\
         Exec={program} open -- {paths}\n\
         Icon-Name=clackatoo\n\
         Selection={selection}\n\
         Mimetypes={mime}\n\
         Quote=single\n\
         {MANAGED}\n"
    )
}

/// `text` in a shell's single quotes, each single quote in it ended,
/// escaped and begun again.
fn single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The line that marks the extension as clackatoo's.
const PYTHON_MANAGED: &str = "# X-Clackatoo-Managed: written by `clackatoo desktop install`";

/// The extension for GNOME's Files, run by nautilus-python: an item for
/// folders and text files selected, and one for the folder shown. Paths
/// go to the program as arguments, as Python's `subprocess` gives them,
/// byte for byte, never through a shell.
fn nautilus(program: &str) -> String {
    format!(
        r#"{PYTHON_MANAGED}
"""{LABEL}, in GNOME Files' menus: each folder in a new terminal tab,
each text file in a new editor tab, as `clackatoo open` does."""

import subprocess

import gi

try:
    gi.require_version("Nautilus", "4.0")
except ValueError:
    gi.require_version("Nautilus", "3.0")
from gi.repository import Gio, GLib, GObject, Nautilus

PROGRAM = {program}


def _path(file):
    location = file.get_location()
    return location.get_path() if location else None


def _opens(file):
    if not _path(file):
        return False
    if file.is_directory():
        return True
    kind = file.get_mime_type()
    return bool(kind) and Gio.content_type_is_a(kind, "text/plain")


def _open(_item, files):
    paths = [path for path in map(_path, files) if path]
    if not paths:
        return
    try:
        child = subprocess.Popen(
            [PROGRAM, "open", "--", *paths],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except OSError:
        return
    # Reaped once it ends, as Files goes on running.
    GLib.child_watch_add(GLib.PRIORITY_DEFAULT, child.pid, lambda *_: None)


def _item(name, files):
    item = Nautilus.MenuItem(name="Clackatoo::" + name, label="{LABEL}", tip="{DESCRIPTION}", icon="clackatoo")
    item.connect("activate", _open, files)
    return item


class OpenInClackatoo(GObject.GObject, Nautilus.MenuProvider):
    # Nautilus 4 gives the files alone; 3 gives its window first.
    def get_file_items(self, *args):
        files = args[-1]
        if not files or not all(map(_opens, files)):
            return []
        return [_item("Open", files)]

    def get_background_items(self, *args):
        folder = args[-1]
        if not folder or not _path(folder):
            return []
        return [_item("OpenHere", [folder])]
"#,
        program = python_string(program),
    )
}

/// `text` as a Python string literal.
fn python_string(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => literal.push_str(r"\\"),
            '\'' => literal.push_str(r"\'"),
            ch if ch.is_control() => {
                let _ = write!(literal, "\\U{:08x}", u32::from(ch));
            }
            ch => literal.push(ch),
        }
    }
    literal.push('\'');
    literal
}

/// The ID that marks clackatoo's action in Thunar's `uca.xml`.
const UCA_ID: &str = "clackatoo-open";

/// clackatoo's action for Thunar's `uca.xml`. Thunar runs the command
/// with `sh -c`, having put each path in `%F` in single quotes
/// (`g_shell_quote`), so the program is quoted alike; Thunar takes `%%`
/// for a `%`. It's offered for folders, the folder shown among them, and
/// text files. Thunar starts it as it would an app that reports its
/// startup, giving it a token for the window to take the focus with.
fn thunar_action(program: &str) -> String {
    let command = format!("{} open -- %F", single_quoted(program).replace('%', "%%"));
    format!(
        "<action>\n\
         \t<icon>clackatoo</icon>\n\
         \t<name>{LABEL}</name>\n\
         \t<submenu></submenu>\n\
         \t<unique-id>{UCA_ID}</unique-id>\n\
         \t<command>{}</command>\n\
         \t<description>{DESCRIPTION}</description>\n\
         \t<range></range>\n\
         \t<patterns>*</patterns>\n\
         \t<startup-notify/>\n\
         \t<directories/>\n\
         \t<text-files/>\n\
         </action>\n",
        xml_escaped(&command)
    )
}

/// `text` as XML's character data.
fn xml_escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// What a new `uca.xml` holds before clackatoo's action is added.
const UCA_EMPTY: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n</actions>\n";

/// The system's `uca.xml`, which Thunar shows while the user has none of
/// their own: the first in `$XDG_CONFIG_DIRS` that's there, and what's in
/// it, if it can be read.
fn system_uca(places: &Places) -> Option<(PathBuf, io::Result<String>)> {
    (places.system_config.iter())
        .map(|dir| dir.join("Thunar/uca.xml"))
        .find(|path| path.exists())
        .map(|path| {
            let text = fs::read_to_string(&path);
            (path, text)
        })
}

/// Adds clackatoo's action to Thunar's `uca.xml`, starting `program`, in
/// place of any it has; returns the file if it changed it, or why it
/// couldn't. With no file of the user's own, Thunar shows the actions in
/// the system's (`/etc/xdg/Thunar/uca.xml`), which theirs would hide, so
/// theirs starts as a copy of that; if that can't be read, none is
/// written.
fn add_to_thunar(
    places: &Places,
    program: &str,
) -> anyhow::Result<Result<Option<PathBuf>, String>> {
    let path = places.thunar();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => match system_uca(places) {
            None => UCA_EMPTY.to_owned(),
            Some((_, Ok(text))) if Uca::read(&text).is_ok() => text,
            Some((system, read)) => {
                let why = match read {
                    Ok(text) => Uca::read(&text).err().unwrap_or_default(),
                    Err(err) => err.to_string(),
                };
                return Ok(Err(format!(
                    "Thunar's actions in {} couldn't be read ({why}), and a file of your own \
                     would hide them, so clackatoo added none to Thunar",
                    system.display()
                )));
            }
        },
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            return Ok(Err(format!(
                "Thunar's {} isn't UTF-8, so clackatoo left it as it is",
                path.display()
            )));
        }
        Err(err) => {
            return Err(
                anyhow::Error::new(err).context(format!("couldn't read {}", path.display()))
            );
        }
    };
    let uca = match Uca::read(&text) {
        Ok(uca) => uca,
        Err(why) => {
            return Ok(Err(format!(
                "Thunar's {} was left as it is, as {why}",
                path.display()
            )));
        }
    };
    let added = uca.with_ours(&thunar_action(program));
    if fs::read_to_string(&path).is_ok_and(|old| old == added) {
        return Ok(Ok(None));
    }
    write(&path, &added)?;
    Ok(Ok(Some(path)))
}

/// `text` with what isn't markup clackatoo reads put out of the way, so
/// that what's in it isn't taken for actions: the XML declaration and
/// other processing instructions, the document type with its own
/// declarations, comments and character data sections, each byte made a
/// space, but for line breaks. So a place in what's returned is the same
/// place in `text`. Something else beginning `<!`, or one that doesn't
/// end, means the file isn't one clackatoo can be sure of.
fn masked(text: &str) -> Result<String, String> {
    let mut bytes = text.as_bytes().to_vec();
    let mut at = 0;
    while let Some(found) = text[at..].find('<') {
        let start = at + found;
        let rest = &text[start..];
        let end = if rest.starts_with("<!--") {
            rest.find("-->").map(|end| end + "-->".len())
        } else if rest.starts_with("<![CDATA[") {
            rest.find("]]>").map(|end| end + "]]>".len())
        } else if rest.starts_with("<?") {
            rest.find("?>").map(|end| end + "?>".len())
        } else if rest.starts_with("<!DOCTYPE") {
            doctype_end(rest)
        } else if rest.starts_with("<!") {
            return Err("it has markup clackatoo doesn't read".to_owned());
        } else {
            at = start + 1;
            continue;
        };
        let end = start + end.ok_or("something in it doesn't end")?;
        for byte in &mut bytes[start..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        at = end;
    }
    String::from_utf8(bytes).map_err(|_| "it isn't UTF-8".to_owned())
}

/// Where the document type declaration at the start of `text` ends, past
/// its own declarations in brackets, if it has them.
fn doctype_end(text: &str) -> Option<usize> {
    let open = text.find(['[', '>'])?;
    if text.as_bytes()[open] == b'>' {
        return Some(open + 1);
    }
    let mut at = open;
    loop {
        at += text[at..].find(']')? + 1;
        let after = &text[at..];
        let spaces = after.len() - after.trim_start().len();
        if after.trim_start().starts_with('>') {
            return Some(at + spaces + 1);
        }
    }
}

/// Thunar's `uca.xml`, read only as far as needed to find clackatoo's
/// action and where another goes: so that what isn't clackatoo's is kept
/// byte for byte, comments and the document type among it, as the file
/// Thunar installs has. If anything else could make that reading wrong,
/// such as an action with attributes, or one inside another, the file
/// isn't read at all.
struct Uca<'a> {
    text: &'a str,
    /// Where `</actions>` begins.
    end: usize,
    /// Each of clackatoo's actions, from the start of its line to the end
    /// of the line its `</action>` ends.
    ours: Vec<(usize, usize)>,
}

impl<'a> Uca<'a> {
    fn read(text: &'a str) -> Result<Self, String> {
        let original = text;
        // Read past comments and the like, keeping where everything is.
        let masked = masked(text)?;
        let text = masked.as_str();
        let body = text.trim_start();
        if !body.starts_with("<actions>") || text.matches("<actions").count() != 1 {
            return Err("it isn't a list of actions".to_owned());
        }
        let trimmed = text.trim_end();
        if !trimmed.ends_with("</actions>") || text.matches("</actions").count() != 1 {
            return Err("its list of actions doesn't end".to_owned());
        }
        let end = trimmed.len() - "</actions>".len();
        // Every action is plain `<action>`, so that what's found between
        // one and its end is all its own.
        if text.matches("<action").count()
            != text.matches("<action>").count() + text.matches("<actions>").count()
        {
            return Err("an action in it has attributes".to_owned());
        }
        // Each action ends before the next begins.
        let mut tags: Vec<(usize, bool)> = (text.match_indices("<action>"))
            .map(|(at, _)| (at, true))
            .chain(text.match_indices("</action>").map(|(at, _)| (at, false)))
            .collect();
        tags.sort_unstable();
        let alternate = (tags.iter().enumerate()).all(|(nth, &(_, opens))| opens == (nth % 2 == 0));
        if !alternate || !tags.len().is_multiple_of(2) {
            return Err("its actions don't each end before the next".to_owned());
        }
        let id = format!("<unique-id>{UCA_ID}</unique-id>");
        let mut ours = Vec::new();
        for (at, _) in text.match_indices(&id) {
            let start = text[..at]
                .rfind("<action>")
                .filter(|&start| !text[start..at].contains("</action>"))
                .ok_or("clackatoo's ID is outside an action")?;
            let close = at
                + text[at..]
                    .find("</action>")
                    .filter(|&close| !text[at..at + close].contains("<action>"))
                    .ok_or("clackatoo's action doesn't end")?;
            let line_start = text[..start].rfind('\n').map_or(0, |newline| newline + 1);
            let start = if text[line_start..start].trim().is_empty() {
                line_start
            } else {
                start
            };
            let mut finish = close + "</action>".len();
            if text[finish..].starts_with('\n') {
                finish += 1;
            }
            ours.push((start, finish));
        }
        Ok(Self {
            text: original,
            end,
            ours,
        })
    }

    /// The file without clackatoo's actions.
    fn without_ours(&self) -> String {
        let mut kept = String::with_capacity(self.text.len());
        let mut from = 0;
        for &(start, finish) in &self.ours {
            kept.push_str(&self.text[from..start]);
            from = finish;
        }
        kept.push_str(&self.text[from..]);
        kept
    }

    /// The file with `action` as clackatoo's, in place of any, last.
    fn with_ours(&self, action: &str) -> String {
        let without = self.without_ours();
        let removed: usize = self.ours.iter().map(|(start, finish)| finish - start).sum();
        let end = self.end - removed;
        let mut added = String::with_capacity(without.len() + action.len() + 1);
        added.push_str(&without[..end]);
        if !added.ends_with('\n') {
            added.push('\n');
        }
        added.push_str(action);
        added.push_str(&without[end..]);
        added
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::process::Command;

    use super::super::unescape_value;
    use super::*;

    const PROGRAM: &str = "/home/someone/.local/bin/clackatoo";

    fn setup() -> (tempfile::TempDir, Places) {
        let home = tempfile::tempdir().unwrap();
        let places = Places {
            data: home.path().join("data"),
            config: home.path().join("config"),
            system_config: vec![home.path().join("etc/xdg")],
        };
        (home, places)
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    /// Every file under `dir`, and what's in it.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let Ok(entries) = fs::read_dir(dir) else {
            return found;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(snapshot(&path));
            } else {
                found.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
        found.sort();
        found
    }

    /// The values of `key` in `text`, a key file.
    fn values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
        let prefix = format!("{key}=");
        (text.lines())
            .filter_map(|line| line.strip_prefix(prefix.as_str()))
            .collect()
    }

    #[test]
    fn each_file_manager_installed_is_given_its_hook_and_only_those() {
        let (_home, places) = setup();

        let added = install(
            &places,
            PROGRAM,
            &[FileManager::Dolphin, FileManager::Files],
        )
        .unwrap();

        assert_eq!(added.to, [FileManager::Dolphin, FileManager::Files]);
        assert_eq!(added.written, [places.dolphin(), places.files()]);
        assert!(added.not.is_empty());
        let dolphin = read(&places.dolphin());
        assert_eq!(values(&dolphin, "Exec"), [format!("{PROGRAM} open -- %F")]);
        // Offered for folders, which the desktop entry doesn't claim.
        assert_eq!(values(&dolphin, "MimeType"), [folders_and_text()]);
        assert!(folders_and_text().starts_with("inode/directory;text/plain;"));
        assert!(is_ours(&dolphin));
        // Dolphin runs only service menus a user can run.
        let mode = fs::metadata(places.dolphin()).unwrap().permissions().mode();
        assert_ne!(mode & 0o100, 0, "{mode:o}");
        assert!(read(&places.files()).contains(&format!("PROGRAM = '{PROGRAM}'")));
        assert!(!places.nemo()[0].exists());
        assert!(!places.thunar().exists());
    }

    #[test]
    fn installing_again_changes_nothing_and_removing_takes_away_only_clackatoos() {
        let (home, places) = setup();
        let all = FileManager::ALL;
        install(&places, PROGRAM, &all).unwrap();
        let once = snapshot(home.path());
        assert_eq!(once.len(), 5, "{once:?}");

        let again = install(&places, PROGRAM, &all).unwrap();

        assert_eq!(snapshot(home.path()), once);
        // Thunar's file, unchanged, isn't said to be written.
        assert!(!again.written.contains(&places.thunar()));
        assert!(theirs(&places, &all).unwrap().is_empty());

        // Someone's own action beside clackatoo's stays.
        let mine = places.data.join("nemo/actions/mine.nemo_action");
        fs::write(&mine, "[Nemo Action]\nName=Mine\n").unwrap();
        let (removed, left) = remove(&places, false).unwrap();
        assert_eq!(removed.len(), 5, "{removed:?}");
        assert!(left.is_empty());
        // Thunar's file, which clackatoo made, goes too.
        assert_eq!(
            snapshot(home.path()),
            [(mine.clone(), b"[Nemo Action]\nName=Mine\n".to_vec())]
        );
        // Nothing left of clackatoo's: nothing to do.
        assert_eq!(remove(&places, false).unwrap(), (vec![], vec![]));
    }

    #[test]
    fn a_hook_clackatoo_didnt_write_is_theirs_and_removed_only_by_force() {
        let (_home, places) = setup();
        let [nemo, _] = places.nemo();
        write(&nemo, "[Nemo Action]\nName=Someone's own\n").unwrap();

        assert_eq!(
            theirs(&places, &[FileManager::Nemo]).unwrap(),
            std::slice::from_ref(&nemo)
        );
        assert!(theirs(&places, &[FileManager::Dolphin]).unwrap().is_empty());
        assert_eq!(remove(&places, false).unwrap(), (vec![], vec![]));
        assert!(nemo.exists());
        assert_eq!(remove(&places, true).unwrap(), (vec![nemo.clone()], vec![]));
        assert!(!nemo.exists());
    }

    #[test]
    fn refreshing_rewrites_clackatoos_hooks_for_the_program_and_adds_none() {
        let (_home, places) = setup();
        install(
            &places,
            "/old/clackatoo",
            &[FileManager::Nemo, FileManager::Thunar],
        )
        .unwrap();

        let written = refresh(&places, PROGRAM).unwrap();

        let [selected, here] = places.nemo();
        assert_eq!(written, [selected.clone(), here.clone(), places.thunar()]);
        assert_eq!(
            values(&read(&selected), "Exec"),
            [format!("'{PROGRAM}' open -- %F")]
        );
        assert_eq!(
            values(&read(&here), "Exec"),
            [format!("'{PROGRAM}' open -- %P")]
        );
        assert!(read(&places.thunar()).contains(&format!("<command>&apos;{PROGRAM}&apos; open")));
        assert!(!read(&places.thunar()).contains("/old/"));
        assert!(!places.dolphin().exists());
        assert!(!places.files().exists());
        // Brought up to date already: nothing written.
        assert!(refresh(&places, PROGRAM).unwrap().is_empty());
    }

    /// A user's own `uca.xml`, as Thunar writes it.
    const USERS_UCA: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n<action>\n\
                             \t<icon>utilities-terminal</icon>\n\t<name>Open Terminal Here</name>\n\
                             \t<submenu></submenu>\n\t<unique-id>1700000000000000-1</unique-id>\n\
                             \t<command>exo-open --working-directory %f --launch \
                             TerminalEmulator</command>\n\t<description>Mine &amp; only \
                             mine</description>\n\t<range></range>\n\t<patterns>*</patterns>\n\
                             \t<startup-notify/>\n\t<directories/>\n</action>\n</actions>\n";

    #[test]
    fn thunars_actions_keep_the_users_own_byte_for_byte() {
        let (_home, places) = setup();
        write(&places.thunar(), USERS_UCA).unwrap();

        install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();

        let added = read(&places.thunar());
        assert!(added.starts_with(&USERS_UCA[..USERS_UCA.len() - "</actions>\n".len()]));
        assert!(added.ends_with(&format!("{}</actions>\n", thunar_action(PROGRAM))));
        assert_eq!(added.matches(UCA_ID).count(), 1);
        // Moved about by Thunar, which writes every action back as it
        // saves, it's still found, and replaced where it is.
        let moved = USERS_UCA.replace(
            "<actions>\n",
            &format!("<actions>\n{}", thunar_action("/old/clackatoo")),
        );
        write(&places.thunar(), &moved).unwrap();
        install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();
        assert_eq!(read(&places.thunar()), added);

        remove(&places, false).unwrap();
        assert_eq!(read(&places.thunar()), USERS_UCA);
    }

    #[test]
    fn thunar_starts_from_the_systems_actions_which_a_file_of_the_users_own_would_hide() {
        let (_home, places) = setup();
        let system = places.system_config[0].join("Thunar/uca.xml");
        write(&system, USERS_UCA).unwrap();

        install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();

        assert!(read(&places.thunar()).contains("Open Terminal Here"));
        assert!(read(&places.thunar()).contains(UCA_ID));
        // With clackatoo's taken out, it's the system's again, so it goes,
        // and Thunar shows the system's as before.
        remove(&places, false).unwrap();
        assert!(!places.thunar().exists());
        assert_eq!(read(&system), USERS_UCA);
    }

    /// A system's actions, installed as `/etc/xdg/Thunar/uca.xml`, in the
    /// shape of the file Thunar installs: a document type with its own
    /// declarations, a comment, actions with no ID, and names in another
    /// language too.
    const SYSTEM_UCA: &str = include_str!("system-uca.xml");

    #[test]
    fn thunars_system_actions_are_read_and_kept_comments_and_all() {
        let (_home, places) = setup();
        let system = places.system_config[0].join("Thunar/uca.xml");
        write(&system, SYSTEM_UCA).unwrap();

        let added = install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();

        assert_eq!(added.to, [FileManager::Thunar], "{:?}", added.not);
        let seeded = read(&places.thunar());
        let before = SYSTEM_UCA.len() - "</actions>\n".len();
        assert!(seeded.starts_with(&SYSTEM_UCA[..before]), "{seeded}");
        assert!(seeded.ends_with(&format!("{}</actions>\n", thunar_action(PROGRAM))));
        // Installing again finds it there, and changes nothing.
        let again = install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();
        assert!(again.written.is_empty());
        assert_eq!(read(&places.thunar()), seeded);

        // The user's own file, comments and document type kept.
        let theirs = SYSTEM_UCA.replace("Terminal here", "My terminal");
        write(&places.thunar(), &theirs).unwrap();
        install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();
        remove(&places, false).unwrap();
        assert_eq!(read(&places.thunar()), theirs);

        // The system's, taken back out: the file goes.
        write(&places.thunar(), &seeded).unwrap();
        remove(&places, false).unwrap();
        assert!(!places.thunar().exists());
    }

    #[test]
    fn with_system_actions_it_cant_read_nothing_is_written_that_would_hide_them() {
        let (_home, places) = setup();
        let system = places.system_config[0].join("Thunar/uca.xml");
        let odd = SYSTEM_UCA.replace("<action>", "<action id=\"1\">");
        write(&system, &odd).unwrap();

        let added = install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();

        assert!(added.to.is_empty());
        assert_eq!(added.not.len(), 1);
        assert!(added.not[0].contains("hide"), "{}", added.not[0]);
        assert!(!places.thunar().exists());
        assert_eq!(read(&system), odd);
    }

    #[test]
    fn a_uca_xml_clackatoo_cant_be_sure_of_is_left_as_it_is() {
        for odd in [
            USERS_UCA.replace("<actions>\n", "<actions>\n<!-- mine\n"),
            USERS_UCA.replace("<actions>\n", "<actions>\n<!ENTITY x \"y\">\n"),
            USERS_UCA.replace("<action>\n", "<action>\n<action>\n"),
            USERS_UCA.replace("<action>", "<action id=\"1\">"),
            USERS_UCA.replace("</actions>\n", ""),
            "<toolbar></toolbar>\n".to_owned(),
            format!("{USERS_UCA}<actions></actions>\n"),
            USERS_UCA.replace(
                "\t<icon>",
                &format!("\t<unique-id>{UCA_ID}</unique-id>\n</action>\n\t<icon>"),
            ),
        ] {
            let (_home, places) = setup();
            write(&places.thunar(), &odd).unwrap();

            let added = install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();

            assert!(added.to.is_empty(), "{odd}");
            assert_eq!(added.not.len(), 1, "{odd}");
            assert_eq!(read(&places.thunar()), odd);
            let (removed, left) = remove(&places, false).unwrap();
            assert!(removed.is_empty());
            assert_eq!(left.len(), 1, "{odd}");
            assert_eq!(read(&places.thunar()), odd);
        }
        // Not UTF-8: left alone too.
        let (_home, places) = setup();
        fs::create_dir_all(places.thunar().parent().unwrap()).unwrap();
        fs::write(places.thunar(), b"<actions>\xff</actions>\n").unwrap();
        let added = install(&places, PROGRAM, &[FileManager::Thunar]).unwrap();
        assert_eq!(added.not.len(), 1);
        assert_eq!(
            fs::read(places.thunar()).unwrap(),
            b"<actions>\xff</actions>\n"
        );
    }

    /// A path a file manager might give: spaces, quotes, a line break, a
    /// leading dash, a `%`, a `$` and a backslash, and bytes that aren't
    /// UTF-8.
    fn hostile_paths() -> Vec<Vec<u8>> {
        vec![
            b"/home/a b/it's \"x\" $HOME `id` %F\\n".to_vec(),
            b"/tmp/line\nbreak".to_vec(),
            b"-rf".to_vec(),
            b"/tmp/caf\xe9".to_vec(),
        ]
    }

    /// A program, at a path with every character a quote could go wrong
    /// on, that writes what it's given to `out`, each argument ending in
    /// a NUL. Written by a shell, not this process (see memory
    /// `tests-never-write-executables-in-process`).
    fn recorder(dir: &Path, out: &Path) -> PathBuf {
        let program = dir.join("my 'odd' \"dir\" $x %F \\ `x`/clackatoo");
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        let body = format!(
            "#!/bin/sh\nfor arg; do printf '%s\\0' \"$arg\"; done > '{}'\n",
            out.display()
        );
        let made = Command::new("sh")
            .args(["-c", "printf '%s' \"$1\" > \"$2\" && chmod +x \"$2\"", "sh"])
            .arg(&body)
            .arg(&program)
            .status()
            .unwrap();
        assert!(made.success());
        program
    }

    /// What the recorder at `out` was given.
    fn recorded(out: &Path) -> Vec<Vec<u8>> {
        let bytes = fs::read(out).unwrap();
        let mut args: Vec<Vec<u8>> = bytes.split(|&byte| byte == 0).map(<[u8]>::to_vec).collect();
        assert_eq!(args.pop(), Some(Vec::new()));
        args
    }

    /// Runs `command` as `sh -c` does.
    fn run_sh(command: &[u8]) {
        let status = Command::new("sh")
            .arg("-c")
            .arg(OsStr::from_bytes(command))
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// `text` as `GLib`'s `g_shell_quote` quotes it, as Thunar quotes each
    /// path, and as Nemo does with `Quote=single`.
    fn shell_quote(text: &[u8]) -> Vec<u8> {
        let mut quoted = vec![b'\''];
        for &byte in text {
            if byte == b'\'' {
                quoted.extend_from_slice(b"'\\''");
            } else {
                quoted.push(byte);
            }
        }
        quoted.push(b'\'');
        quoted
    }

    /// `command` with `code` replaced by `paths`, each quoted, then `%%`
    /// by `%`, as Thunar and Nemo expand it.
    fn expand(command: &str, code: &str, paths: &[Vec<u8>]) -> Vec<u8> {
        let (before, after) = command.rsplit_once(code).unwrap();
        let mut line = before.replace("%%", "%").into_bytes();
        let quoted: Vec<Vec<u8>> = paths.iter().map(|path| shell_quote(path)).collect();
        line.extend_from_slice(&quoted.join(&b' '));
        line.extend_from_slice(after.replace("%%", "%").as_bytes());
        line
    }

    /// The arguments `clackatoo open` is to be given.
    fn opened(paths: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut args = vec![b"open".to_vec(), b"--".to_vec()];
        args.extend(paths.iter().cloned());
        args
    }

    /// Each hook's command, run the way its file manager runs it, gives
    /// the program every path exactly, whatever is in it or in the
    /// program's own path. A shell stands in for `GLib`'s
    /// `g_shell_parse_argv` (Nemo), which reads quotes as one does, and
    /// for KIO's reading of an `Exec` line (Dolphin), which reads them as
    /// a shell reads double quotes.
    #[test]
    fn hooks_give_the_program_any_path_exactly_through_no_shell_that_could_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let program = recorder(dir.path(), &out);
        let program = program.to_str().unwrap();
        let paths = hostile_paths();

        // Thunar: the command in the file, unescaped, run by `sh -c`.
        let action = thunar_action(program);
        let command = action
            .split_once("<command>")
            .and_then(|(_, rest)| rest.split_once("</command>"))
            .unwrap()
            .0
            .replace("&apos;", "'")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        run_sh(&expand(&command, "%F", &paths));
        assert_eq!(recorded(&out), opened(&paths));

        // Nemo: the key file's value, unescaped, read as a shell would.
        for (which, code) in [(Nemo::Selected, "%F"), (Nemo::Here, "%P")] {
            let action = nemo(program, which);
            let exec = unescape_value(values(&action, "Exec")[0]);
            assert_eq!(values(&action, "Quote"), ["single"]);
            run_sh(&expand(&exec, code, &paths));
            assert_eq!(recorded(&out), opened(&paths));
        }

        // Dolphin: the program read as the desktop entry's, the paths
        // given as arguments of their own.
        let exec = unescape_value(values(&dolphin(program), "Exec")[0]);
        let exec = exec.strip_suffix(" open -- %F").unwrap().replace("%%", "%");
        let mut line = exec.into_bytes();
        line.extend_from_slice(b" open -- \"$@\"");
        let status = Command::new("sh")
            .arg("-c")
            .arg(OsStr::from_bytes(&line))
            .arg("sh")
            .args(paths.iter().map(|path| OsStr::from_bytes(path)))
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(recorded(&out), opened(&paths));
    }

    /// The extension is Python that reads, and names the program exactly,
    /// if Python is here to check.
    #[test]
    fn the_extension_for_files_is_python_naming_the_program_exactly() {
        let program = "/opt/a 'b' \"c\" \\d/clackatoo";
        let extension = nautilus(program);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clackatoo.py");
        fs::write(&file, &extension).unwrap();
        let checked = Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg(
                "import ast, sys\n\
                 tree = ast.parse(open(sys.argv[1]).read())\n\
                 for node in tree.body:\n\
                 \x20   if isinstance(node, ast.Assign) and node.targets[0].id == 'PROGRAM':\n\
                 \x20       sys.stdout.write(ast.literal_eval(node.value))\n",
            )
            .arg(&file)
            .output();
        let Ok(checked) = checked else {
            eprintln!("python3 isn't installed: not checked");
            return;
        };
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&checked.stdout), program);
        assert!(extension.starts_with(PYTHON_MANAGED));
        assert!(is_ours(&extension));
    }
}
