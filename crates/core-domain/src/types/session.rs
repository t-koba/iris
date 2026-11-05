use crate::types::object::BackendId;

#[derive(Clone, Debug)]
pub struct BackendSession {
    pub backend_id: BackendId,
    pub display_name: String,
}
