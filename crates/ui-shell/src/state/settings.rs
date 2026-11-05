#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum CacheEncryptionPolicy {
    Never,
    Always,
    Auto,
}

pub const DEFAULT_NOTIFICATION_TIMEOUT_SECS: u64 = 7;
pub const MAX_NOTIFICATION_TIMEOUT_SECS: u64 = 120;

pub fn normalize_notification_timeout_secs(secs: u64) -> u64 {
    secs.min(MAX_NOTIFICATION_TIMEOUT_SECS)
}

pub fn parse_notification_timeout_input_secs(value: &str) -> Option<u64> {
    let secs = value.trim().parse::<u64>().ok()?;
    (secs <= MAX_NOTIFICATION_TIMEOUT_SECS).then_some(secs)
}

pub fn notification_timeout_input_from_secs(secs: u64) -> String {
    secs.to_string()
}

impl CacheEncryptionPolicy {
    pub fn i18n_key(self) -> &'static str {
        match self {
            CacheEncryptionPolicy::Never => "cache-encryption-policy-never",
            CacheEncryptionPolicy::Always => "cache-encryption-policy-always",
            CacheEncryptionPolicy::Auto => "cache-encryption-policy-auto",
        }
    }

    pub const ALL: [CacheEncryptionPolicy; 3] = [
        CacheEncryptionPolicy::Auto,
        CacheEncryptionPolicy::Always,
        CacheEncryptionPolicy::Never,
    ];
}

impl std::fmt::Display for CacheEncryptionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tr(self.i18n_key()))
    }
}

#[derive(Clone, Debug)]
pub struct SidebarGroup {
    pub name: String,
    pub collapsed: bool,
    pub items: Vec<SidebarItem>,
    pub system: bool,
}

#[derive(Clone, Debug)]
pub struct SidebarItem {
    pub name: String,
    pub path: String,
    pub badge: String,
    pub badge_color: [f32; 4],
    pub ejectable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudProvider {
    S3,
    PCloud,
}

impl CloudProvider {
    pub const ALL: [CloudProvider; 2] = [CloudProvider::S3, CloudProvider::PCloud];

    pub fn label(self) -> &'static str {
        match self {
            CloudProvider::S3 => "S3",
            CloudProvider::PCloud => "pCloud",
        }
    }
}

impl std::fmt::Display for CloudProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkProtocol {
    Smb,
    Nfs,
}

impl NetworkProtocol {
    pub const ALL: [NetworkProtocol; 2] = [NetworkProtocol::Smb, NetworkProtocol::Nfs];

    pub fn label(self) -> &'static str {
        match self {
            NetworkProtocol::Smb => "SMB",
            NetworkProtocol::Nfs => "NFS",
        }
    }
}

impl std::fmt::Display for NetworkProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

pub const DEFAULT_SMB_PORT_INPUT: &str = "445";

pub fn parse_smb_port_input(value: &str) -> Option<u16> {
    let port = value.trim().parse::<u16>().ok()?;
    (port > 0).then_some(port)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SettingsSection {
    #[default]
    Appearance,
    Cache,
    Shortcuts,
    Diagnostics,
    Connections,
    RemoteTransfer,
}

impl SettingsSection {
    pub fn i18n_key(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "settings-section-appearance",
            SettingsSection::Cache => "settings-section-cache",
            SettingsSection::Shortcuts => "settings-section-shortcuts",
            SettingsSection::Diagnostics => "settings-section-diagnostics",
            SettingsSection::Connections => "settings-section-connections",
            SettingsSection::RemoteTransfer => "settings-section-remote-transfer",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NetworkConnectFormState {
    pub protocol: NetworkProtocol,
    pub server: String,
    pub smb_port_input: String,
    pub share_path: String,
    pub username: String,
    pub smb_domain: String,
    pub smb_password: String,
    pub smb_allow_guest: bool,
    pub error_message: Option<String>,
    pub available_shares: Option<Vec<String>>,
    pub browsing_shares: bool,
}

impl Default for NetworkConnectFormState {
    fn default() -> Self {
        Self {
            protocol: NetworkProtocol::Smb,
            server: String::new(),
            smb_port_input: DEFAULT_SMB_PORT_INPUT.to_string(),
            share_path: String::new(),
            username: String::new(),
            smb_domain: String::new(),
            smb_password: String::new(),
            smb_allow_guest: false,
            error_message: None,
            available_shares: None,
            browsing_shares: false,
        }
    }
}

impl NetworkConnectFormState {
    pub fn can_connect(&self) -> bool {
        !self.server.trim().is_empty()
            && !self.share_path.trim().is_empty()
            && (self.protocol != NetworkProtocol::Smb
                || (parse_smb_port_input(&self.smb_port_input).is_some()
                    && self.smb_credentials_complete()))
    }

    pub fn smb_credentials_complete(&self) -> bool {
        let has_identity = !self.username.trim().is_empty()
            || !self.smb_domain.trim().is_empty()
            || !self.smb_password.is_empty();
        (has_identity && !self.username.trim().is_empty() && !self.smb_password.is_empty())
            || (!has_identity && self.smb_allow_guest)
    }
}

#[derive(Clone, Debug)]
pub struct CloudConnectFormState {
    pub provider: CloudProvider,
    pub display_name: String,
    pub s3_bucket: String,
    pub s3_region: String,
    pub s3_endpoint_url: String,
    pub s3_prefix: String,
    pub pcloud_client_id: String,
    pub pcloud_redirect_url: String,
    pub pcloud_callback_url: String,
    pub pcloud_auth_url: Option<String>,
    pub error_message: Option<String>,
}

impl Default for CloudConnectFormState {
    fn default() -> Self {
        Self {
            provider: CloudProvider::S3,
            display_name: String::new(),
            s3_bucket: String::new(),
            s3_region: String::new(),
            s3_endpoint_url: String::new(),
            s3_prefix: String::new(),
            pcloud_client_id: String::new(),
            pcloud_redirect_url: "http://127.0.0.1/iris-pcloud".to_string(),
            pcloud_callback_url: String::new(),
            pcloud_auth_url: None,
            error_message: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarDropTarget {
    BeforeGroup(usize),
    ItemPosition { group: usize, position: usize },
}
