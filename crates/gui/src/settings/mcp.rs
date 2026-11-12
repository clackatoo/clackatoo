//! Managed server browser and drafts. Credentials remain masked while editing.
use super::{
    Mode, Outcome, SettingsMenu,
    view::{ControlView, Part, RowView},
};
use crate::{options::easy::Page, render::Ink, vim::Stroke};
use clackatoo_protocol::{McpAction, McpImport, McpReply, McpScope, McpServerInfo, McpState};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(1);
pub(super) struct Mcp {
    pub(super) id: u64,
    pub(super) entries: Vec<McpServerInfo>,
    pub(super) status: String,
    view: View,
    pending: Option<McpAction>,
    next_poll: Instant,
    removing: Option<(String, String)>,
    /// The servers signed in to from here, each with why it failed, if
    /// it did: their rows say how it went.
    sign_ins: BTreeMap<String, Option<String>>,
    /// The button the keys are on, of the row of buttons that's this
    /// row of the page's.
    button: (usize, usize),
    /// Answers to come to polls an action went ahead of, which are let
    /// go: the daemon answers in turn, and the next poll reads again.
    skip: usize,
}
impl std::fmt::Debug for Mcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mcp")
            .field("entries", &self.entries.len())
            .field("pending", &self.pending.is_some())
            .finish_non_exhaustive()
    }
}
enum View {
    List,
    Details(String),
    Edit(Editor),
    Secret(Editor),
    Callback(String, Editor),
    Import {
        provider: String,
        entries: Vec<McpImport>,
        selected: BTreeSet<String>,
    },
}
pub(super) struct Editor {
    fields: Vec<String>,
    editing: Option<Text>,
    original: Option<McpServerInfo>,
    scope: McpScope,
}
struct Text {
    field: usize,
    cursor: usize,
    before: String,
}

#[derive(Clone)]
enum Row {
    Add,
    Secret,
    Import(&'static str),
    Refresh,
    Server(String),
    Back,
    Status,
    Source,
    Definition,
    Enabled,
    ReadOnly,
    Edit,
    Test,
    Reconnect,
    Login,
    Callback,
    Forget,
    Trust,
    Remove,
    Tool(usize),
    Field(usize),
    Save,
    Cancel,
    ImportToggle(usize),
    ImportCommit,
}

const FIELDS: &[(&str, &str)] = &[
    (
        "Name",
        "Stable name, using letters, digits, underscores and hyphens.",
    ),
    (
        "Scope",
        "Personal applies across projects. Project writes .agents/mcp.json.",
    ),
    (
        "Transport",
        "A local command over stdio, or a Streamable HTTP endpoint.",
    ),
    (
        "Command or URL",
        "Command executable, without shell expansion; or HTTPS URL. Loopback HTTP is also supported.",
    ),
    (
        "Arguments",
        "JSON array of command arguments. Each value is passed literally.",
    ),
    (
        "Environment",
        "JSON object. Use {\"secret\":\"name\"} for private values, or {\"env\":\"VARIABLE\"} for inherited values.",
    ),
    (
        "HTTP headers",
        "JSON object. Store credentials privately, then refer to them with {\"secret\":\"name\"}.",
    ),
    (
        "Enabled",
        "Connect when an eligible chat needs this server.",
    ),
    (
        "Read-only server",
        "Your classification: read-only specialists may use this server. This does not sandbox the upstream process.",
    ),
    (
        "Request timeout (seconds)",
        "1–600 seconds. Timed-out tool calls are cancelled and never automatically replayed.",
    ),
    (
        "OAuth required",
        "Require a saved browser login before connecting. Public servers may leave this off and sign in later if needed.",
    ),
    (
        "OAuth client ID",
        "Optional registered public client. Blank uses server-supported client registration.",
    ),
    (
        "OAuth scopes",
        "Space-separated requested scopes. Blank uses the authorization server's defaults.",
    ),
];

impl Mcp {
    pub(super) fn new() -> Self {
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            entries: Vec::new(),
            status: "Reading MCP servers…".into(),
            view: View::List,
            pending: None,
            next_poll: Instant::now(),
            removing: None,
            sign_ins: BTreeMap::new(),
            button: (0, 0),
            skip: 0,
        }
    }
    fn rows(&self) -> Vec<Row> {
        match &self.view {
            View::List => [
                Row::Add,
                Row::Secret,
                Row::Import("claude"),
                Row::Import("codex"),
                Row::Refresh,
            ]
            .into_iter()
            .chain(self.entries.iter().map(|s| Row::Server(s.name.clone())))
            .collect(),
            View::Details(name) => {
                let mut rows = vec![
                    Row::Back,
                    Row::Status,
                    Row::Source,
                    Row::Definition,
                    Row::Enabled,
                    Row::ReadOnly,
                    Row::Edit,
                    Row::Test,
                    Row::Reconnect,
                    Row::Login,
                    Row::Callback,
                    Row::Forget,
                    Row::Trust,
                ];
                if let Some(server) = self.entries.iter().find(|s| &s.name == name) {
                    if server.editable {
                        rows.push(Row::Remove);
                    }
                    rows.extend((0..server.tools.len()).map(Row::Tool));
                }
                rows
            }
            View::Edit(_) => (0..FIELDS.len())
                .map(Row::Field)
                .chain([Row::Save, Row::Cancel])
                .collect(),
            View::Secret(_) => (0..2)
                .map(Row::Field)
                .chain([Row::Save, Row::Cancel])
                .collect(),
            View::Callback(..) => vec![Row::Field(0), Row::Save, Row::Cancel],
            View::Import { entries, .. } => [Row::Back]
                .into_iter()
                .chain((0..entries.len()).map(Row::ImportToggle))
                .chain([Row::ImportCommit])
                .collect(),
        }
    }
    pub(super) fn editor(&self) -> Option<&Editor> {
        match &self.view {
            View::Edit(e) | View::Secret(e) | View::Callback(_, e) => Some(e),
            _ => None,
        }
    }
    fn editor_mut(&mut self) -> Option<&mut Editor> {
        match &mut self.view {
            View::Edit(e) | View::Secret(e) | View::Callback(_, e) => Some(e),
            _ => None,
        }
    }
    fn selected(&self) -> Option<&McpServerInfo> {
        if let View::Details(name) = &self.view {
            self.entries.iter().find(|s| &s.name == name)
        } else {
            None
        }
    }
    fn secret_field(&self, field: usize) -> bool {
        matches!(self.view, View::Secret(_)) && field == 1
            || matches!(self.view, View::Callback(..))
    }
}

impl SettingsMenu {
    pub(crate) fn mcp_id(&self) -> u64 {
        self.mcp.id
    }
    pub(crate) fn mcp_loading(&mut self) {
        self.mcp.pending = Some(McpAction::List);
    }
    pub(crate) fn mcp_poll_due(&self, now: Instant) -> bool {
        self.mcp_wake().is_some_and(|due| due <= now)
    }
    pub(crate) fn mcp_wake(&self) -> Option<Instant> {
        (self.mcp_page() && self.mcp.pending.is_none()).then_some(self.mcp.next_poll)
    }
    pub(super) fn mcp_page(&self) -> bool {
        self.mode == Mode::Easy
            && self.easy.page() == Page::Mcp
            && !self.searching()
            && !self.typing
    }
    pub(super) fn mcp_browser(&self) -> bool {
        matches!(self.mcp.view, View::List)
    }
    pub(super) fn mcp_subtitle(&self) -> String {
        if self.mcp.editor().is_some() {
            "Changes stay in this draft until Save. Esc cancels a field edit.".into()
        } else {
            self.mcp.status.clone()
        }
    }
    pub(super) fn mcp_title(&self) -> String {
        match &self.mcp.view {
            View::List => "MCP Servers".into(),
            View::Details(name) => format!("MCP · {name}"),
            View::Edit(editor) => editor.original.as_ref().map_or_else(
                || "Add MCP server".into(),
                |s| format!("Edit MCP · {}", s.name),
            ),
            View::Secret(_) => "Private credential".into(),
            View::Callback(name, _) => format!("Sign in · {name}"),
            View::Import { provider, .. } => format!("Import MCP · {provider}"),
        }
    }
    pub(super) fn mcp_rows(&self) -> usize {
        self.mcp.rows().len()
    }
    pub(super) fn mcp_editing(&self) -> bool {
        self.mcp_page() && self.mcp.editor().is_some_and(|e| e.editing.is_some())
    }
    pub(super) fn finish_mcp_field(&mut self) {
        if let Some(editor) = self.mcp.editor_mut() {
            editor.editing = None;
        }
    }
    pub(crate) fn mcp_came(&mut self, id: u64, result: Result<McpReply, String>) -> Option<String> {
        if id != self.mcp.id {
            return None;
        }
        if self.mcp.skip > 0 {
            self.mcp.skip -= 1;
            return None;
        }
        let pending = self.mcp.pending.take();
        self.mcp.next_poll = Instant::now() + Duration::from_secs(2);
        // A sign in that never got as far as the browser failed.
        if let Some(McpAction::Authenticate { name }) = &pending {
            let failed = match &result {
                Err(why) => Some(why.clone()),
                Ok(reply) if reply.login_url.is_none() => Some(reply.notice.clone()),
                Ok(_) => None,
            };
            self.mcp.sign_ins.insert(name.clone(), failed);
        }
        match result {
            Err(why) => {
                self.mcp.status.clone_from(&why);
                self.said = Some((why, true));
                None
            }
            Ok(reply) => {
                let preserve = matches!(
                    pending,
                    Some(McpAction::Authenticate { .. } | McpAction::CompleteLogin { .. })
                );
                if !preserve {
                    self.mcp.entries = reply.servers;
                    self.mcp.entries.sort_by(|a, b| a.name.cmp(&b.name));
                }
                if let Some(McpAction::Import { provider, names }) = &pending
                    && names.is_empty()
                {
                    self.mcp.view = View::Import {
                        provider: provider.clone(),
                        entries: reply.imports,
                        selected: BTreeSet::new(),
                    };
                    self.specialist_focus(0, None);
                }
                if let Some(McpAction::Save { name, .. }) = pending {
                    self.mcp.view = View::Details(name);
                    self.specialist_focus(0, None);
                } else if matches!(
                    pending,
                    Some(
                        McpAction::Secret { .. }
                            | McpAction::Remove { .. }
                            | McpAction::CompleteLogin { .. }
                    )
                ) || matches!(pending,Some(McpAction::Import{ref names,..}) if !names.is_empty())
                {
                    self.mcp.view = View::List;
                    self.specialist_focus(0, None);
                }
                if let View::Details(name) = &self.mcp.view
                    && !self.mcp.entries.iter().any(|s| &s.name == name)
                {
                    self.mcp.view = View::List;
                    self.specialist_focus(0, None);
                }
                self.mcp.status = if reply.notice.is_empty() {
                    format!(
                        "{} managed servers. Connections belong to their chat and workspace.",
                        self.mcp.entries.len()
                    )
                } else {
                    reply.notice.clone()
                };
                if !reply.notice.is_empty() {
                    self.said_saved = Some((reply.notice, false));
                }
                reply.login_url
            }
        }
    }
    fn mcp_request(&mut self, action: McpAction) -> Outcome {
        // An action goes ahead of a poll, whose answer is let go, rather
        // than a click or Enter doing nothing while it's read.
        match &self.mcp.pending {
            Some(McpAction::List) if !matches!(action, McpAction::List) => self.mcp.skip += 1,
            Some(_) => return Outcome::Nothing,
            None => {}
        }
        if let McpAction::Authenticate { name } = &action {
            self.mcp.sign_ins.insert(name.clone(), None);
        }
        self.mcp.pending = Some(action.clone());
        self.said = None;
        Outcome::Mcp {
            action,
            id: self.mcp.id,
        }
    }
    pub(super) fn mcp_row(&self, at: usize) -> RowView {
        let row = |label: String, help: String, control| RowView {
            label,
            help,
            control,
            notes: Vec::new(),
            lock: None,
        };
        let button = |label: &str, help: &str, action: &'static str| {
            row(label.into(), help.into(), ControlView::Button(action))
        };
        let pending = self.mcp.pending.is_some();
        match self.mcp.rows()[at].clone() {
            Row::Add=>button("Add MCP server","Connect a local stdio process or a remote HTTP service.","Add"),
            Row::Secret=>button("Private credentials","Store a credential separately from project configuration.","Store credential"),
            Row::Import(provider)=>button(if provider=="claude"{"Import from Claude Code"}else{"Import from Codex"},"Review servers before importing. Existing provider files are never changed.","Preview import"),
            Row::Refresh=>button("Refresh","Read changed configuration and current connection status.",if pending{"Working…"}else{"Refresh"}),
            Row::Server(name)=>{
                let server=self.mcp.entries.iter().find(|s|s.name==name).expect("listed server");
                let control=match self.sign_in_button(server){
                    Some(sign_in)=>ControlView::Buttons{buttons:vec![sign_in,("Manage",true)],at:self.mcp_button(at)},
                    None=>ControlView::Button("Manage"),
                };
                let mut view=row(server.name.clone(),format!("{} · {} · {} tools",state_name(server.state),server.source,server.tools.len()),control);
                if let Some(failed)=self.sign_in_failed(server){view.notes.push((failed,Ink::Warning));}
                else if !server.detail.is_empty(){view.notes.push((server.detail.clone(),if server.state==McpState::Error{Ink::Warning}else{Ink::Muted}));}view
            },
            Row::Back=>button("Back to servers","Return to the server browser.","Back"),
            Row::Status=>{let server=self.mcp.selected().expect("server detail");row("Connection".into(),server.detail.clone(),ControlView::Field{text:state_name(server.state).into(),editing:false})},
            Row::Source=>{let server=self.mcp.selected().expect("server detail");row("Configuration source".into(),server.path.clone(),ControlView::Field{text:server.source.clone(),editing:false})},
            Row::Definition=>{let server=self.mcp.selected().expect("server detail");row("Server definition".into(),"Review this exact transport and its arguments before approving it.".into(),ControlView::Paragraph{text:server.settings.clone(),editing:false})},
            Row::Enabled=>row("Enabled".into(),"Bundled entries get a personal override when changed here.".into(),ControlView::Toggle(self.mcp.selected().is_some_and(|s|s.enabled))),
            Row::ReadOnly=>row("Read-only server".into(),"Your classification allows read-only specialists to use this server; it is not an OS sandbox.".into(),ControlView::Toggle(self.mcp.selected().is_some_and(|s|serde_json::from_str::<Value>(&s.settings).ok().and_then(|v|v.get("read_only").and_then(Value::as_bool))==Some(true)))),
            Row::Edit=>button("Server configuration","Edit transport, arguments, credential references and timeouts. Bundled definitions are copied to personal configuration.","Edit"),
            Row::Test=>button("Test connection","Initialize the server and inspect its tools without running any tools.",if pending{"Working…"}else{"Test"}),
            Row::Reconnect=>button("Reconnect","Reconnect active proxies. Interrupted calls are never replayed. Reopen a chat if its provider already disconnected the proxy.","Reconnect"),
            Row::Login=>button("Browser sign in","Use OAuth with this server. Command servers take credentials through their environment.","Sign in"),
            Row::Callback=>button("Complete a remote sign in","If the daemon is on another machine, paste the full callback URL from your browser's address bar.","Paste callback URL"),
            Row::Forget=>button("Forget browser login","Remove this server's saved OAuth credentials and cancel a pending login.","Forget login"),
            Row::Trust=>{let s=self.mcp.selected().expect("server detail");button("Approve this definition","Review its configuration before allowing a project or packaged command to start.",if s.trusted{"Approved"}else{"Approve"})},
            Row::Remove=>button("Remove configuration","Remove this source's entry. A lower-priority definition may become visible.",if self.mcp.removing.is_some(){"Confirm remove"}else{"Remove"}),
            Row::Tool(index)=>{let tool=&self.mcp.selected().expect("server detail").tools[index];row(tool.name.clone(),tool.description.clone(),ControlView::Field{text:"Tool".into(),editing:false})},
            Row::Field(field)=>{
                let editor=self.mcp.editor().expect("editor");
                if matches!(self.mcp.view,View::Edit(_)) {
                    let (label,help)=FIELDS[field];
                    let control=match field {
                        1=>ControlView::Segmented{labels:vec!["Personal","Project"],picked:Some(usize::from(editor.scope==McpScope::Project))},
                        2=>ControlView::Segmented{labels:vec!["Command","HTTP"],picked:Some(usize::from(editor.fields[2]=="http"))},
                        7|8|10=>ControlView::Toggle(editor.fields[field]=="true"),
                        4..=6=>ControlView::Paragraph{text:display(editor,field,false),editing:editor.editing.as_ref().is_some_and(|e|e.field==field)},
                        _=>ControlView::Field{text:display(editor,field,false),editing:false},
                    };row(label.into(),help.into(),control)
                }else{
                    let (label,help)=if matches!(self.mcp.view,View::Callback(..)){("Callback URL","Paste the complete redirect URL. It is validated against this login attempt and never logged.")}
                        else if field==0{("Credential name","Reference it in env or headers with {\"secret\":\"name\"}.")}
                        else{("Credential value","Masked and saved with owner-only permissions. For an Authorization header, include the Bearer prefix.")};
                    row(label.into(),help.into(),ControlView::Field{text:display(editor,field,self.mcp.secret_field(field)),editing:false})
                }
            },
            Row::Save=>button("Save","Validate and save this draft.",if pending{"Saving…"}else{"Save"}),
            Row::Cancel=>button("Cancel","Discard this draft.","Cancel"),
            Row::ImportToggle(index)=>{
                let View::Import{entries,selected,..}=&self.mcp.view else{unreachable!()};let entry=&entries[index];
                row(entry.name.clone(),entry.description.clone(),ControlView::Toggle(selected.contains(&entry.name)))
            },
            Row::ImportCommit=>button("Import selected servers","Store transport credentials privately and create personal definitions. Existing server names are never overwritten.",if pending{"Importing…"}else{"Import selected"}),
        }
    }
    pub(super) fn mcp_act(&mut self, at: usize) -> Outcome {
        self.mcp_act_part(at, None)
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per server browser, draft or import control"
    )]
    pub(super) fn mcp_act_part(&mut self, at: usize, part: Option<Part>) -> Outcome {
        let Some(row) = self.mcp.rows().get(at).cloned() else {
            return Outcome::Nothing;
        };
        // While an action's asked, only going back or elsewhere; a poll
        // stops nothing.
        if (self.mcp.pending.as_ref()).is_some_and(|p| !matches!(p, McpAction::List))
            && !matches!(row, Row::Back | Row::Cancel | Row::Server(_))
        {
            return Outcome::Nothing;
        }
        match row {
            Row::Back | Row::Cancel => {
                self.mcp.view = View::List;
                self.mcp.removing = None;
            }
            Row::Add => self.mcp.view = View::Edit(Editor::new(None)),
            Row::Secret => self.mcp.view = View::Secret(Editor::plain(2)),
            Row::Import(provider) => {
                return self.mcp_request(McpAction::Import {
                    provider: provider.into(),
                    names: Vec::new(),
                });
            }
            Row::Refresh => return self.mcp_request(McpAction::List),
            Row::Server(name) => {
                // Its Sign in button, if it has one, is the first.
                let sign_in = (self.mcp.entries.iter())
                    .find(|s| s.name == name)
                    .and_then(|server| self.sign_in_button(server));
                match (sign_in, part) {
                    (Some((_, true)), Some(Part::Segment(0))) => {
                        return self.mcp_request(McpAction::Authenticate { name });
                    }
                    (Some(_), Some(Part::Segment(0))) => return Outcome::Nothing,
                    _ => {}
                }
                self.mcp.view = View::Details(name);
                self.mcp.removing = None;
            }
            Row::Edit => self.mcp.view = View::Edit(Editor::new(self.mcp.selected().cloned())),
            Row::Enabled | Row::ReadOnly => {
                let Some(server) = self.mcp.selected().cloned() else {
                    return Outcome::Nothing;
                };
                let mut settings: Value =
                    serde_json::from_str(&server.settings).unwrap_or_default();
                let key = if matches!(row, Row::Enabled) {
                    "enabled"
                } else {
                    "read_only"
                };
                settings[key] = json!(!settings.get(key).and_then(Value::as_bool).unwrap_or(false));
                return self.mcp_request(McpAction::Save {
                    name: server.name,
                    settings: settings.to_string(),
                    scope: if server.editable {
                        server.scope
                    } else {
                        McpScope::Personal
                    },
                    expected: Some(server.revision),
                });
            }
            Row::Test
            | Row::Reconnect
            | Row::Login
            | Row::Forget
            | Row::Trust
            | Row::Remove
            | Row::Callback => {
                let Some(server) = self.mcp.selected().cloned() else {
                    return Outcome::Nothing;
                };
                let action = match row {
                    Row::Test => McpAction::Test { name: server.name },
                    Row::Reconnect => McpAction::Reconnect { name: server.name },
                    Row::Login => McpAction::Authenticate { name: server.name },
                    Row::Forget => McpAction::ForgetAuth { name: server.name },
                    Row::Trust => {
                        if server.trusted {
                            return Outcome::Nothing;
                        }
                        McpAction::Trust {
                            name: server.name,
                            expected: server.revision,
                        }
                    }
                    Row::Remove => {
                        if self.mcp.removing.as_ref()
                            != Some(&(server.name.clone(), server.revision.clone()))
                        {
                            self.mcp.removing = Some((server.name, server.revision));
                            self.said=Some(("Remove this definition? Press Confirm remove, or go Back to cancel.".into(),true));
                            return Outcome::Changed;
                        }
                        self.mcp.removing = None;
                        McpAction::Remove {
                            name: server.name,
                            expected: server.revision,
                        }
                    }
                    Row::Callback => {
                        self.mcp.view = View::Callback(server.name, Editor::plain(1));
                        self.specialist_focus(0, None);
                        return Outcome::Changed;
                    }
                    _ => unreachable!(),
                };
                return self.mcp_request(action);
            }
            Row::Field(field) => {
                let edit = matches!(self.mcp.view, View::Edit(_));
                let editor = self.mcp.editor_mut().expect("editor");
                if edit {
                    match field {
                        0 if editor.original.as_ref().is_some_and(|s| s.editable) => {
                            return Outcome::Nothing;
                        }
                        1 => {
                            if editor.original.as_ref().is_some_and(|s| s.editable) {
                                return Outcome::Nothing;
                            }
                            editor.scope = if matches!(part, Some(Part::Segment(1)))
                                || part.is_none() && editor.scope == McpScope::Personal
                            {
                                McpScope::Project
                            } else {
                                McpScope::Personal
                            };
                            return Outcome::Changed;
                        }
                        2 => {
                            editor.fields[2] = if matches!(part, Some(Part::Segment(1)))
                                || part.is_none() && editor.fields[2] == "stdio"
                            {
                                "http"
                            } else {
                                "stdio"
                            }
                            .into();
                            return Outcome::Changed;
                        }
                        7 | 8 | 10 => {
                            editor.fields[field] = (editor.fields[field] != "true").to_string();
                            return Outcome::Changed;
                        }
                        _ => {}
                    }
                }
                editor.editing = Some(Text {
                    field,
                    cursor: editor.fields[field].len(),
                    before: editor.fields[field].clone(),
                });
                return Outcome::Changed;
            }
            Row::Save => {
                let action = match &self.mcp.view {
                    View::Edit(editor) => editor.action(),
                    View::Secret(editor) => Ok(McpAction::Secret {
                        name: editor.fields[0].trim().into(),
                        value: editor.fields[1].clone(),
                    }),
                    View::Callback(name, editor) => Ok(McpAction::CompleteLogin {
                        name: name.clone(),
                        url: editor.fields[0].trim().into(),
                    }),
                    _ => return Outcome::Nothing,
                };
                return match action {
                    Ok(action) => self.mcp_request(action),
                    Err(why) => {
                        self.said = Some((why, true));
                        Outcome::Changed
                    }
                };
            }
            Row::ImportToggle(index) => {
                let View::Import {
                    entries, selected, ..
                } = &mut self.mcp.view
                else {
                    return Outcome::Nothing;
                };
                let entry = &entries[index];
                if entry.description.starts_with("Cannot import") {
                    self.said = Some((entry.description.clone(), true));
                    return Outcome::Changed;
                }
                if !selected.remove(&entry.name) {
                    selected.insert(entry.name.clone());
                }
                return Outcome::Changed;
            }
            Row::ImportCommit => {
                let View::Import {
                    provider, selected, ..
                } = &self.mcp.view
                else {
                    return Outcome::Nothing;
                };
                if selected.is_empty() {
                    self.said = Some(("Select at least one server to import.".into(), true));
                    return Outcome::Changed;
                }
                return self.mcp_request(McpAction::Import {
                    provider: provider.clone(),
                    names: selected.iter().cloned().collect(),
                });
            }
            Row::Status | Row::Source | Row::Definition | Row::Tool(_) => return Outcome::Nothing,
        }
        self.specialist_focus(0, None);
        Outcome::Changed
    }
    /// What server `server`'s Sign in button says, and whether it acts:
    /// none unless it needs a sign in, or one from here is going or done.
    fn sign_in_button(&self, server: &McpServerInfo) -> Option<(&'static str, bool)> {
        let asked = matches!(
            &self.mcp.pending,
            Some(McpAction::Authenticate { name }) if *name == server.name
        );
        if server.signing_in || asked {
            Some(("Signing in…", false))
        } else if server.state == McpState::NeedsLogin {
            Some(("Sign in", true))
        } else if self.mcp.sign_ins.get(&server.name) == Some(&None)
            && matches!(server.state, McpState::Ready | McpState::Connected)
        {
            Some(("Signed in", false))
        } else {
            None
        }
    }

    /// Why signing in to `server` from here failed, if it did and it
    /// still needs a sign in.
    fn sign_in_failed(&self, server: &McpServerInfo) -> Option<String> {
        if server.signing_in || server.state != McpState::NeedsLogin {
            return None;
        }
        match self.mcp.sign_ins.get(&server.name)? {
            Some(why) if !why.is_empty() => Some(why.clone()),
            // Failed in the browser, which the daemon says.
            _ => (!server.detail.is_empty()).then(|| server.detail.clone()),
        }
    }

    /// The button the keys are on in row `at`, of a row of buttons.
    fn mcp_button(&self, at: usize) -> usize {
        let (row, button) = self.mcp.button;
        if row == at { button } else { 0 }
    }

    /// Enter on row `at`: the button the keys are on, of a row of them.
    pub(super) fn mcp_enter(&mut self, at: usize) -> Outcome {
        let buttons = matches!(self.mcp_row(at).control, ControlView::Buttons { .. });
        let part = buttons.then(|| Part::Segment(u8::try_from(self.mcp_button(at)).unwrap_or(0)));
        self.mcp_act_part(at, part)
    }

    /// `l` or `h` on row `at`: the keys go to the next of its buttons,
    /// or the one before.
    pub(super) fn mcp_step(&mut self, at: usize, by: isize) -> Outcome {
        let ControlView::Buttons { buttons, .. } = self.mcp_row(at).control else {
            return Outcome::Nothing;
        };
        let button = (self.mcp_button(at).saturating_add_signed(by)).min(buttons.len() - 1);
        if button == self.mcp_button(at) {
            return Outcome::Nothing;
        }
        self.mcp.button = (at, button);
        Outcome::Changed
    }

    pub(super) fn mcp_paste(&mut self, text: &str) -> Outcome {
        if self
            .mcp
            .pending
            .as_ref()
            .is_some_and(|p| !matches!(p, McpAction::List))
        {
            return Outcome::Nothing;
        }
        let multi = matches!(self.mcp.view, View::Edit(_));
        let Some(editor) = self.mcp.editor_mut() else {
            return Outcome::Nothing;
        };
        let Some(edit) = &mut editor.editing else {
            return Outcome::Nothing;
        };
        let text = if multi && (4..=6).contains(&edit.field) {
            text.replace('\r', "")
        } else {
            text.replace(['\r', '\n'], " ")
        };
        if editor.fields[edit.field].len() + text.len() > 64 * 1024 {
            return Outcome::Nothing;
        }
        editor.fields[edit.field].insert_str(edit.cursor, &text);
        edit.cursor += text.len();
        Outcome::Changed
    }
    pub(super) fn mcp_key(&mut self, stroke: Stroke) -> Outcome {
        if self
            .mcp
            .pending
            .as_ref()
            .is_some_and(|p| !matches!(p, McpAction::List))
        {
            return Outcome::Nothing;
        }
        let multi = matches!(self.mcp.view, View::Edit(_));
        let Some(editor) = self.mcp.editor_mut() else {
            return Outcome::Nothing;
        };
        let Some(edit) = &mut editor.editing else {
            return Outcome::Nothing;
        };
        let text = &mut editor.fields[edit.field];
        let prev = |s: &str, c: usize| s[..c].char_indices().next_back().map_or(0, |(i, _)| i);
        let next = |s: &str, c: usize| s[c..].chars().next().map_or(c, |ch| c + ch.len_utf8());
        match stroke {
            Stroke::Escape => {
                text.clone_from(&edit.before);
                editor.editing = None;
            }
            Stroke::Enter | Stroke::Tab => editor.editing = None,
            Stroke::Left => edit.cursor = prev(text, edit.cursor),
            Stroke::Right => edit.cursor = next(text, edit.cursor),
            Stroke::Home => edit.cursor = text[..edit.cursor].rfind('\n').map_or(0, |i| i + 1),
            Stroke::End => {
                edit.cursor = text[edit.cursor..]
                    .find('\n')
                    .map_or(text.len(), |i| edit.cursor + i);
            }
            Stroke::Up | Stroke::Down => {
                edit.cursor =
                    super::specialists::vertical_cursor(text, edit.cursor, stroke == Stroke::Up);
            }
            Stroke::Backspace => {
                let from = prev(text, edit.cursor);
                text.drain(from..edit.cursor);
                edit.cursor = from;
            }
            Stroke::Delete => {
                let to = next(text, edit.cursor);
                text.drain(edit.cursor..to);
            }
            Stroke::Char {
                ch: 'a',
                ctrl: true,
            } => edit.cursor = 0,
            Stroke::Char {
                ch: 'e',
                ctrl: true,
            } => edit.cursor = text.len(),
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => {
                text.clear();
                edit.cursor = 0;
            }
            Stroke::Char {
                ch: 'j',
                ctrl: true,
            } if multi && (4..=6).contains(&edit.field) => return self.mcp_paste("\n"),
            Stroke::Char { ch, ctrl: false } => return self.mcp_paste(&ch.to_string()),
            _ => {}
        }
        Outcome::Changed
    }
}

impl Editor {
    fn plain(count: usize) -> Self {
        Self {
            fields: vec![String::new(); count],
            editing: None,
            original: None,
            scope: McpScope::Personal,
        }
    }
    fn new(original: Option<McpServerInfo>) -> Self {
        let config = original
            .as_ref()
            .and_then(|s| serde_json::from_str::<Value>(&s.settings).ok())
            .unwrap_or(Value::Null);
        let http = config.get("url").and_then(Value::as_str).is_some();
        let serialize = |key: &str, default: Value| {
            serde_json::to_string_pretty(config.get(key).unwrap_or(&default))
                .expect("JSON serializes")
        };
        let fields = vec![
            original
                .as_ref()
                .map_or_else(String::new, |s| s.name.clone()),
            String::new(),
            if http { "http" } else { "stdio" }.into(),
            config
                .get(if http { "url" } else { "command" })
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            serialize("args", json!([])),
            serialize("env", json!({})),
            serialize("headers", json!({})),
            config
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
                .to_string(),
            config
                .get("read_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                .to_string(),
            config
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .to_string(),
            (!config.get("oauth").unwrap_or(&Value::Null).is_null()).to_string(),
            config
                .pointer("/oauth/client_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            config
                .pointer("/oauth/scopes")
                .and_then(Value::as_array)
                .map_or_else(String::new, |scopes| {
                    scopes
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                }),
        ];
        let scope = original
            .as_ref()
            .filter(|s| s.editable)
            .map_or(McpScope::Personal, |s| s.scope);
        Self {
            fields,
            editing: None,
            original,
            scope,
        }
    }
    fn action(&self) -> Result<McpAction, String> {
        let f = &self.fields;
        let name = f[0].trim();
        if name.is_empty() {
            return Err("Give the server a name.".into());
        }
        let timeout = f[9]
            .parse::<u32>()
            .map_err(|_| "Request timeout must be a number")?;
        let mut config =
            json!({"enabled":f[7]=="true","read_only":f[8]=="true","timeout_seconds":timeout});
        if f[2] == "http" {
            config["url"] = json!(f[3].trim());
            config["headers"] =
                serde_json::from_str(&f[6]).map_err(|_| "HTTP headers must be a JSON object")?;
            if f[10] == "true" {
                config["oauth"] = json!({"client_id":if f[11].trim().is_empty(){None}else{Some(f[11].trim())},"scopes":f[12].split_whitespace().collect::<Vec<_>>()});
            }
        } else {
            config["command"] = json!(f[3].trim());
            config["args"] =
                serde_json::from_str(&f[4]).map_err(|_| "Arguments must be a JSON array")?;
            config["env"] =
                serde_json::from_str(&f[5]).map_err(|_| "Environment must be a JSON object")?;
        }
        Ok(McpAction::Save {
            name: name.into(),
            settings: config.to_string(),
            scope: self.scope,
            expected: self
                .original
                .as_ref()
                .filter(|s| s.name == name)
                .map(|s| s.revision.clone()),
        })
    }
}
fn display(editor: &Editor, field: usize, secret: bool) -> String {
    if secret {
        return if editor.fields[field].is_empty() {
            String::new()
        } else {
            "•".repeat(editor.fields[field].chars().count().min(24))
        };
    }
    let mut text = editor.fields[field].clone();
    if let Some(edit) = &editor.editing
        && edit.field == field
    {
        text.insert(edit.cursor, '│');
    }
    text
}
fn state_name(state: McpState) -> &'static str {
    match state {
        McpState::Disabled => "Disabled",
        McpState::Disconnected => "Disconnected",
        McpState::Starting => "Starting",
        McpState::Connected => "Connected",
        McpState::NeedsLogin => "Needs sign in",
        McpState::Error => "Error",
        McpState::Ready => "Ready",
    }
}

#[cfg(test)]
mod tests;
