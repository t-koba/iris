use super::*;

impl AppRuntime {
    pub(super) fn initialize() -> Arc<Self> {
        let event_bus = Arc::new(EventBus::new(1024));
        let mut registry = BackendRegistry::new();
        let local_backend_id = BackendId("local".to_string());
        let local_adapter = Arc::new(LocalFsAdapter::new(
            default_local_root(),
            local_backend_id.clone(),
        ));
        registry.register(local_backend_id, local_adapter);

        let job_store_path = runtime_data_dir().join("jobs.sqlite3");
        if let Some(parent) = job_store_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let job_store = Arc::new(
            JobStore::open(&job_store_path)
                .or_else(|_| JobStore::open_in_memory())
                .unwrap_or_else(|error| panic!("failed to open job store: {}", error)),
        );
        let job_manager = Arc::new(JobManager::new(
            Arc::clone(&job_store),
            Arc::clone(&event_bus),
            ConcurrencyLimits::default(),
        ));
        let catalog_db_path = runtime_data_dir().join("catalog.sqlite3");
        if let Some(parent) = catalog_db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let catalog_cache_dir = runtime_data_dir().join("thumbnail-cache");
        let _ = std::fs::create_dir_all(&catalog_cache_dir);
        let open_cache_dir = runtime_data_dir().join("open-cache");
        let _ = std::fs::create_dir_all(&open_cache_dir);
        let index_cache_dir = runtime_data_dir().join("index");
        let _ = std::fs::create_dir_all(&index_cache_dir);
        let tile_cache_dir = runtime_data_dir().join("map-tiles");
        let _ = std::fs::create_dir_all(&tile_cache_dir);
        #[cfg(not(test))]
        let catalog_repository = CatalogRepository::open(&catalog_db_path)
            .or_else(|_| CatalogRepository::open_in_memory())
            .unwrap_or_else(|error| panic!("failed to open catalog repository: {}", error));
        #[cfg(test)]
        let catalog_repository = CatalogRepository::open_in_memory()
            .unwrap_or_else(|error| panic!("failed to open catalog repository: {}", error));
        let catalog = Arc::new(Mutex::new(catalog_repository));

        let runtime = Arc::new(Self {
            event_bus,
            registry: Mutex::new(registry),
            registered_backends: Mutex::new(Vec::new()),
            job_store,
            job_manager,
            active_transfers: Mutex::new(HashMap::new()),
            active_transfer_progress: Mutex::new(HashMap::new()),
            catalog,
            catalog_cache_dir,
            open_cache_dir,
            index_cache_dir,
            tile_cache_dir,
            map_tile_cache_writes: AtomicU64::new(0),
            session_cache: Arc::new(
                SessionCache::new()
                    .unwrap_or_else(|error| panic!("failed to create session cache: {}", error)),
            ),
            cache_encryption_policy: Mutex::new(CacheEncryptionPolicy::Auto),
            thumbnail_tasks: Mutex::new(HashMap::new()),
            thumbnail_cache_writes: AtomicU64::new(0),
            known_remote_paths: Mutex::new(HashMap::new()),
            negative_path_cache: Mutex::new(NegativePathCache::default()),
            remote_transport_mappings: Mutex::new(Vec::new()),
        });
        runtime.refresh_mounted_backends();
        runtime
    }

    pub fn set_remote_transport_mappings(
        &self,
        mappings: Vec<crate::remote_transport::RemoteTransportMapping>,
    ) {
        *self
            .remote_transport_mappings
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = mappings;
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<DomainEvent> {
        self.event_bus.subscribe()
    }

    pub fn set_cache_encryption_policy(&self, policy: CacheEncryptionPolicy) {
        *self
            .cache_encryption_policy
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = policy;
    }

    pub fn cache_directories(&self) -> Vec<PathBuf> {
        vec![
            self.catalog_cache_dir.clone(),
            self.open_cache_dir.clone(),
            self.index_cache_dir.clone(),
            self.tile_cache_dir.clone(),
        ]
    }

    pub fn filter_index_cache_path(&self, root: &Path, show_hidden: bool) -> PathBuf {
        let root = root.to_string_lossy();
        let hidden_mode = if show_hidden {
            "include-hidden"
        } else {
            "hide-hidden"
        };
        let hash = short_config_hash(&["filter-index-v1", hidden_mode, &root]);
        self.index_cache_dir.join(format!("{hash}.idx"))
    }

    pub fn storage_directories(&self) -> Vec<(StorageCategory, PathBuf)> {
        let mut dirs = vec![
            (StorageCategory::Thumbnail, self.catalog_cache_dir.clone()),
            (StorageCategory::Thumbnail, self.open_cache_dir.clone()),
            (StorageCategory::Index, self.index_cache_dir.clone()),
            (StorageCategory::Tile, self.tile_cache_dir.clone()),
        ];
        if let Some(log_dir) = crate::observability::log_dir() {
            dirs.push((StorageCategory::Log, log_dir));
        }
        if let Some(config_dir) = crate::config::config_dir_for_storage() {
            dirs.push((StorageCategory::Quarantine, config_dir));
        }
        dirs
    }

    pub fn refresh_mounted_backends(&self) {
        let Ok(volumes) = list_mounted_volumes() else {
            return;
        };

        self.sync_mounted_backends(&volumes);
    }

    pub fn sync_mounted_backends(&self, volumes: &[MountedVolume]) {
        let active_mounts: HashSet<PathBuf> = volumes
            .iter()
            .filter(|volume| matches!(volume.kind, VolumeKind::Smb | VolumeKind::Nfs))
            .map(|volume| volume.mount_path.clone())
            .collect();

        let stale_mounts: Vec<PathBuf> = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|backend| matches!(backend.kind, VolumeKind::Smb | VolumeKind::Nfs))
            .filter(|backend| !active_mounts.contains(&backend.root_path))
            .map(|backend| backend.root_path.clone())
            .collect();

        for volume in volumes {
            self.register_volume_backend(volume);
        }

        for mount_path in stale_mounts {
            self.unregister_mount(&mount_path);
        }
    }

    pub async fn restore_cloud_backends(&self) -> Vec<CloudSidebarLocation> {
        let configs = self.load_cloud_accounts().unwrap_or_default();
        let configured_backend_ids = configs
            .iter()
            .map(cloud_account_backend_id)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let mut locations = Vec::new();
        for config in configs {
            if let Ok(location) = self.register_cloud_account(config, false).await {
                locations.push(location);
            }
        }
        let stale_cloud_roots = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|backend| is_cloud_backend_id(&backend.backend_id))
            .filter(|backend| !configured_backend_ids.contains(&backend.backend_id.0))
            .map(|backend| backend.root_path.clone())
            .collect::<Vec<_>>();
        for root_path in stale_cloud_roots {
            self.unregister_mount(&root_path);
        }
        locations
    }

    pub async fn connect_s3_account(
        &self,
        display_name: String,
        bucket: String,
        region: String,
        endpoint_url: Option<String>,
        prefix: Option<String>,
    ) -> Result<CloudSidebarLocation, String> {
        let backend_id = BackendId(format!(
            "s3:{}",
            short_config_hash(&[
                bucket.as_str(),
                region.as_str(),
                endpoint_url.as_deref().unwrap_or_default(),
                prefix.as_deref().unwrap_or_default(),
            ])
        ));
        let config = CloudAccountConfig::S3 {
            backend_id: backend_id.0,
            display_name,
            bucket,
            region,
            endpoint_url,
            prefix,
        };
        let location = self.register_cloud_account(config.clone(), true).await?;
        self.save_or_replace_cloud_account(config)?;
        Ok(location)
    }

    pub fn begin_pcloud_authorization(
        &self,
        client_id: &str,
        redirect_url: &str,
    ) -> Result<String, String> {
        if client_id.trim().is_empty() {
            return Err("pCloud client_id is required".to_string());
        }
        if redirect_url.trim().is_empty() {
            return Err("redirect URL is required".to_string());
        }
        Ok(format!(
            "https://my.pcloud.com/oauth2/authorize?response_type=token&client_id={}&redirect_uri={}",
            url_encode_component(client_id.trim()),
            url_encode_component(redirect_url.trim())
        ))
    }

    pub async fn connect_pcloud_account(
        &self,
        display_name: String,
        callback_url: String,
    ) -> Result<CloudSidebarLocation, String> {
        let token_response = parse_pcloud_callback(&callback_url)?;
        let backend_id = BackendId(format!("pcloud:{}", token_response.account_id));
        let token = TokenSet {
            access_token: token_response.access_token.clone(),
        };
        CredentialManager::new()
            .save_token(&backend_id.0, &token_response.account_id, &token)
            .map_err(|error| error.to_string())?;

        let config = CloudAccountConfig::PCloud {
            backend_id: backend_id.0,
            display_name,
            account_id: token_response.account_id,
            api_base: token_response.api_base,
        };
        let location = self.register_cloud_account(config.clone(), true).await?;
        self.save_or_replace_cloud_account(config)?;
        Ok(location)
    }

    pub async fn connect_smb_native(
        &self,
        request: SmbNativeConnectRequest,
    ) -> Result<CloudSidebarLocation, String> {
        let SmbNativeConnectRequest {
            display_name,
            server,
            port,
            share_name,
            username,
            domain,
            password,
            allow_guest,
        } = request;
        let server = server.trim().to_string();
        let share_name = share_name
            .trim()
            .trim_start_matches('/')
            .trim_end_matches('/')
            .to_string();
        if server.is_empty() {
            return Err(crate::i18n::tr("connections-server-required"));
        }
        if share_name.is_empty() {
            return Err(crate::i18n::tr("connections-share-required"));
        }

        let port_text = port.to_string();
        let backend_id = BackendId(format!(
            "smb:{}",
            short_config_hash(&[
                server.as_str(),
                port_text.as_str(),
                share_name.as_str(),
                username.as_deref().unwrap_or_default(),
                domain.as_deref().unwrap_or_default(),
            ])
        ));
        let adapter = Arc::new(SmbAdapter::new(
            SmbConnectionConfig {
                server,
                port,
                share_name: share_name.clone(),
                username,
                domain,
                password,
                workstation: None,
                allow_guest,
            },
            backend_id.clone(),
        ));
        adapter.connect().await.map_err(|error| error.to_string())?;
        let display_name = if display_name.trim().is_empty() {
            share_name
        } else {
            display_name
        };
        Ok(self.register_remote_backend(
            backend_id,
            display_name,
            StableId(String::new()),
            VolumeKind::OtherNetwork,
            adapter,
        ))
    }

    pub fn register_network_mount(
        &self,
        mount_path: &Path,
        protocol: NetworkMountProtocol,
    ) -> Result<BackendId, String> {
        let volume = MountedVolume {
            name: mount_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| mount_path.to_string_lossy().to_string()),
            mount_path: mount_path.to_path_buf(),
            ejectable: true,
            kind: match protocol {
                NetworkMountProtocol::Smb => VolumeKind::Smb,
                NetworkMountProtocol::Nfs => VolumeKind::Nfs,
            },
        };
        self.register_volume_backend(&volume)
            .ok_or_else(|| "failed to register mounted backend".to_string())
    }

    pub fn unregister_mount(&self, mount_path: &Path) {
        let mut registered_backends = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = registered_backends
            .iter()
            .position(|backend| backend.root_path == mount_path)
        {
            let backend_id = registered_backends[index].backend_id.clone();
            registered_backends.remove(index);
            self.registry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&backend_id);
        }
    }

    async fn register_cloud_account(
        &self,
        config: CloudAccountConfig,
        verify_connection: bool,
    ) -> Result<CloudSidebarLocation, String> {
        match config {
            CloudAccountConfig::S3 {
                backend_id,
                display_name,
                bucket,
                region,
                endpoint_url,
                prefix,
            } => {
                let backend_id = BackendId(backend_id);
                let adapter = Arc::new(S3Adapter::new(
                    S3Config {
                        bucket,
                        prefix: prefix.clone(),
                        endpoint_url: endpoint_url.clone(),
                        region,
                    },
                    backend_id.clone(),
                ));
                if verify_connection {
                    adapter.connect().await.map_err(|error| error.to_string())?;
                }
                Ok(self.register_remote_backend(
                    backend_id,
                    display_name,
                    StableId(prefix.unwrap_or_default()),
                    VolumeKind::OtherNetwork,
                    adapter,
                ))
            }
            CloudAccountConfig::PCloud {
                backend_id,
                display_name,
                account_id,
                api_base,
            } => {
                let backend_id = BackendId(backend_id);
                let adapter = if verify_connection {
                    let token = CredentialManager::new()
                        .load_token(&backend_id.0, &account_id)
                        .map_err(|error| error.to_string())?;
                    Arc::new(PCloudAdapter::new_with_base(
                        token.access_token,
                        backend_id.clone(),
                        api_base,
                    ))
                } else {
                    Arc::new(PCloudAdapter::new_with_stored_credentials(
                        backend_id.clone(),
                        account_id,
                        api_base,
                    ))
                };
                if verify_connection {
                    adapter.connect().await.map_err(|error| error.to_string())?;
                }
                Ok(self.register_remote_backend(
                    backend_id,
                    display_name,
                    StableId("folder:0".to_string()),
                    VolumeKind::OtherNetwork,
                    adapter,
                ))
            }
        }
    }

    fn register_remote_backend(
        &self,
        backend_id: BackendId,
        display_name: String,
        root_stable_id: StableId,
        kind: VolumeKind,
        adapter: Arc<dyn BackendAdapter>,
    ) -> CloudSidebarLocation {
        let root_path = virtual_root_path(&backend_id);
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .register(backend_id.clone(), adapter);
        let mut backends = self
            .registered_backends
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = backends
            .iter_mut()
            .find(|entry| entry.backend_id == backend_id)
        {
            existing.root_path = root_path.clone();
            existing.kind = kind;
            existing.display_name = display_name.clone();
            existing.root_stable_id = root_stable_id.clone();
        } else {
            backends.push(RegisteredBackend {
                backend_id: backend_id.clone(),
                root_path: root_path.clone(),
                kind,
                display_name: display_name.clone(),
                root_stable_id: root_stable_id.clone(),
            });
        }
        self.remember_remote_path(&backend_id, "", &root_stable_id);
        CloudSidebarLocation {
            name: display_name,
            path: root_path.to_string_lossy().to_string(),
        }
    }

    fn save_or_replace_cloud_account(&self, config: CloudAccountConfig) -> Result<(), String> {
        let mut configs = self.load_cloud_accounts().unwrap_or_default();
        let backend_id = cloud_account_backend_id(&config).to_string();
        configs.retain(|existing| cloud_account_backend_id(existing) != backend_id);
        configs.push(config);
        let path = cloud_accounts_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let json = serde_json::to_vec_pretty(&configs).map_err(|error| error.to_string())?;
        std::fs::write(path, json).map_err(|error| error.to_string())
    }

    fn load_cloud_accounts(&self) -> Result<Vec<CloudAccountConfig>, String> {
        let path = cloud_accounts_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
    }

    pub(super) fn remember_remote_path(
        &self,
        backend_id: &BackendId,
        relative_path: &str,
        stable_id: &StableId,
    ) {
        self.known_remote_paths
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(
                (backend_id.0.clone(), relative_path.to_string()),
                stable_id.clone(),
            );
    }
}
