use super::*;

impl AppRuntime {
    pub(super) async fn ensure_directory_exists(
        &self,
        resolved: &ResolvedBackend,
        path: &Path,
    ) -> Result<(), String> {
        if self.path_exists(path).await? {
            return Ok(());
        }

        if resolved.kind == VolumeKind::Local
            && !self.is_virtual_display_path(path.to_string_lossy().as_ref())
        {
            return ensure_directory_exists(resolved, path).await;
        }

        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        let parent_ref = self.object_ref_for_display_path(parent, true)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} has no final component", path.display()))?;
        let created = resolved
            .adapter
            .mkdir(&parent_ref, name)
            .await
            .map_err(|error| error.to_string())?;
        self.remember_remote_path(
            &resolved.backend_id,
            &created.display_path,
            &created.key.stable_id,
        );
        Ok(())
    }

    pub(super) async fn verify_directory_copy(
        &self,
        src: &ObjectRef,
        src_resolved: &ResolvedBackend,
        dst: &ObjectRef,
        dst_resolved: &ResolvedBackend,
        high_risk_path: bool,
    ) -> Result<(), String> {
        let src_children = self.list_directory_children(src, src_resolved).await?;
        let dst_children = self.list_directory_children(dst, dst_resolved).await?;
        if src_children.len() != dst_children.len() {
            return Err(format!(
                "verify failed: directory child count mismatch src={} dst={}",
                src.display_path, dst.display_path
            ));
        }

        let dst_by_name = dst_children
            .into_iter()
            .map(|entry| (entry.name.clone(), entry))
            .collect::<HashMap<_, _>>();

        for src_child in src_children {
            let Some(dst_child) = dst_by_name.get(&src_child.name) else {
                return Err(format!(
                    "verify failed: missing destination child '{}' under {}",
                    src_child.name, dst.display_path
                ));
            };
            if src_child.is_dir != dst_child.is_dir {
                return Err(format!(
                    "verify failed: type mismatch for child '{}' src={} dst={}",
                    src_child.name, src.display_path, dst.display_path
                ));
            }
            if src_child.is_dir {
                Box::pin(self.verify_directory_copy(
                    &src_child.object_ref,
                    src_resolved,
                    &dst_child.object_ref,
                    dst_resolved,
                    high_risk_path,
                ))
                .await?;
            } else {
                let src_ref = self.source_object_ref(&src_child.object_ref, src_resolved)?;
                let dst_ref = self.source_object_ref(&dst_child.object_ref, dst_resolved)?;
                verify_object_copy(
                    src_resolved.adapter.as_ref(),
                    &src_ref,
                    dst_resolved.adapter.as_ref(),
                    &dst_ref,
                    high_risk_path,
                )
                .await?;
            }
        }

        Ok(())
    }
}

/// Runs VerifyLight and escalates to VerifyStrong when required.
pub(super) async fn verify_object_copy(
    src_backend: &dyn BackendAdapter,
    src: &ObjectRef,
    dst_backend: &dyn BackendAdapter,
    dst: &ObjectRef,
    high_risk_path: bool,
) -> Result<(), String> {
    let light = iris_transfer::verify::verify_light(src_backend, src, dst_backend, dst)
        .await
        .map_err(|error| error.to_string())?;
    let must_strong = iris_transfer::verify::should_escalate_to_strong(
        &src.key.backend_id.0,
        &dst.key.backend_id.0,
        &light,
        false,
        high_risk_path,
    );
    match light {
        iris_transfer::verify::VerifyResult::Match => {}
        iris_transfer::verify::VerifyResult::Mismatch { reason } => {
            return Err(format!("verify light failed: {}", reason));
        }
    }
    if must_strong {
        match iris_transfer::verify::verify_strong(src_backend, src, dst_backend, dst)
            .await
            .map_err(|error| error.to_string())?
        {
            iris_transfer::verify::VerifyResult::Match => {}
            iris_transfer::verify::VerifyResult::Mismatch { reason } => {
                return Err(format!("verify strong failed: {}", reason));
            }
        }
    }
    Ok(())
}
