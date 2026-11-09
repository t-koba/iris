use crate::remote_transport::{RemoteTransportKind, RemoteTransportMapping, SshAuthKind};
use crate::state::{
    normalize_notification_timeout_secs, CacheEncryptionPolicy, ListColumnSpec, PreviewPosition,
    PropertiesPosition, DEFAULT_MEMORY_BUDGET_LIMIT_BYTES, DEFAULT_NOTIFICATION_TIMEOUT_SECS,
    DEFAULT_THUMBNAIL_CACHE_LIMIT_BYTES,
};
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

/// Files with any other schema version are quarantined and replaced by defaults.
pub(crate) const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    pub fn i18n_key(self) -> &'static str {
        match self {
            ThemeChoice::System => "theme-system",
            ThemeChoice::Light => "theme-light",
            ThemeChoice::Dark => "theme-dark",
        }
    }
}

impl std::fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tr(self.i18n_key()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocaleChoice {
    #[default]
    System,
    English,
    Japanese,
}

impl LocaleChoice {
    pub const ALL: [LocaleChoice; 3] = [
        LocaleChoice::System,
        LocaleChoice::English,
        LocaleChoice::Japanese,
    ];

    pub fn from_locale(locale: Option<&str>) -> Self {
        match locale {
            Some(value) if value.starts_with("ja") => LocaleChoice::Japanese,
            Some(value) if value.starts_with("en") => LocaleChoice::English,
            _ => LocaleChoice::System,
        }
    }

    pub fn to_locale(self) -> Option<String> {
        match self {
            LocaleChoice::System => None,
            LocaleChoice::English => Some("en-US".to_string()),
            LocaleChoice::Japanese => Some("ja-JP".to_string()),
        }
    }

    pub fn i18n_key(self) -> &'static str {
        match self {
            LocaleChoice::System => "language-system",
            LocaleChoice::English => "language-english",
            LocaleChoice::Japanese => "language-japanese",
        }
    }
}

impl std::fmt::Display for LocaleChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tr(self.i18n_key()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutOverride {
    pub action: String,
    pub combo: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SidebarGroupConfig {
    pub name: String,
    pub collapsed: bool,
    pub items: Vec<SidebarItemConfig>,
    pub kind: crate::state::SidebarGroupKind,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedSidebarGroupConfig {
    name: String,
    collapsed: bool,
    items: Vec<SidebarItemConfig>,
    #[serde(default)]
    system: Option<bool>,
    #[serde(default)]
    kind: Option<crate::state::SidebarGroupKind>,
}

impl<'de> Deserialize<'de> for SidebarGroupConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let saved = SavedSidebarGroupConfig::deserialize(deserializer)?;
        let kind = match (saved.kind, saved.system) {
            (Some(kind), _) => kind,
            (None, Some(system)) => legacy_sidebar_group_kind(&saved.name, system),
            (None, None) => return Err(de::Error::missing_field("kind")),
        };
        Ok(Self {
            name: saved.name,
            collapsed: saved.collapsed,
            items: saved.items,
            kind,
        })
    }
}

fn legacy_sidebar_group_kind(name: &str, system: bool) -> crate::state::SidebarGroupKind {
    use crate::state::SidebarGroupKind;

    let named = |kind: SidebarGroupKind| {
        crate::i18n::translations_of(kind.default_name_key())
            .iter()
            .any(|translation| translation == name)
    };
    let candidates: &[SidebarGroupKind] = if system {
        &[SidebarGroupKind::Places, SidebarGroupKind::Volumes]
    } else {
        &[SidebarGroupKind::Favorites, SidebarGroupKind::Cloud]
    };
    candidates
        .iter()
        .copied()
        .find(|kind| named(*kind))
        .unwrap_or(SidebarGroupKind::Custom)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SidebarItemConfig {
    pub name: String,
    pub path: String,
    pub badge: String,
    #[serde(rename = "color")]
    pub badge_color: [f32; 4],
    pub ejectable: bool,
}

/// In-memory aggregate of every config file.
#[derive(Clone, Debug)]
pub struct LayoutConfig {
    pub sidebar_width: f32,
    pub right_panel_width: f32,
    pub preview_panel_height: f32,
    pub preview_position: PreviewPosition,
    pub dual_pane_enabled: bool,
    pub pane_split_ratio: f32,
    pub sidebar_visible: bool,
    pub preview_panel_visible: bool,
    pub properties_panel_visible: bool,
    pub show_hidden_files: bool,
    pub cache_encryption_policy: CacheEncryptionPolicy,
    pub thumbnail_cache_limit_bytes: usize,
    pub memory_budget_limit_bytes: usize,
    pub map_tile_fetch_enabled: bool,
    pub window_width: f32,
    pub window_height: f32,
    pub window_x: Option<f32>,
    pub window_y: Option<f32>,
    pub theme: ThemeChoice,
    pub locale: Option<String>,
    pub shortcut_overrides: Vec<ShortcutOverride>,
    pub remote_copy_undo_enabled: bool,
    pub notification_timeout_secs: u64,
    pub pdf_text_search: PdfTextSearchSettings,
    pub list_columns: Vec<ListColumnSpec>,
    pub secondary_list_columns: Vec<ListColumnSpec>,
    pub properties_split_ratio: f32,
    pub properties_position: PropertiesPosition,
    pub sidebar_groups: Vec<SidebarGroupConfig>,
    pub tabs: Vec<PaneTabConfig>,
    pub remote_transport_mappings: Vec<RemoteTransportMapping>,
    pub remote_transport_secret_warnings: Vec<RemoteTransportSecretWarning>,
    /// Files that could not be read and were set aside, so their settings are back to defaults.
    pub quarantined_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTransportSecretWarning {
    pub mapping_id: String,
    pub host: String,
    pub error: String,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            sidebar_width: 200.0,
            right_panel_width: 280.0,
            preview_panel_height: 300.0,
            preview_position: PreviewPosition::Right,
            dual_pane_enabled: false,
            pane_split_ratio: 0.5,
            sidebar_visible: true,
            preview_panel_visible: false,
            properties_panel_visible: false,
            show_hidden_files: false,
            cache_encryption_policy: CacheEncryptionPolicy::Auto,
            thumbnail_cache_limit_bytes: DEFAULT_THUMBNAIL_CACHE_LIMIT_BYTES,
            memory_budget_limit_bytes: DEFAULT_MEMORY_BUDGET_LIMIT_BYTES,
            map_tile_fetch_enabled: false,
            window_width: 1280.0,
            window_height: 800.0,
            window_x: None,
            window_y: None,
            theme: ThemeChoice::System,
            locale: None,
            shortcut_overrides: Vec::new(),
            remote_copy_undo_enabled: true,
            notification_timeout_secs: DEFAULT_NOTIFICATION_TIMEOUT_SECS,
            pdf_text_search: PdfTextSearchSettings::default(),
            list_columns: ListColumnSpec::default_columns(),
            secondary_list_columns: ListColumnSpec::default_columns(),
            properties_split_ratio: 0.5,
            properties_position: PropertiesPosition::BelowPreview,
            sidebar_groups: Vec::new(),
            tabs: Vec::new(),
            remote_transport_mappings: Vec::new(),
            remote_transport_secret_warnings: Vec::new(),
            quarantined_files: Vec::new(),
        }
    }
}

/// Search-only per-document byte budgets; not PDF rendering limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PdfTextSearchSettings {
    pub input_bytes: usize,
    pub decoded_bytes: usize,
    pub text_bytes: usize,
}

impl Default for PdfTextSearchSettings {
    fn default() -> Self {
        Self {
            input_bytes: 8 * 1024 * 1024,
            decoded_bytes: 16 * 1024 * 1024,
            text_bytes: 1024 * 1024,
        }
    }
}

impl PdfTextSearchSettings {
    pub fn limits(self) -> iris_thumb_preview::pdf_text::PdfTextLimits {
        iris_thumb_preview::pdf_text::PdfTextLimits {
            input_bytes: self.input_bytes,
            decoded_bytes: self.decoded_bytes,
            text_bytes: self.text_bytes,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsFile {
    pub(crate) schema_version: u32,
    pub(crate) show_hidden_files: bool,
    pub(crate) cache_encryption_policy: CacheEncryptionPolicy,
    pub(crate) thumbnail_cache_limit_bytes: usize,
    pub(crate) memory_budget_limit_bytes: usize,
    pub(crate) map_tile_fetch_enabled: bool,
    pub(crate) theme: ThemeChoice,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(crate) locale: Option<String>,
    pub(crate) shortcut_overrides: Vec<ShortcutOverride>,
    pub(crate) remote_copy_undo_enabled: bool,
    pub(crate) notification_timeout_secs: u64,
    #[serde(default)]
    pub(crate) pdf_text_search: PdfTextSearchSettings,
}

impl Default for SettingsFile {
    fn default() -> Self {
        let defaults = LayoutConfig::default();
        Self {
            schema_version: SCHEMA_VERSION,
            show_hidden_files: defaults.show_hidden_files,
            cache_encryption_policy: defaults.cache_encryption_policy,
            thumbnail_cache_limit_bytes: defaults.thumbnail_cache_limit_bytes,
            memory_budget_limit_bytes: defaults.memory_budget_limit_bytes,
            map_tile_fetch_enabled: defaults.map_tile_fetch_enabled,
            theme: defaults.theme,
            locale: defaults.locale,
            shortcut_overrides: defaults.shortcut_overrides,
            remote_copy_undo_enabled: defaults.remote_copy_undo_enabled,
            notification_timeout_secs: defaults.notification_timeout_secs,
            pdf_text_search: defaults.pdf_text_search,
        }
    }
}

impl SettingsFile {
    pub(crate) fn normalized_notification_timeout_secs(&self) -> u64 {
        normalize_notification_timeout_secs(self.notification_timeout_secs)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayoutFile {
    pub(crate) schema_version: u32,
    pub(crate) sidebar_width: f32,
    pub(crate) right_panel_width: f32,
    pub(crate) preview_panel_height: f32,
    pub(crate) window_width: f32,
    pub(crate) window_height: f32,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(crate) window_x: Option<f32>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(crate) window_y: Option<f32>,
    pub(crate) preview_position: PreviewPosition,
    pub(crate) dual_pane_enabled: bool,
    pub(crate) pane_split_ratio: f32,
    pub(crate) sidebar_visible: bool,
    pub(crate) preview_panel_visible: bool,
    pub(crate) properties_panel_visible: bool,
    pub(crate) list_columns: Vec<ListColumnSpec>,
    pub(crate) secondary_list_columns: Vec<ListColumnSpec>,
    pub(crate) properties_split_ratio: f32,
    pub(crate) properties_position: PropertiesPosition,
    pub(crate) sidebar_groups: Vec<SidebarGroupConfig>,
    pub(crate) tabs: Vec<PaneTabConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaneTabConfig {
    pub pane: usize,
    pub path: String,
    pub active: bool,
}

impl Default for LayoutFile {
    fn default() -> Self {
        let defaults = LayoutConfig::default();
        Self {
            schema_version: SCHEMA_VERSION,
            sidebar_width: defaults.sidebar_width,
            right_panel_width: defaults.right_panel_width,
            preview_panel_height: defaults.preview_panel_height,
            window_width: defaults.window_width,
            window_height: defaults.window_height,
            window_x: defaults.window_x,
            window_y: defaults.window_y,
            preview_position: defaults.preview_position,
            dual_pane_enabled: defaults.dual_pane_enabled,
            pane_split_ratio: defaults.pane_split_ratio,
            sidebar_visible: defaults.sidebar_visible,
            preview_panel_visible: defaults.preview_panel_visible,
            properties_panel_visible: defaults.properties_panel_visible,
            list_columns: ListColumnSpec::default_columns(),
            secondary_list_columns: ListColumnSpec::default_columns(),
            properties_split_ratio: defaults.properties_split_ratio,
            properties_position: defaults.properties_position,
            sidebar_groups: defaults.sidebar_groups,
            tabs: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionsFile {
    pub(crate) schema_version: u32,
    pub(crate) remote_transport_mappings: Vec<RemoteTransportMappingFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteTransportMappingFile {
    pub(crate) id: String,
    pub(crate) local_mount_path: String,
    pub(crate) host: String,
    pub(crate) ssh_port: u16,
    pub(crate) ssh_username: String,
    pub(crate) remote_base_path: String,
    #[serde(deserialize_with = "deserialize_transport_config")]
    pub(crate) transport: String,
    #[serde(deserialize_with = "deserialize_auth_config")]
    pub(crate) auth: String,
    pub(crate) ssh_identity_file: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub(crate) ssh_secret_ref: Option<String>,
    pub(crate) enabled: bool,
    pub(crate) auto_detected: bool,
    pub(crate) notes: String,
}

impl From<&RemoteTransportMapping> for RemoteTransportMappingFile {
    fn from(mapping: &RemoteTransportMapping) -> Self {
        Self {
            id: mapping.id.clone(),
            local_mount_path: mapping.local_mount_path.clone(),
            host: mapping.host.clone(),
            ssh_port: mapping.ssh_port,
            ssh_username: mapping.ssh_username.clone(),
            remote_base_path: mapping.remote_base_path.clone(),
            transport: mapping.transport.as_config_str().to_string(),
            auth: mapping.auth.as_config_str().to_string(),
            ssh_identity_file: mapping.ssh_identity_file.clone(),
            ssh_secret_ref: (!mapping.ssh_password.is_empty()).then(|| mapping.id.clone()),
            enabled: mapping.enabled,
            auto_detected: mapping.auto_detected,
            notes: mapping.notes.clone(),
        }
    }
}

impl RemoteTransportMappingFile {
    pub(crate) fn into_mapping(self) -> Result<RemoteTransportMapping, String> {
        let transport = RemoteTransportKind::from_config_str(&self.transport)
            .ok_or_else(|| format!("unknown remote transport '{}'", self.transport))?;
        let auth = SshAuthKind::from_config_str(&self.auth)
            .ok_or_else(|| format!("unknown SSH auth '{}'", self.auth))?;
        Ok(RemoteTransportMapping {
            id: self.id,
            local_mount_path: self.local_mount_path,
            host: self.host,
            ssh_port: self.ssh_port,
            ssh_username: self.ssh_username,
            remote_base_path: self.remote_base_path,
            transport,
            auth,
            ssh_identity_file: self.ssh_identity_file,
            ssh_password: String::new(),
            enabled: self.enabled,
            auto_detected: self.auto_detected,
            notes: self.notes,
        })
    }
}

fn deserialize_transport_config<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    RemoteTransportKind::from_config_str(&value)
        .map(|_| value.clone())
        .ok_or_else(|| de::Error::custom(format!("unknown remote transport '{value}'")))
}

fn deserialize_auth_config<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    SshAuthKind::from_config_str(&value)
        .map(|_| value.clone())
        .ok_or_else(|| de::Error::custom(format!("unknown SSH auth '{value}'")))
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
