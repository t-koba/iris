use iris_core_domain::backend_adapter::BackendAdapter;
use iris_core_domain::types::object::BackendId;
use std::collections::HashMap;
use std::sync::Arc;

/// Adapters keyed by backend id.
pub struct BackendRegistry {
    adapters: HashMap<String, Arc<dyn BackendAdapter>>,
}

impl Default for BackendRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    pub fn register(&mut self, id: BackendId, adapter: Arc<dyn BackendAdapter>) {
        self.adapters.insert(id.0, adapter);
    }

    pub fn get(&self, id: &BackendId) -> Option<Arc<dyn BackendAdapter>> {
        self.adapters.get(&id.0).cloned()
    }

    pub fn remove(&mut self, id: &BackendId) -> Option<Arc<dyn BackendAdapter>> {
        self.adapters.remove(&id.0)
    }
}
