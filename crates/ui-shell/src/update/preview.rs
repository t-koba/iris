use crate::messages::Message;
use crate::runtime::app_runtime;
use crate::state::{AppState, EntryCacheStamp, FolderStats, PreviewData};
use iced::widget::image as iced_image;
use iced::Task;
use iris_core_domain::types::object::ObjectRef;
use iris_core_domain::types::thumbnail::{ImageFormat, ThumbnailHint};
use std::path::PathBuf;

const PREVIEW_JPEG_MAX_DIMENSION: u32 = 1280;
const PREVIEW_RAW_MAX_DIMENSION: u32 = 2048;
const PREVIEW_VIDEO_MAX_DIMENSION: u32 = 640;
const PREVIEW_DEBOUNCE_MS: u64 = 150;

#[derive(Clone, Copy)]
struct PreviewLoadMode {
    wants_visual: bool,
    wants_metadata: bool,
}

fn preview_load_mode(state: &AppState) -> Option<PreviewLoadMode> {
    let wants_visual = state.preview_panel_visible || state.quick_look_open;
    let wants_metadata = state.properties_panel_visible;

    if wants_visual || wants_metadata {
        Some(PreviewLoadMode {
            wants_visual,
            wants_metadata,
        })
    } else {
        None
    }
}

pub fn clear_preview_state(state: &mut AppState) {
    state.preview_data = None;
    state.preview_target_path = None;
    state.preview_target_stamp = None;
    state.preview_request_version = state.preview_request_version.wrapping_add(1);
    state.preview_loading = false;
    state.preview_failed = false;
}

/// Called on selection change; starts a debounce timer instead of loading immediately.
pub fn refresh_preview_for_selection(state: &mut AppState) -> Task<Message> {
    let should_clear = if state.active_pane().selected_indices.len() != 1 {
        true
    } else {
        let index = state
            .active_pane()
            .selected_indices
            .iter()
            .copied()
            .next()
            .unwrap_or(0);
        state
            .active_pane()
            .entry(index)
            .map(|e| e.is_dir)
            .unwrap_or(true)
    };

    // For a selected folder, also fetch shallow folder statistics.
    let folder_stats_task = {
        let pane = state.active_pane();
        let focused_entry = pane
            .selected_indices
            .iter()
            .copied()
            .next()
            .and_then(|i| pane.entry(i));
        if let Some(entry) = focused_entry {
            if entry.is_dir {
                let folder_path = format!("{}/{}", pane.current_path, entry.name);
                if state.focused_folder_stats_path.as_deref() != Some(&folder_path) {
                    state.focused_folder_stats = None;
                    state.focused_folder_stats_path = Some(folder_path.clone());
                    let path_for_msg = folder_path.clone();
                    Task::perform(compute_shallow_folder_stats(folder_path), move |stats| {
                        Message::FolderStatsLoaded(path_for_msg, stats)
                    })
                } else {
                    Task::none()
                }
            } else {
                state.focused_folder_stats = None;
                state.focused_folder_stats_path = None;
                Task::none()
            }
        } else {
            state.focused_folder_stats = None;
            state.focused_folder_stats_path = None;
            Task::none()
        }
    };

    if preview_load_mode(state).is_none() {
        clear_preview_state(state);
        return folder_stats_task;
    }

    if should_clear {
        clear_preview_state(state);
        return folder_stats_task;
    }

    let Some(index) = state.active_pane().selected_indices.iter().copied().next() else {
        return folder_stats_task;
    };
    let Some(entry) = state.active_pane().entry(index) else {
        return Task::none();
    };
    let path = entry.object_ref.display_path.clone();
    let stamp = EntryCacheStamp::from_entry(entry);

    if state.preview_target_path.as_ref() == Some(&path)
        && state.preview_target_stamp.as_ref() == Some(&stamp)
        && !state.preview_failed
        && (state.preview_loading || state.preview_data.is_some())
    {
        return Task::none();
    }

    state.preview_request_version = state.preview_request_version.wrapping_add(1);
    let version = state.preview_request_version;
    state.preview_target_path = Some(path);
    state.preview_target_stamp = Some(stamp);
    state.preview_loading = true;
    state.preview_data = None;
    state.preview_failed = false;

    Task::perform(
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(PREVIEW_DEBOUNCE_MS)).await;
            version
        },
        Message::PreviewDebounced,
    )
}

/// Starts the load after the debounce if the version still matches.
pub fn start_preview_load(state: &mut AppState, version: u64) -> Task<Message> {
    if state.preview_request_version != version {
        return Task::none();
    }

    let Some(mode) = preview_load_mode(state) else {
        return Task::none();
    };

    let Some(path) = state.preview_target_path.clone() else {
        return Task::none();
    };

    let preview_entry = state
        .active_pane()
        .entry_by_display_path(&path)
        .cloned()
        .or_else(|| {
            let other = 1 - state.active_pane;
            state.pane(other).entry_by_display_path(&path).cloned()
        });
    let Some(preview_entry) = preview_entry else {
        return Task::none();
    };

    let cached_thumbnail = state
        .thumbnail_cache
        .get(&crate::state::ThumbnailCacheKey::from_entry(&preview_entry))
        .map(|thumbnail| thumbnail.handle.clone());

    Task::perform(
        load_preview_data(preview_entry.object_ref, mode, cached_thumbnail),
        move |data| Message::PreviewLoaded(path, data),
    )
}

async fn load_preview_visual(
    object_ref: &ObjectRef,
    ext: &str,
    cached_thumbnail: Option<iced_image::Handle>,
) -> Option<iced_image::Handle> {
    if is_video_extension(ext) {
        if let Some(cached_thumbnail) = cached_thumbnail {
            return Some(cached_thumbnail);
        }
        if !iris_thumb_preview::video::supports_video_thumbnails() {
            return None;
        }
    }

    let path = &object_ref.display_path;
    let hint = ThumbnailHint {
        max_dimension: if is_raw_extension(ext) {
            PREVIEW_RAW_MAX_DIMENSION
        } else if is_video_extension(ext) {
            PREVIEW_VIDEO_MAX_DIMENSION
        } else {
            PREVIEW_JPEG_MAX_DIMENSION
        },
        preferred_formats: vec![ImageFormat::Jpeg, ImageFormat::WebP, ImageFormat::Png],
    };
    let content_type = mime_guess::from_path(path)
        .first()
        .map(|mime| mime.to_string());
    let generated = app_runtime()
        .generate_thumbnail(object_ref, &hint, content_type.as_deref())
        .await
        .ok()?;

    Some(iced_image::Handle::from_bytes(generated.data))
}

async fn load_preview_data(
    object_ref: ObjectRef,
    mode: PreviewLoadMode,
    cached_thumbnail: Option<iced_image::Handle>,
) -> Option<PreviewData> {
    let path = object_ref.display_path.clone();
    let ext = PathBuf::from(&path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if is_jpeg_extension(&ext) || is_image_extension(&ext) || is_raw_extension(&ext) {
        let handle = if mode.wants_visual {
            load_preview_visual(&object_ref, &ext, None).await
        } else {
            None
        };
        let metadata = if mode.wants_metadata {
            app_runtime().extract_image_metadata(&object_ref).await.ok()
        } else {
            None
        };

        if handle.is_none() && metadata.is_none() {
            return None;
        }

        let (width, height, media, exif_data) = if let Some(metadata) = metadata {
            (
                metadata.width,
                metadata.height,
                metadata.media,
                metadata.exif_entries,
            )
        } else {
            (None, None, Default::default(), Vec::new())
        };

        return Some(PreviewData::Image {
            handle,
            width,
            height,
            media,
            exif_data,
        });
    }

    if ext == "pdf" {
        let thumbnail = if mode.wants_visual {
            load_preview_visual(&object_ref, &ext, None).await
        } else {
            None
        };
        let page_count = if mode.wants_metadata && object_ref.key.backend_id.0.starts_with("local")
        {
            let p = PathBuf::from(&path);
            tokio::task::spawn_blocking(move || iris_thumb_preview::pdf::pdf_page_count(&p))
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        if thumbnail.is_none() && page_count.is_none() {
            return None;
        }

        return Some(PreviewData::Pdf {
            thumbnail,
            page_count,
        });
    }

    if is_video_extension(&ext) {
        if !mode.wants_visual {
            return None;
        }

        return Some(PreviewData::Video {
            thumbnail: load_preview_visual(&object_ref, &ext, cached_thumbnail).await,
            duration_secs: None,
        });
    }

    if is_text_extension(&ext) {
        let bytes = app_runtime()
            .read_object_prefix(&object_ref, 32 * 1024)
            .await
            .ok()?;
        let content = String::from_utf8_lossy(&bytes).to_string();
        return Some(PreviewData::Text { content });
    }

    if !mode.wants_visual {
        return None;
    }

    let file_type = if ext.is_empty() {
        crate::i18n::tr("preview-unknown-file-type")
    } else {
        ext.to_uppercase()
    };
    let metadata = app_runtime().stat_object(&object_ref).await.ok()?;
    Some(PreviewData::Unknown {
        file_type,
        size: metadata.size.unwrap_or(0),
    })
}

pub fn is_text_extension(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "md"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "rs"
            | "js"
            | "ts"
            | "tsx"
            | "css"
            | "html"
            | "csv"
            | "log"
    )
}

pub fn is_image_extension(ext: &str) -> bool {
    matches!(
        ext,
        "jpg"
            | "jpeg"
            | "png"
            | "gif"
            | "bmp"
            | "webp"
            | "tiff"
            | "tif"
            | "heic"
            | "heif"
            | "hif"
            | "avif"
            | "jp2"
            | "j2k"
            | "j2c"
            | "jpc"
            | "jpf"
            | "jpx"
            | "jpm"
            | "jxr"
            | "hdp"
            | "wdp"
            | "jxl"
    )
}

pub fn is_jpeg_extension(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg")
}

pub fn is_raw_extension(ext: &str) -> bool {
    iris_thumb_preview::raw::is_known_raw_extension(ext)
}

pub fn is_video_extension(ext: &str) -> bool {
    matches!(ext, "mp4" | "mov" | "mkv" | "avi" | "mj2")
}

/// File count, folder count and total size of the direct children.
async fn compute_shallow_folder_stats(path: String) -> FolderStats {
    tokio::task::spawn_blocking(move || {
        let mut file_count = 0usize;
        let mut dir_count = 0usize;
        let mut total_size = 0u64;
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        dir_count += 1;
                    } else {
                        file_count += 1;
                        total_size += meta.len();
                    }
                }
            }
        }
        FolderStats {
            file_count,
            dir_count,
            total_size,
        }
    })
    .await
    .unwrap_or(FolderStats {
        file_count: 0,
        dir_count: 0,
        total_size: 0,
    })
}
