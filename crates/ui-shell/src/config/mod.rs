//! Durable application configuration.
//!
//! On disk the configuration is split into app settings, layout state and remote connections;
//! in memory it is the single `LayoutConfig`.

mod io;
mod model;

pub use io::{
    config_dir_for_storage, delete_remote_transport_secret, load_config, parse_config_files,
    save_config,
};
pub use model::{
    LayoutConfig, LocaleChoice, PaneTabConfig, RemoteTransportSecretWarning, ShortcutOverride,
    SidebarGroupConfig, SidebarItemConfig, ThemeChoice,
};

#[cfg(test)]
mod tests;
