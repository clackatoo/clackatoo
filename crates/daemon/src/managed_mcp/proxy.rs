//! A per-server MCP proxy. Payloads retain their upstream identities;
//! authorization is checked through a separate local daemon connection.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clackatoo_protocol::{
    ClientMessage, McpProxyAction, McpState, PROTOCOL_VERSION, Request, Response, ServerMessage,
    TerminalId,
    frame::{self, FrameDecoder},
};
use rmcp::transport::Transport as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{
    config::Bound,
    credentials,
    transport::{self, Upstream},
};

const MAX_MESSAGE: usize = 4 << 20;
const MAX_CALLS: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Grant {
    pub(crate) bound: Bound,
    pub(crate) generation: u64,
}
impl Grant {
    pub(crate) fn revision(&self) -> String {
        format!("{}:{}", self.bound.grant(), self.generation)
    }
}

#[derive(Clone)]
struct Control {
    socket: PathBuf,
    chat: TerminalId,
    server: String,
    token: String,
}
impl Control {
    async fn request(&self, action: McpProxyAction) -> Result<Response, String> {
        let mut stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .map_err(|_| "The daemon is unavailable")?;
        if stream
            .peer_cred()
            .map_err(|_| "Cannot verify the daemon's identity")?
            .uid()
            != rustix::process::getuid().as_raw()
        {
            return Err("The daemon socket belongs to another user".into());
        }
        stream
            .write_all(
                &frame::encode(
                    PROTOCOL_VERSION,
                    &ClientMessage::Hello {
                        protocol_version: PROTOCOL_VERSION,
                    },
                )
                .map_err(|_| "Cannot encode daemon handshake")?,
            )
            .await
            .map_err(|_| "Cannot greet the daemon")?;
        let mut decoder = FrameDecoder::new();
        let hello = receive(&mut stream, &mut decoder, PROTOCOL_VERSION).await?;
        let ServerMessage::Hello {
            protocol_version: wire,
            ..
        } = hello
        else {
            return Err("The daemon refused this MCP proxy".into());
        };
        let wire = wire.min(PROTOCOL_VERSION);
        if wire < clackatoo_protocol::wire::MANAGED_MCP {
            return Err("This daemon needs upgrading to manage MCP servers".into());
        }
        let request = ClientMessage::Request(Request::McpProxy {
            chat: self.chat,
            server: self.server.clone(),
            token: self.token.clone(),
            action,
        });
        stream
            .write_all(
                &frame::encode(wire, &request).map_err(|_| "Cannot encode MCP authorization")?,
            )
            .await
            .map_err(|_| "Cannot ask the daemon about MCP access")?;
        loop {
            match receive(&mut stream, &mut decoder, wire).await? {
                ServerMessage::Response(Response::Error(error)) => return Err(error.to_string()),
                ServerMessage::Response(response) => return Ok(response),
                ServerMessage::Event(_) => {}
                _ => return Err("Unexpected daemon response".into()),
            }
        }
    }
    async fn check(&self, revision: &str, value: &Value) -> Result<Response, String> {
        let method = value
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let request = if method == "tools/call" || (!method.is_empty() && !metadata(method)) {
            let text = value
                .get("params")
                .map_or_else(String::new, Value::to_string);
            if text.len() > 64 * 1024 {
                return Err("MCP request exceeds the 64 KiB approval display limit".into());
            }
            text
        } else {
            String::new()
        };
        self.request(McpProxyAction::Check {
            revision: revision.into(),
            method: method.into(),
            request,
        })
        .await
    }
    async fn heartbeat(&self, revision: &str, initialized: bool) -> Result<Response, String> {
        loop {
            match self
                .request(McpProxyAction::Heartbeat {
                    revision: revision.into(),
                    state: if initialized {
                        McpState::Connected
                    } else {
                        McpState::Starting
                    },
                })
                .await
            {
                Err(why) if why.contains("MCP catalogue is loading") => return Ok(Response::Done),
                Err(why)
                    if matches!(
                        why.as_str(),
                        "The daemon is unavailable"
                            | "The daemon connection closed"
                            | "Cannot greet the daemon"
                            | "Cannot ask the daemon about MCP access"
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                result => return result,
            }
        }
    }
    async fn report(
        &self,
        revision: &str,
        state: McpState,
        detail: String,
        tools: Vec<clackatoo_protocol::McpToolInfo>,
    ) {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            self.request(McpProxyAction::Report {
                revision: revision.into(),
                state,
                detail,
                tools,
            }),
        )
        .await;
    }
}

pub(crate) fn metadata(method: &str) -> bool {
    method.is_empty()
        || method == "initialize"
        || method == "ping"
        || method.starts_with("notifications/")
        || [
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "prompts/list",
            "logging/setLevel",
        ]
        .contains(&method)
}

async fn receive(
    stream: &mut tokio::net::UnixStream,
    decoder: &mut FrameDecoder,
    wire: u32,
) -> Result<ServerMessage, String> {
    let mut bytes = vec![0; 16384];
    loop {
        if let Some(message) = decoder
            .decode(wire)
            .map_err(|_| "Cannot decode daemon response")?
        {
            return Ok(message);
        }
        let n = stream
            .read(&mut bytes)
            .await
            .map_err(|_| "The daemon connection closed")?;
        if n == 0 {
            return Err("The daemon connection closed".into());
        }
        decoder.push(&bytes[..n]);
    }
}

/// Run an authenticated managed server proxy over the agent's stdio.
///
/// # Errors
/// If authorization is refused, the upstream fails, or the agent disconnects.
pub fn relay(
    socket: &Path,
    chat: TerminalId,
    server: &str,
    token: &str,
    input: std::fs::File,
    output: std::fs::File,
) -> Result<(), std::io::Error> {
    let control = Control {
        socket: socket.into(),
        chat,
        server: server.into(),
        token: token.into(),
    };
    let runtime = transport::runtime().map_err(std::io::Error::other)?;
    runtime
        .block_on(run(control, input, output))
        .map_err(std::io::Error::other)
}

fn input_thread(mut input: std::fs::File) -> tokio::sync::mpsc::Receiver<Result<Value, String>> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0; 16384];
        'read: loop {
            let n = match input.read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            for &byte in &chunk[..n] {
                if byte == b'\n' {
                    if !buffer.is_empty() {
                        let value = serde_json::from_slice(&buffer)
                            .map_err(|_| "Invalid MCP JSON message".into());
                        if tx.blocking_send(value).is_err() {
                            break 'read;
                        }
                        buffer.clear();
                    }
                } else {
                    buffer.push(byte);
                    if buffer.len() > MAX_MESSAGE {
                        let _ = tx.blocking_send(Err("MCP message exceeds 4 MiB".into()));
                        break 'read;
                    }
                }
            }
        }
        if !buffer.is_empty() && buffer.len() <= MAX_MESSAGE {
            let _ = tx.blocking_send(
                serde_json::from_slice(&buffer).map_err(|_| "Invalid final MCP message".into()),
            );
        }
    });
    rx
}

fn output_thread(mut output: std::fs::File) -> tokio::sync::mpsc::Sender<Value> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(8);
    std::thread::spawn(move || {
        while let Some(value) = rx.blocking_recv() {
            let Ok(mut bytes) = serde_json::to_vec(&value) else {
                break;
            };
            if bytes.len() > MAX_MESSAGE {
                break;
            }
            bytes.push(b'\n');
            if output
                .write_all(&bytes)
                .and_then(|()| output.flush())
                .is_err()
            {
                break;
            }
        }
    });
    tx
}

async fn answer(output: &tokio::sync::mpsc::Sender<Value>, id: &Value, message: &str) {
    let _ = output
        .send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}}))
        .await;
}

#[allow(
    clippy::too_many_lines,
    reason = "one select arm per proxy event, sharing session state"
)]
async fn run(control: Control, input: std::fs::File, output: std::fs::File) -> Result<(), String> {
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        control.request(McpProxyAction::Open),
    )
    .await
    .map_err(|_| "MCP authorization timed out")??;
    let Response::McpConnection(configuration) = response else {
        return Err("The daemon did not grant this MCP connection".into());
    };
    let mut grant: Grant =
        serde_json::from_str(&configuration).map_err(|_| "Invalid MCP connection grant")?;
    let mut upstream = match Upstream::connect(&grant.bound).await {
        Ok(upstream) => upstream,
        Err(why) => {
            control
                .report(
                    &grant.revision(),
                    failure_state(&why),
                    credentials::redacted(&grant.bound, &why),
                    Vec::new(),
                )
                .await;
            return Err(why);
        }
    };
    let mut input = input_thread(input);
    let output = output_thread(output);
    let mut checks = tokio::task::JoinSet::new();
    let mut sends = tokio::task::JoinSet::new();
    let mut pending = BTreeMap::<String, (Value, tokio::time::Instant)>::new();
    let mut checking = BTreeMap::<String, (Value, tokio::task::AbortHandle)>::new();
    let mut initialization = None;
    let mut initialized = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let result = loop {
        tokio::select! {
            line=input.recv()=>{
                let Some(line)=line else{break Ok(());};let value=match line{Ok(value)=>value,Err(e)=>break Err(e)};
                if value.get("method").and_then(Value::as_str)==Some("initialize") {initialization=Some(value.clone());}
                if value.get("method").and_then(Value::as_str)==Some("notifications/cancelled")
                    && let Some(id)=value.pointer("/params/requestId") {
                    if let Some((_, task))=checking.remove(&id.to_string()) {task.abort();}
                    pending.remove(&id.to_string());
                }
                let method=value.get("method").and_then(Value::as_str).unwrap_or_default();
                if metadata(method) {
                    if !method.is_empty() && value.get("id").is_some() && pending.len()>=MAX_CALLS {
                        answer(&output,&value["id"],"Too many MCP requests are pending").await;continue;
                    }
                    // Handshake, notifications and replies must retain input
                    // order; they never wait behind an interactive approval.
                    match tokio::time::timeout(Duration::from_secs(5),control.check(&grant.revision(), &value)).await {
                        Ok(Ok(Response::Done))=>{},
                        Ok(Err(why))=>break Err(why),
                        _=>{if let Some(id)=value.get("id"){answer(&output,id,"The MCP connection changed; retry after reconnecting.").await;}continue;}
                    }
                    let message=serde_json::from_value(value.clone()).map_err(|_|"Invalid MCP message")?;
                    if let Some(id)=value.get("id") && !method.is_empty() {
                        pending.insert(id.to_string(),(id.clone(),tokio::time::Instant::now()+Duration::from_secs(30)));
                    }
                    tokio::time::timeout(Duration::from_secs(5),upstream.send(message)).await.map_err(|_|"MCP transport stalled")?.map_err(|e|e.to_string())?;
                    continue;
                }
                if let Some(id)=value.get("id") && (checking.contains_key(&id.to_string()) || pending.contains_key(&id.to_string())) {
                    answer(&output,id,"An MCP request with this ID is already pending").await;continue;
                }
                if checks.len()>=MAX_CALLS || pending.len()>=MAX_CALLS {
                    if let Some(id)=value.get("id"){answer(&output,id,"Too many MCP requests are pending").await;}continue;
                }
                let own=control.clone();let revision=grant.revision();let timeout=Duration::from_mins(5);
                let id=value.get("method").and_then(|_|value.get("id")).map(Value::to_string);
                let original_id = value.get("id").cloned();
                let task=checks.spawn(async move {
                    let result=tokio::time::timeout(timeout,own.check(&revision,&value)).await.map_err(|_|"MCP permission request timed out".into()).and_then(|r|r);
                    (value,result)
                });
                if let Some(id)=id{checking.insert(id,(original_id.expect("request ID"),task));}
            },
            checked=checks.join_next(),if !checks.is_empty()=>{
                let Some(Ok((value,allowed)))=checked else{continue;};
                if let Some(id)=value.get("id"){checking.remove(&id.to_string());}
                match allowed {
                    Ok(Response::Done)=>{},
                    Ok(Response::McpConnection(_))=>{
                        if let Some(id)=value.get("id"){answer(&output,id,"The server is reconnecting; this request was not sent. Retry after it reconnects.").await;}continue;
                    },
                    Ok(_)=>{if let Some(id)=value.get("id"){answer(&output,id,"Unexpected MCP authorization response").await;}continue;},
                    Err(why)=>{if let Some(id)=value.get("id"){answer(&output,id,&why).await;}continue;},
                }
                let Ok(message)=serde_json::from_value(value.clone()) else {if let Some(id)=value.get("id"){answer(&output,id,"Invalid MCP request").await;}continue;};
                if let Some(id)=value.get("id") && value.get("method").is_some(){
                    pending.insert(id.to_string(),(id.clone(),tokio::time::Instant::now()+Duration::from_secs(u64::from(grant.bound.definition.as_ref().expect("granted transport").timeout_seconds))));
                }
                sends.spawn(upstream.send(message));
            },
            sent=sends.join_next(),if !sends.is_empty()=>{if let Some(Ok(Err(e)))=sent{break Err(e.to_string());}},
            reply=upstream.receive()=>{
                let Some(reply)=reply else{
                    let detail=upstream.diagnostic.lock().map(|d|d.clone()).unwrap_or_default();
                    break Err(if detail.is_empty(){"The MCP server disconnected".into()}else{format!("The MCP server disconnected: {detail}")});
                };
                let value=serde_json::to_value(reply).map_err(|_|"Cannot encode MCP response")?;
                if value.get("method").is_none() && let Some(id)=value.get("id") && pending.remove(&id.to_string()).is_none(){continue;}
                if value.pointer("/result/protocolVersion").is_some(){initialized=true;control.report(&grant.revision(),McpState::Connected,String::new(),Vec::new()).await;}
                let tools=transport::tool_infos(&value, &grant.bound);
                if !tools.is_empty(){control.report(&grant.revision(),McpState::Connected,String::new(),tools).await;}
                if !matches!(tokio::time::timeout(Duration::from_secs(10),output.send(value)).await,Ok(Ok(()))){break Ok(());}
            },
            _=heartbeat.tick()=>{
                let checked=tokio::time::timeout(Duration::from_secs(5),control.heartbeat(&grant.revision(),initialized)).await;
                match checked {
                    Ok(Ok(Response::Done))=>{},
                    Ok(Ok(Response::McpConnection(configuration)))=>{
                        // An explicit reconnect restores only MCP initialization.
                        // Tool requests are failed, never replayed.
                        checks.abort_all();
                        for (_, (id,_)) in std::mem::take(&mut checking){answer(&output,&id,"The user reconnected this MCP server before this request was sent. Retry after reconnecting.").await;}
                        sends.abort_all();
                        for (_, (id,_)) in std::mem::take(&mut pending){answer(&output,&id,"The user reconnected this MCP server; the previous request was interrupted. Check its outcome before retrying.").await;}
                        let next:Grant=serde_json::from_str(&configuration).map_err(|_|"Invalid refreshed MCP grant")?;
                        let _=tokio::time::timeout(Duration::from_secs(2),upstream.close()).await;
                        grant=next;upstream=Upstream::connect(&grant.bound).await?;
                        if let Some(init)=initialization.clone(){
                            let expected=init.get("id").cloned();
                            upstream.send(serde_json::from_value(init).map_err(|_|"Invalid saved MCP initialization")?).await.map_err(|e|e.to_string())?;
                            let timeout=Duration::from_secs(u64::from(grant.bound.definition.as_ref().expect("definition").timeout_seconds));
                            tokio::time::timeout(timeout,async {
                                loop {
                                    let reply=upstream.receive().await.ok_or("Server closed during reconnect")?;
                                    let value=serde_json::to_value(reply).map_err(|_|"Invalid reconnect response")?;
                                    if value.get("id")==expected.as_ref(){if value.get("error").is_some(){return Err("Server refused reconnect initialization");}break;}
                                    if let Some(id)=value.get("id") && value.get("method").is_some(){
                                        let response=if value["method"]=="ping"{json!({"jsonrpc":"2.0","id":id,"result":{}})}else{json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Interactive callbacks are unavailable during reconnect initialization"}})};
                                        upstream.send(serde_json::from_value(response).expect("callback response")).await.map_err(|_|"Cannot answer reconnect callback")?;
                                    }
                                }
                                if initialized {upstream.send(serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).expect("notification")).await.map_err(|_|"Cannot finish MCP reconnect")?;}
                                Ok::<_,&str>(())
                            }).await.map_err(|_|"MCP reconnect timed out")?.map_err(str::to_owned)?;
                            let _=output.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})).await;
                        }
                    },
                    Ok(Err(why)) if why.contains("MCP catalogue is loading")=>{},
                    Ok(Err(why))=>break Err(why),
                    _=>break Err("The daemon no longer authorizes this MCP connection".into()),
                }
                let expired:Vec<_>=pending.iter().filter(|(_,(_,due))|*due<=tokio::time::Instant::now()).map(|(k,_)|k.clone()).collect();
                for key in expired {if let Some((id,_))=pending.remove(&key){
                    sends.spawn(upstream.send(serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id,"reason":"Request timed out"}})).expect("cancellation")));
                    answer(&output,&id,"MCP request timed out; its outcome may be unknown. Check before retrying.").await;
                }}
            },
        }
    };
    checks.abort_all();
    sends.abort_all();
    let _ = tokio::time::timeout(Duration::from_secs(2), upstream.close()).await;
    let detail = result
        .as_ref()
        .err()
        .map(|e| credentials::redacted(&grant.bound, e))
        .unwrap_or_default();
    control
        .report(
            &grant.revision(),
            if result.is_ok() {
                McpState::Disconnected
            } else {
                failure_state(&detail)
            },
            detail,
            Vec::new(),
        )
        .await;
    result
}

pub(crate) fn failure_state(why: &str) -> McpState {
    let lower = why.to_ascii_lowercase();
    if [
        "authentication",
        "unauthorized",
        "401",
        "oauth",
        "token expired",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        McpState::NeedsLogin
    } else {
        McpState::Error
    }
}
