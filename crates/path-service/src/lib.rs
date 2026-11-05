//! Path normalization, display paths and URL parsing.

pub mod display;
pub mod normalize;
pub mod platform;
pub mod url;

pub use display::{join_display_path, parent_relative_display_path};
pub use normalize::{
    expand_home_path, file_name_problem, normalize_local_path, normalize_relative_display_path,
    sanitize_file_name, FileNameProblem,
};
pub use platform::{is_boot_volume_path, same_filesystem};
pub use url::{
    parse_pcloud_callback, percent_decode, short_config_hash, url_encode_component,
    PCloudTokenResponse,
};
