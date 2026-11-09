use super::io::{
    apply_ssh_secret_load_result, normalize_list_columns, parse_segment,
    schema_version_quarantine_reason,
};
use super::model::*;
use crate::remote_transport::{RemoteTransportKind, RemoteTransportMapping, SshAuthKind};
use crate::state::{ListColumnKind, ListColumnSpec};

#[test]
fn settings_roundtrip_preserves_total_memory_budget() {
    let settings = SettingsFile {
        memory_budget_limit_bytes: 384 * 1024 * 1024,
        ..SettingsFile::default()
    };

    let serialized = serde_json::to_string(&settings).expect("serialize settings");
    let parsed: SettingsFile = serde_json::from_str(&serialized).expect("parse settings");
    assert_eq!(parsed.memory_budget_limit_bytes, 384 * 1024 * 1024);
}

#[test]
fn serde_layout_roundtrip_preserves_sidebar_groups() {
    let layout = LayoutFile {
        schema_version: SCHEMA_VERSION,
        window_width: 1440.0,
        window_height: 900.0,
        window_x: Some(120.0),
        window_y: Some(80.0),
        list_columns: vec![
            ListColumnSpec {
                kind: ListColumnKind::Name,
                width: 0.0,
                visible: true,
                sortable: true,
            },
            ListColumnSpec {
                kind: ListColumnKind::Date,
                width: 222.0,
                visible: true,
                sortable: false,
            },
            ListColumnSpec {
                kind: ListColumnKind::Size,
                width: 96.0,
                visible: false,
                sortable: true,
            },
        ],
        secondary_list_columns: vec![
            ListColumnSpec {
                kind: ListColumnKind::Size,
                width: 88.0,
                visible: true,
                sortable: true,
            },
            ListColumnSpec {
                kind: ListColumnKind::Name,
                width: 0.0,
                visible: true,
                sortable: true,
            },
            ListColumnSpec {
                kind: ListColumnKind::Date,
                width: 188.0,
                visible: false,
                sortable: false,
            },
        ],
        sidebar_groups: vec![
            SidebarGroupConfig {
                name: "Places".to_string(),
                collapsed: true,
                items: Vec::new(),
                kind: crate::state::SidebarGroupKind::Places,
            },
            SidebarGroupConfig {
                name: "Favorites".to_string(),
                collapsed: false,
                items: vec![SidebarItemConfig {
                    name: "src".to_string(),
                    path: "/Users/test/src".to_string(),
                    badge: "*".to_string(),
                    badge_color: [0.5, 0.5, 0.5, 1.0],
                    ejectable: false,
                }],
                kind: crate::state::SidebarGroupKind::Favorites,
            },
        ],
        tabs: vec![
            PaneTabConfig {
                pane: 0,
                path: "/Users/test/src".to_string(),
                active: false,
            },
            PaneTabConfig {
                pane: 0,
                path: "/Users/test/Pictures".to_string(),
                active: true,
            },
            PaneTabConfig {
                pane: 1,
                path: "/Volumes/Data".to_string(),
                active: true,
            },
        ],
        ..LayoutFile::default()
    };

    let serialized = serde_json::to_string_pretty(&layout).expect("serialize layout");
    let parsed: LayoutFile = serde_json::from_str(&serialized).expect("parse layout");

    assert_eq!(parsed.schema_version, SCHEMA_VERSION);
    assert_eq!(parsed.window_width, 1440.0);
    assert_eq!(parsed.window_height, 900.0);
    assert_eq!(parsed.window_x, Some(120.0));
    assert_eq!(parsed.window_y, Some(80.0));
    assert_eq!(parsed.list_columns[1].kind, ListColumnKind::Date);
    assert_eq!(parsed.list_columns[1].width, 222.0);
    assert!(!parsed.list_columns[1].sortable);
    assert!(!parsed.list_columns[2].visible);
    assert_eq!(parsed.secondary_list_columns[0].kind, ListColumnKind::Size);
    assert_eq!(parsed.secondary_list_columns[2].width, 188.0);
    assert_eq!(parsed.sidebar_groups.len(), 2);
    assert_eq!(parsed.sidebar_groups[0].name, "Places");
    assert_eq!(
        parsed.sidebar_groups[0].kind,
        crate::state::SidebarGroupKind::Places
    );
    assert_eq!(parsed.sidebar_groups[1].items[0].name, "src");
    assert_eq!(parsed.tabs.len(), 3);
    assert_eq!(parsed.tabs[1].pane, 0);
    assert_eq!(parsed.tabs[1].path, "/Users/test/Pictures");
    assert!(parsed.tabs[1].active);
}

#[test]
fn layout_parser_migrates_legacy_sidebar_groups_without_losing_layout_state() {
    let places_key = "sidebar-group-places";
    let current_places_name = crate::i18n::tr(places_key);
    let saved_places_name = crate::i18n::translations_of(places_key)
        .into_iter()
        .find(|name| name != &current_places_name)
        .expect("the other bundled translation");

    let layout = LayoutFile {
        window_width: 1440.0,
        window_height: 900.0,
        window_x: Some(120.0),
        window_y: Some(80.0),
        tabs: vec![PaneTabConfig {
            pane: 1,
            path: "/sample/photos".to_string(),
            active: true,
        }],
        ..LayoutFile::default()
    };
    let mut value = serde_json::to_value(layout).expect("serialize current layout");
    value["sidebar_groups"] = serde_json::json!([
        {
            "name": saved_places_name,
            "collapsed": true,
            "items": [],
            "system": true
        },
        {
            "name": "Favorites",
            "collapsed": false,
            "items": [{
                "name": "Project",
                "path": "/sample/project",
                "badge": "*",
                "color": [0.5, 0.5, 0.5, 1.0],
                "ejectable": false
            }],
            "system": false,
            "kind": "favorites"
        },
        {
            "name": "Work",
            "collapsed": true,
            "items": [],
            "system": false
        },
        {
            "name": "Legacy system group",
            "collapsed": false,
            "items": [],
            "system": true
        }
    ]);

    let parsed = parse_segment::<LayoutFile>(&value.to_string())
        .expect("parse schema-v1 layout written by the prior serializer");
    assert_eq!(parsed.window_width, 1440.0);
    assert_eq!(parsed.window_height, 900.0);
    assert_eq!(parsed.window_x, Some(120.0));
    assert_eq!(parsed.window_y, Some(80.0));
    assert_eq!(parsed.tabs.len(), 1);
    assert_eq!(parsed.tabs[0].pane, 1);
    assert_eq!(parsed.tabs[0].path, "/sample/photos");
    assert!(parsed.tabs[0].active);
    assert_eq!(parsed.sidebar_groups.len(), 4);
    assert_eq!(parsed.sidebar_groups[0].name, saved_places_name);
    assert!(parsed.sidebar_groups[0].collapsed);
    assert_eq!(
        parsed.sidebar_groups[0].kind,
        crate::state::SidebarGroupKind::Places
    );
    assert_eq!(
        parsed.sidebar_groups[1].kind,
        crate::state::SidebarGroupKind::Favorites
    );
    assert_eq!(parsed.sidebar_groups[1].name, "Favorites");
    assert!(!parsed.sidebar_groups[1].collapsed);
    assert_eq!(parsed.sidebar_groups[1].items[0].name, "Project");
    assert_eq!(parsed.sidebar_groups[1].items[0].path, "/sample/project");
    assert_eq!(
        parsed.sidebar_groups[2].kind,
        crate::state::SidebarGroupKind::Custom
    );
    assert_eq!(
        parsed.sidebar_groups[3].kind,
        crate::state::SidebarGroupKind::Custom
    );

    let current = serde_json::to_value(&parsed).expect("serialize migrated layout");
    assert!(current["sidebar_groups"][0].get("system").is_none());
    assert_eq!(current["sidebar_groups"][0]["kind"], "places");

    let mut unexpected_field = value.clone();
    unexpected_field["sidebar_groups"][0]["unexpected"] = serde_json::json!(true);
    let error = parse_segment::<LayoutFile>(&unexpected_field.to_string())
        .expect_err("reject unrelated unknown sidebar-group fields");
    assert!(error.contains("unknown field"));

    let mut missing_legacy_marker = value;
    missing_legacy_marker["sidebar_groups"][0]
        .as_object_mut()
        .expect("sidebar group object")
        .remove("system");
    let error = parse_segment::<LayoutFile>(&missing_legacy_marker.to_string())
        .expect_err("reject groups missing both current kind and legacy marker");
    assert!(error.contains("missing field `kind`"));
}

#[test]
fn list_column_normalization_preserves_order_and_repairs_widths() {
    let normalized = normalize_list_columns(vec![
        ListColumnSpec {
            kind: ListColumnKind::Size,
            width: 12.0,
            visible: true,
            sortable: true,
        },
        ListColumnSpec {
            kind: ListColumnKind::Name,
            width: -4.0,
            visible: false,
            sortable: false,
        },
        ListColumnSpec {
            kind: ListColumnKind::Size,
            width: 999.0,
            visible: false,
            sortable: true,
        },
    ]);

    assert_eq!(
        normalized
            .iter()
            .map(|column| column.kind)
            .collect::<Vec<_>>(),
        vec![
            ListColumnKind::Size,
            ListColumnKind::Name,
            ListColumnKind::Date
        ]
    );
    assert_eq!(normalized[0].width, 50.0);
    assert_eq!(normalized[1].width, 0.0);
    assert!(normalized[1].visible);
    assert!(!normalized[1].sortable);
    assert_eq!(normalized[2].width, 140.0);
    assert!(normalized[2].sortable);
}

#[test]
fn serde_connections_do_not_persist_plain_ssh_passwords() {
    let original = RemoteTransportMapping {
        id: "m0002".into(),
        local_mount_path: "/Volumes/Multimedia".into(),
        host: "qnap.local".into(),
        ssh_port: 22,
        ssh_username: String::new(),
        remote_base_path: "/volume1/multimedia".into(),
        transport: RemoteTransportKind::RsyncSsh,
        auth: SshAuthKind::Password,
        ssh_identity_file: String::new(),
        ssh_password: "daemon-secret".into(),
        enabled: false,
        auto_detected: true,
        notes: "QNAP TS-453D".into(),
    };
    let file = ConnectionsFile {
        schema_version: SCHEMA_VERSION,
        remote_transport_mappings: vec![RemoteTransportMappingFile::from(&original)],
    };

    let serialized = serde_json::to_string_pretty(&file).expect("serialize connections");
    assert!(!serialized.contains("daemon-secret"));
    assert!(serialized.contains("ssh_secret_ref"));

    let parsed: ConnectionsFile = serde_json::from_str(&serialized).expect("parse connections");
    let mapping = parsed.remote_transport_mappings[0]
        .clone()
        .into_mapping()
        .expect("parse mapping");
    assert_eq!(mapping.host, "qnap.local");
    assert_eq!(mapping.transport, RemoteTransportKind::RsyncSsh);
    assert_eq!(mapping.auth, SshAuthKind::Password);
    assert!(mapping.ssh_password.is_empty());
}

#[test]
fn ssh_secret_load_success_restores_password() {
    let mapping = RemoteTransportMapping {
        id: "m0003".into(),
        local_mount_path: "/Volumes/Share".into(),
        host: "nas.local".into(),
        ssh_port: 22,
        ssh_username: "iris".into(),
        remote_base_path: "/share".into(),
        transport: RemoteTransportKind::Sftp,
        auth: SshAuthKind::Password,
        ssh_identity_file: String::new(),
        ssh_password: String::new(),
        enabled: true,
        auto_detected: false,
        notes: String::new(),
    };

    let (mapping, warning) = apply_ssh_secret_load_result(mapping, Ok("restored-secret".into()));

    assert_eq!(mapping.ssh_password, "restored-secret");
    assert!(mapping.enabled);
    assert!(warning.is_none());
}

#[test]
fn ssh_secret_load_failure_disables_mapping_and_warns() {
    let mapping = RemoteTransportMapping {
        id: "m0004".into(),
        local_mount_path: "/Volumes/Share".into(),
        host: "nas.local".into(),
        ssh_port: 22,
        ssh_username: "iris".into(),
        remote_base_path: "/share".into(),
        transport: RemoteTransportKind::Sftp,
        auth: SshAuthKind::Password,
        ssh_identity_file: String::new(),
        ssh_password: String::new(),
        enabled: true,
        auto_detected: false,
        notes: String::new(),
    };

    let (mapping, warning) =
        apply_ssh_secret_load_result(mapping, Err("missing credential".into()));

    assert!(mapping.ssh_password.is_empty());
    assert!(!mapping.enabled);
    let warning = warning.expect("secret load warning");
    assert_eq!(warning.mapping_id, "m0004");
    assert_eq!(warning.host, "nas.local");
    assert_eq!(warning.error, "missing credential");
}

#[test]
fn missing_schema_version_is_rejected_before_deserialization() {
    let value: serde_json::Value = serde_json::from_str(r#"{"sidebar_width": 184.0}"#).unwrap();
    assert_eq!(
        schema_version_quarantine_reason(&value),
        Err("missing-schema-version".to_string())
    );
}

#[test]
fn unknown_settings_fields_are_rejected() {
    let mut value = serde_json::to_value(SettingsFile::default()).expect("settings json");
    value["future_field"] = serde_json::Value::String("rejected".to_string());

    let error = serde_json::from_value::<SettingsFile>(value).expect_err("unknown field");
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn newer_schema_versions_are_rejected() {
    let mut value = serde_json::to_value(SettingsFile::default()).expect("settings json");
    value["schema_version"] =
        serde_json::Value::Number(serde_json::Number::from(SCHEMA_VERSION + 1));

    assert_eq!(
        schema_version_quarantine_reason(&value),
        Err(format!("unsupported-schema-{}", SCHEMA_VERSION + 1))
    );
}

#[test]
fn remote_transport_config_rejects_unknown_transport_and_auth() {
    let mapping = RemoteTransportMapping {
        id: "m0005".into(),
        local_mount_path: "/Volumes/Share".into(),
        host: "nas.local".into(),
        ssh_port: 22,
        ssh_username: "iris".into(),
        remote_base_path: "/share".into(),
        transport: RemoteTransportKind::Sftp,
        auth: SshAuthKind::Agent,
        ssh_identity_file: String::new(),
        ssh_password: String::new(),
        enabled: true,
        auto_detected: false,
        notes: String::new(),
    };
    let file = ConnectionsFile {
        schema_version: SCHEMA_VERSION,
        remote_transport_mappings: vec![RemoteTransportMappingFile::from(&mapping)],
    };

    let mut invalid_transport = serde_json::to_value(&file).expect("connections json");
    invalid_transport["remote_transport_mappings"][0]["transport"] =
        serde_json::Value::String("Scp".to_string());
    let error =
        serde_json::from_value::<ConnectionsFile>(invalid_transport).expect_err("transport");
    assert!(error.to_string().contains("unknown remote transport"));

    let mut invalid_auth = serde_json::to_value(&file).expect("connections json");
    invalid_auth["remote_transport_mappings"][0]["auth"] =
        serde_json::Value::String("KeyboardInteractive".to_string());
    let error = serde_json::from_value::<ConnectionsFile>(invalid_auth).expect_err("auth");
    assert!(error.to_string().contains("unknown SSH auth"));
}

#[test]
fn serde_settings_roundtrip_preserves_p3_fields() {
    let settings = SettingsFile {
        theme: ThemeChoice::Light,
        locale: Some("en-US".to_string()),
        map_tile_fetch_enabled: true,
        notification_timeout_secs: 12,
        shortcut_overrides: vec![ShortcutOverride {
            action: "copy".to_string(),
            combo: "Cmd+C".to_string(),
        }],
        ..SettingsFile::default()
    };

    let serialized = serde_json::to_string_pretty(&settings).expect("serialize settings");
    let parsed: SettingsFile = serde_json::from_str(&serialized).expect("parse settings");

    assert_eq!(parsed.theme, ThemeChoice::Light);
    assert_eq!(parsed.locale.as_deref(), Some("en-US"));
    assert!(parsed.map_tile_fetch_enabled);
    assert_eq!(parsed.normalized_notification_timeout_secs(), 12);
    assert_eq!(parsed.shortcut_overrides[0].action, "copy");
}

#[test]
fn notification_timeout_setting_is_clamped_on_read() {
    let settings = SettingsFile {
        notification_timeout_secs: crate::state::MAX_NOTIFICATION_TIMEOUT_SECS + 1,
        ..SettingsFile::default()
    };

    assert_eq!(
        settings.normalized_notification_timeout_secs(),
        crate::state::MAX_NOTIFICATION_TIMEOUT_SECS
    );
}

#[test]
fn pdf_text_search_settings_default_on_old_files_and_preserve_overrides() {
    let mut old = serde_json::to_value(SettingsFile {
        show_hidden_files: true,
        ..SettingsFile::default()
    })
    .unwrap();
    old.as_object_mut().unwrap().remove("pdf_text_search");
    let loaded: SettingsFile = serde_json::from_value(old.clone()).unwrap();
    assert!(loaded.show_hidden_files);
    assert_eq!(
        loaded.pdf_text_search,
        PdfTextSearchSettings {
            input_bytes: 8_388_608,
            decoded_bytes: 16_777_216,
            text_bytes: 1_048_576
        }
    );
    // Partial overrides inherit each missing field; zero is a real stop budget.
    old["pdf_text_search"] = serde_json::json!({"text_bytes": 0});
    let loaded: SettingsFile = serde_json::from_value(old).unwrap();
    assert_eq!(loaded.pdf_text_search.input_bytes, 8_388_608);
    assert_eq!(loaded.pdf_text_search.text_bytes, 0);
    let saved = serde_json::to_string(&loaded).unwrap();
    let roundtrip: SettingsFile = serde_json::from_str(&saved).unwrap();
    assert_eq!(roundtrip.pdf_text_search, loaded.pdf_text_search);
    assert_eq!(roundtrip.pdf_text_search.limits().text_bytes, 0);
}
