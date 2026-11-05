use super::*;
use iced::widget::image as iced_image;
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
                .generate_thumbnail_detailed_with_cancellation(
                    &object_ref,
                    &hint,
                    content_type.as_deref(),
                    &cancel_token,
                )
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
    let lower = path.to_lowercase();
    if is_lightweight_image_path(&lower) {
        0
    } else if is_raw_path(&lower) {
        1
    } else {
        2
    }
}

pub(super) fn is_background_media_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    is_raw_path(&lower)
        || is_modern_image_path(&lower)
        || lower.ends_with(".pdf")
        || is_video_path(&lower)
}

fn is_modern_image_path(lower_path: &str) -> bool {
    [
        ".avif", ".heic", ".heif", ".hif", ".jp2", ".j2k", ".j2c", ".jpc", ".jpf", ".jpx", ".jpm",
        ".jxr", ".hdp", ".wdp", ".jxl",
    ]
    .iter()
    .any(|extension| lower_path.ends_with(extension))
}

pub(super) fn is_lightweight_image_path(lower_path: &str) -> bool {
    lower_path.ends_with(".jpg")
        || lower_path.ends_with(".jpeg")
        || lower_path.ends_with(".png")
        || lower_path.ends_with(".webp")
        || lower_path.ends_with(".gif")
        || lower_path.ends_with(".bmp")
        || lower_path.ends_with(".tif")
        || lower_path.ends_with(".tiff")
}

pub(super) fn is_raw_path(lower_path: &str) -> bool {
    lower_path.ends_with(".arw")
        || lower_path.ends_with(".cr2")
        || lower_path.ends_with(".cr3")
        || lower_path.ends_with(".dng")
        || lower_path.ends_with(".nef")
        || lower_path.ends_with(".orf")
        || lower_path.ends_with(".raf")
        || lower_path.ends_with(".rw2")
        || lower_path.ends_with(".x3f")
}

pub(super) fn is_video_path(lower_path: &str) -> bool {
    lower_path.ends_with(".mp4")
        || lower_path.ends_with(".mov")
        || lower_path.ends_with(".mkv")
        || lower_path.ends_with(".avi")
        || lower_path.ends_with(".mj2")
}
