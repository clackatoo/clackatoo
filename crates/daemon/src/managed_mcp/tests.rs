mod http;

use super::{
    config::{self, Definition, Registry, Setting},
    credentials, transport,
};
use crate::skills::{Catalogue, Roots};
use clackatoo_protocol::McpScope;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;

fn roots(dir: &std::path::Path) -> Roots {
    let roots = Roots {
        home: dir.join("home"),
        cwd: dir.join("repo"),
        config: Some(dir.join("config")),
    };
    for path in [&roots.home, &roots.cwd, roots.config.as_ref().unwrap()] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(roots.cwd.join(".git"), "fixture").unwrap();
    roots
}
fn write(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn shadowed_entries_do_not_consume_the_remaining_registry_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let project: serde_json::Map<_, _> = (0..126)
        .map(|i| {
            (
                format!("server{i:03}"),
                serde_json::json!({"command":"server"}),
            )
        })
        .collect();
    write(
        &roots.cwd.join(".agents/mcp.json"),
        &serde_json::json!({"servers":project}).to_string(),
    );
    write(
        &config::personal(&roots).join("mcp.json"),
        r#"{"servers":{"server000":{"command":"shadowed"},"server001":{"command":"shadowed"},"unique":{"command":"unique"}}}"#,
    );
    let registry = Registry::discover(&roots, &BTreeSet::new());
    assert_eq!(registry.servers.len(), 127);
    assert!(registry.servers.contains_key("unique"));
    assert_eq!(
        registry.servers["server000"]
            .definition
            .as_ref()
            .unwrap()
            .command
            .as_deref(),
        Some("server")
    );
}

#[test]
fn diagnostics_redact_overlapping_credentials_and_values_retained_by_a_running_process() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    write(
        &config::personal(&roots).join("mcp.json"),
        r#"{"servers":{"search":{"command":"server"}}}"#,
    );
    let registry = Registry::discover(&roots, &BTreeSet::new());
    let bound = &registry.servers["search"];
    credentials::set(&bound.private, "a-short", "shared-prefix").unwrap();
    credentials::set(&bound.private, "z-long", "shared-prefix-longer").unwrap();
    assert_eq!(
        credentials::redacted(bound, "shared-prefix-longer"),
        "[redacted]"
    );
    credentials::set(&bound.private, "a-short", "replacement").unwrap();
    assert_eq!(
        credentials::redacted_with(bound, "shared-prefix", &["shared-prefix".into()]),
        "[redacted]"
    );
    credentials::set(&bound.private, "single-character", "e").unwrap();
    assert_eq!(credentials::redacted(bound, "replacement"), "[redacted]");
    assert_eq!(
        credentials::redacted(bound, &"e".repeat(10_000)).len(),
        4096
    );
}

#[test]
fn reviewed_imports_preserve_provider_files_and_move_values_to_private_references() {
    use super::import;
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let source = roots.home.join(".claude.json");
    let original = r#"{"mcpServers":{"local":{"command":"search-server","env":{"KEY":"private-import-value"}},"remote":{"type":"http","url":"https://example.com/mcp","headers":{"Authorization":"Bearer private-http-import"}},"restricted":{"command":"restricted-server","enabled_tools":["read"]}}}"#;
    write(&source, original);
    let preview = import::preview(&roots, "claude").unwrap();
    assert_eq!(preview.len(), 3);
    assert!(!format!("{preview:?}").contains("private-import-value"));
    assert!(!format!("{preview:?}").contains("private-http-import"));
    let registry = Registry::discover(&roots, &BTreeSet::new());
    assert!(
        import::apply(
            &roots,
            &registry,
            "claude",
            &["local".into(), "restricted".into()]
        )
        .is_err()
    );
    assert!(!config::personal(&roots).join("mcp.json").exists());
    import::apply(
        &roots,
        &registry,
        "claude",
        &["local".into(), "remote".into()],
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(source).unwrap(), original);
    let saved = std::fs::read_to_string(config::personal(&roots).join("mcp.json")).unwrap();
    assert!(!saved.contains("private-import-value") && !saved.contains("private-http-import"));
    let registry = Registry::discover(&roots, &BTreeSet::new());
    let bound = &registry.servers["local"];
    let setting = &bound.definition.as_ref().unwrap().env["KEY"];
    assert!(matches!(setting, Setting::Secret { .. }));
    assert_eq!(
        credentials::resolve(&bound.private, setting).unwrap(),
        "private-import-value"
    );
    assert!(import::apply(&roots, &registry, "claude", &["local".into()]).is_err());
    assert_eq!(
        std::fs::read_to_string(config::personal(&roots).join("mcp.json")).unwrap(),
        saved
    );
    write(
        &roots.cwd.join(".codex/config.toml"),
        "[mcp_servers.codex_docs]\nurl='https://example.com/mcp'\n[mcp_servers.codex_docs.http_headers]\nAuthorization='Bearer private-codex-import'\n",
    );
    import::apply(&roots, &registry, "codex", &["codex_docs".into()]).unwrap();
    assert!(
        !std::fs::read_to_string(config::personal(&roots).join("mcp.json"))
            .unwrap()
            .contains("private-codex-import")
    );
}

#[test]
fn project_definitions_shadow_personal_even_when_invalid_and_discovery_never_runs_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let marker = dir.path().join("must-not-exist");
    write(
        &roots.config.as_ref().unwrap().join("mcp.json"),
        &serde_json::json!({"servers":{"search":{"command":"touch","args":[marker]}}}).to_string(),
    );
    write(
        &roots.cwd.join(".agents/mcp.json"),
        r#"{"servers":{"search":{"url":"http://unsafe.example/mcp"}}}"#,
    );
    let catalogue = Registry::discover(&roots, &BTreeSet::new());
    let server = &catalogue.servers["search"];
    assert!(server.definition.is_none());
    assert!(!server.problem.is_empty());
    assert_eq!(server.scope, McpScope::Project);
    assert!(!marker.exists());
}

#[test]
fn saving_detects_conflicts_and_keeps_other_servers_and_project_approval_tracks_exact_content() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let catalogue = Registry::discover(&roots, &BTreeSet::new());
    config::save(
        &roots,
        &catalogue,
        "search",
        Some(r#"{"command":"server"}"#),
        McpScope::Project,
        None,
    )
    .unwrap();
    let catalogue = Registry::discover(&roots, &BTreeSet::new());
    let first = &catalogue.servers["search"];
    assert!(!first.trusted());
    credentials::approve(&first.private, first.grant()).unwrap();
    let catalogue = Registry::discover(&roots, &BTreeSet::new());
    assert!(catalogue.servers["search"].trusted());
    config::save(
        &roots,
        &catalogue,
        "other",
        Some(r#"{"command":"another"}"#),
        McpScope::Project,
        None,
    )
    .unwrap();
    config::save(
        &roots,
        &catalogue,
        "search",
        Some(r#"{"command":"changed"}"#),
        McpScope::Project,
        Some(&first.revision),
    )
    .unwrap();
    let current = Registry::discover(&roots, &BTreeSet::new());
    assert!(!current.servers["search"].trusted());
    assert!(current.servers.contains_key("other"));
    assert!(
        config::save(
            &roots,
            &current,
            "search",
            Some(r#"{"command":"stale"}"#),
            McpScope::Project,
            Some(&first.revision)
        )
        .is_err()
    );
    let current = Registry::discover(&roots, &BTreeSet::new());
    assert_eq!(
        current.servers["search"]
            .definition
            .as_ref()
            .unwrap()
            .command
            .as_deref(),
        Some("changed")
    );
}

#[test]
fn packaged_servers_are_overridden_without_writing_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let package = dir.path().join("package");
    let text = r#"{"servers":{"search":{"command":"server"}}}"#;
    write(&package.join("mcp.json"), text);
    let registry = Registry::discover(&roots, &BTreeSet::from([package.clone()]));
    let server = &registry.servers["search"];
    config::save(
        &roots,
        &registry,
        "search",
        Some(r#"{"command":"server","enabled":false}"#),
        McpScope::Personal,
        Some(&server.revision),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(package.join("mcp.json")).unwrap(),
        text
    );
    let registry = Registry::discover(&roots, &BTreeSet::from([package]));
    assert!(
        !registry.servers["search"]
            .definition
            .as_ref()
            .unwrap()
            .enabled
    );
    assert!(registry.servers["search"].editable);
}

#[test]
fn private_values_have_owner_only_modes_are_references_in_public_configuration_and_are_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let private = config::private(&roots);
    credentials::set(&private, "api-key", "very-private-value").unwrap();
    assert_eq!(
        std::fs::metadata(private.join("credentials.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let registry = Registry::discover(&roots, &BTreeSet::new());
    config::save(
        &roots,
        &registry,
        "search",
        Some(r#"{"command":"server","env":{"KEY":{"secret":"api-key"}}}"#),
        McpScope::Personal,
        None,
    )
    .unwrap();
    let registry = Registry::discover(&roots, &BTreeSet::new());
    let bound = &registry.servers["search"];
    assert!(!bound.info().settings.contains("very-private-value"));
    assert_eq!(
        credentials::redacted(bound, "error: very-private-value"),
        "error: [redacted]"
    );
    assert_eq!(
        credentials::resolve(
            &private,
            &Setting::Secret {
                secret: "api-key".into()
            }
        )
        .unwrap(),
        "very-private-value"
    );
    std::fs::set_permissions(
        private.join("credentials.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(
        credentials::resolve(
            &private,
            &Setting::Secret {
                secret: "api-key".into()
            }
        )
        .is_err()
    );
}

#[test]
fn transport_validation_rejects_credentials_in_urls_and_ambiguous_or_unsupported_transports() {
    for text in [
        r#"{"url":"https://user:password@example.com/mcp"}"#,
        r#"{"url":"http://example.com/mcp"}"#,
        r#"{"command":"one","url":"https://example.com/mcp"}"#,
        r#"{"command":"one","oauth":{}}"#,
        r#"{"command":"one","timeout_seconds":0}"#,
        r#"{"command":"one","unknown":true}"#,
    ] {
        assert!(Definition::parse(text).is_err(), "{text}");
    }
    assert!(Definition::parse(r#"{"url":"http://127.0.0.1:8080/mcp"}"#).is_ok());
}

#[test]
fn private_credential_symlinks_are_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let private = dir.path().join("private");
    credentials::directory(&private).unwrap();
    let elsewhere = dir.path().join("untouched");
    std::fs::write(&elsewhere, "original").unwrap();
    std::os::unix::fs::symlink(&elsewhere, private.join("credentials.json")).unwrap();
    assert!(credentials::set(&private, "key", "value").is_err());
    assert_eq!(std::fs::read_to_string(elsewhere).unwrap(), "original");
}

#[test]
fn forgetting_oauth_prevents_an_old_refresh_or_late_login_from_resurrecting_credentials() {
    use rmcp::transport::auth::{CredentialStore as _, StoredCredentials};
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let registry = Registry::discover(&roots, &BTreeSet::new());
    config::save(
        &roots,
        &registry,
        "search",
        Some(r#"{"url":"https://example.com/mcp"}"#),
        McpScope::Personal,
        None,
    )
    .unwrap();
    let registry = Registry::discover(&roots, &BTreeSet::new());
    let bound = &registry.servers["search"];
    let runtime = transport::runtime().unwrap();
    let old = credentials::OAuthStore::of(bound).unwrap();
    let value = StoredCredentials::new("client".into(), None, vec![], None);
    runtime.block_on(old.save(value.clone())).unwrap();
    assert!(old.exists());
    let guard = runtime.block_on(old.acquire_refresh_guard()).unwrap();
    old.forget().unwrap();
    drop(guard);
    assert!(runtime.block_on(old.load()).unwrap().is_none());
    assert!(runtime.block_on(old.save(value.clone())).is_err());
    let new = credentials::OAuthStore::of(bound).unwrap();
    assert!(!new.exists());
    runtime.block_on(new.save(value)).unwrap();
    assert!(runtime.block_on(new.load()).unwrap().is_some());
    assert!(runtime.block_on(old.load()).unwrap().is_none());
}

#[test]
fn specialists_select_server_names_and_private_bundle_servers_do_not_leak_into_general_chats() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let bundle = roots.cwd.join(".agents/specialists/reviewer");
    write(
        &bundle.join("SPECIALIST.md"),
        "---\nname: reviewer\n---\nReview code\n",
    );
    write(
        &bundle.join("mcp.json"),
        r#"{"servers":{"private-search":{"command":"private-server"}}}"#,
    );
    let catalogue = Catalogue::discover(&roots);
    assert!(!catalogue.managed_mcp.servers["private-search"].available_by_default);
    let scoped = catalogue
        .scoped(&catalogue.specialists["reviewer"])
        .unwrap();
    assert!(scoped.managed_mcp.servers["private-search"].available_by_default);
    let specialist = crate::specialists::Specialist {
        extra: crate::specialists::Extras {
            mcp_servers: Some(vec![]),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        catalogue
            .scoped(&specialist)
            .unwrap()
            .managed_mcp
            .servers
            .is_empty()
    );
}

#[test]
fn a_connection_probe_handles_notifications_callbacks_and_pagination_without_executing_tools() {
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let script = r"import sys,json,os
assert os.environ.get('CLACKATOO_MCP_TOKEN') is None
assert os.environ['KEY']=='private-probe-value'
for line in sys.stdin:
 m=json.loads(line); method=m.get('method')
 if method=='initialize':
  print(json.dumps({'jsonrpc':'2.0','method':'notifications/message','params':{'level':'info','data':'connecting'}}),flush=True)
  result={'protocolVersion':'2025-06-18','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
 elif method=='tools/list':
  page=m.get('params',{}).get('cursor')
  result={'tools':[{'name':'second' if page else 'first','description':'private-probe-value','inputSchema':{'type':'object'}}]}
  if not page: result['nextCursor']='next'
 elif method: continue
 else: continue
 print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
";
    let private = config::private(&roots);
    credentials::set(&private, "probe-key", "private-probe-value").unwrap();
    let definition = Definition {
        command: Some("python3".into()),
        args: vec!["-u".into(), "-c".into(), script.into()],
        env: BTreeMap::from([
            (
                "KEY".into(),
                Setting::Secret {
                    secret: "probe-key".into(),
                },
            ),
            (
                "CLACKATOO_MCP_TOKEN".into(),
                Setting::Literal("must-not-leak".into()),
            ),
        ]),
        ..Definition::default()
    };
    let registry = Registry::discover(&roots, &BTreeSet::new());
    config::save(
        &roots,
        &registry,
        "probe",
        Some(&serde_json::to_string(&definition).unwrap()),
        McpScope::Personal,
        None,
    )
    .unwrap();
    let registry = Registry::discover(&roots, &BTreeSet::new());
    let tools = transport::probe(&registry.servers["probe"]).unwrap();
    assert_eq!(
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(tools.iter().all(|t| t.description == "[redacted]"));
}

#[test]
fn a_server_wanting_a_login_needs_a_sign_in_however_it_says_so() {
    use clackatoo_protocol::McpState;
    for why in [
        "Auth required",
        "HTTP 401 Unauthorized",
        "Authentication required. Sign in from MCP Servers in Settings.",
    ] {
        assert_eq!(
            super::proxy::failure_state(why),
            McpState::NeedsLogin,
            "{why}"
        );
    }
    assert_eq!(
        super::proxy::failure_state("Server refused MCP initialization"),
        McpState::Error
    );
}
