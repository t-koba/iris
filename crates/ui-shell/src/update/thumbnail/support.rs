use super::*;
use iced::widget::image as iced_image;
use iris_core_domain::media::MediaFormat;
use tokio_util::sync::CancellationToken;

pub(super) enum ThumbnailTaskOutcome {
    Ready(ThumbnailData),
    Unsupported,
    Failed,
}

pub(super) fn backend_type_for_object(
    object_ref: &iris_core_domain::types::object::ObjectRef,
) -> String {
    backend_type_from_backend_id(&object_ref.key.backend_id)
}

pub(super) fn preview_throttled_thumbnail_backend(state: &AppState) -> Option<String> {
    if !state.preview_loading {
        return None;
    }
    let path = state.preview_target_path.as_deref()?;
    let backend_type =
        backend_type_from_backend_id(&app_runtime().backend_id_for_path(Path::new(path)));
    is_network_backend_type(&backend_type).then_some(backend_type)
}

pub(super) fn backend_type_from_backend_id(
    backend_id: &iris_core_domain::types::object::BackendId,
) -> String {
    backend_id
        .0
        .split(':')
        .next()
        .unwrap_or(backend_id.0.as_str())
        .to_string()
}

pub(super) fn is_network_backend_type(backend_type: &str) -> bool {
    matches!(backend_type, "smb" | "nfs" | "s3" | "pcloud")
}

pub(super) async fn generate_one_thumbnail(
    object_ref: iris_core_domain::types::object::ObjectRef,
    content_type: Option<String>,
) -> ThumbnailTaskOutcome {
    let task_path = object_ref.display_path.clone();
    let backend_type = backend_type_for_object(&object_ref);
    let started_at = std::time::Instant::now();
    let cancel_token = CancellationToken::new();
    let task = tokio::spawn({
        let object_ref = object_ref.clone();
        let content_type = content_type.clone();
        let cancel_token = cancel_token.clone();
        async move {
            let hint = ThumbnailHint {
                max_dimension: THUMB_SIZE,
                preferred_formats: vec![ImageFormat::Jpeg, ImageFormat::WebP, ImageFormat::Png],
            };
            app_runtime()
                .generate_thumbnail(&object_ref, &hint, content_type.as_deref(), &cancel_token)
                .await
        }
    });
    app_runtime().track_thumbnail_task(&task_path, task.abort_handle(), cancel_token);

    let generated = match task.await {
        Ok(Ok(generated)) => generated,
        Ok(Err(crate::runtime::ThumbnailGenerationError::Unsupported(_))) => {
            app_runtime().finish_thumbnail_task(&task_path);
            iris_thumb_preview::scheduler::observe_thumbnail_latency(
                &backend_type,
                started_at.elapsed(),
            );
            return ThumbnailTaskOutcome::Unsupported;
        }
        _ => {
            app_runtime().finish_thumbnail_task(&task_path);
            iris_thumb_preview::scheduler::observe_thumbnail_latency(
                &backend_type,
                started_at.elapsed(),
            );
            return ThumbnailTaskOutcome::Failed;
        }
    };
    app_runtime().finish_thumbnail_task(&task_path);
    iris_thumb_preview::scheduler::observe_thumbnail_latency(&backend_type, started_at.elapsed());

    let encoded_len = generated.data.len();
    let decoded_len = (generated.width as usize)
        .saturating_mul(generated.height as usize)
        .saturating_mul(4);
    let memory_len = encoded_len.max(decoded_len);
    let handle = iced_image::Handle::from_bytes(generated.data);
    ThumbnailTaskOutcome::Ready(ThumbnailData { handle, memory_len })
}

pub(super) fn thumbnail_priority_rank(path: &str) -> u8 {
    match MediaFormat::from_path(path) {
        Some(MediaFormat::Jpeg | MediaFormat::Image) => 0,
        Some(MediaFormat::Raw) => 1,
        _ => 2,
    }
}

/// Slow formats that are fetched only after everything visible.
pub(super) fn is_background_media_path(path: &str) -> bool {
    matches!(
        MediaFormat::from_path(path),
        Some(MediaFormat::Raw | MediaFormat::ModernImage | MediaFormat::Pdf | MediaFormat::Video)
    )
}
