use crate::i18n::tr;
use crate::messages::{DirectoryMessage, Message, PaneMessage};
use crate::runtime::{app_runtime, StorageCategory};
use crate::state::{AppState, PaneLoadState, StorageUsage};
use crate::update::preview::clear_preview_state;
use crate::update::thumbnail::clear_thumbnail_cache;
use iced::futures::SinkExt;
use iced::stream;
use iced::Task;
use iris_backends::localfs::stable_id::get_stable_id_from_metadata;
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::{MediaMetadata, MetadataLevel};
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_search::bloom::BloomSignature;
use iris_search::filter::{facets_for_records, verify_content_match, FilterModel, GpsFilter};
use iris_search::reconcile::reconcile_segment;
use iris_search::segment::{IndexedKind, Segment, SegmentRecord};
use iris_search::stream::{filter_segment_incremental, FilterEvent};
use iris_search::walker::{walk_local_with_cancel, WalkOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const DIRECTORY_HEAD_LIMIT: usize = 100;
const DIRECTORY_BATCH_LIMIT: usize = 500;

type DirectorySignatureRow = (
    String,
    String,
    MetadataLevel,
    Option<u64>,
    Option<SystemTime>,
);

pub fn reset_directory_state(state: &mut AppState) {
    reset_pane_directory_state(state, 0);
    clear_thumbnail_cache(state);
    clear_preview_state(state);
    state.thumbnail_generation_queue.clear();
    state.thumbnail_generation_queued.clear();
    state.thumbnail_generation_requests.clear();
}

pub fn reset_pane_directory_state(state: &mut AppState, pane_idx: usize) {
    let pane = state.pane_mut(pane_idx);
    pane.clear_entries();
    pane.metadata_hydration_inflight.clear();
    pane.selected_indices.clear();
    pane.selection_anchor_path = None;
    pane.focused_index = None;
    pane.stop_recursive_filter();
    pane.load_state = PaneLoadState::Idle;
    pane.set_filter_query(String::new());
}

pub fn directory_signature(entries: &[ListEntry]) -> Vec<DirectorySignatureRow> {
    let mut signature: Vec<DirectorySignatureRow> = entries
        .iter()
        .map(|entry| {
            (
                entry.object_ref.display_path.clone(),
                entry.object_ref.key.stable_id.0.clone(),
                entry.metadata_level,
                entry.size,
                entry.mtime,
            )
        })
        .collect();
    signature.sort_by(|a, b| a.0.cmp(&b.0));
    signature
}

/// The path a pane shows for `path`: `.` and `..` resolved lexically.
///
/// No file system access: this runs on the UI thread, and a dead network mount would block
/// `canonicalize` indefinitely. Symlinked folders keep the path they were opened through, so
/// Up returns to where the user came from.
pub fn pane_display_path(path: String) -> String {
    if app_runtime().is_virtual_display_path(&path) {
        path
    } else {
        iris_path_service::normalize_local_path(std::path::Path::new(&path))
    }
}

pub fn navigate_to_path(state: &mut AppState, path: String) -> Task<Message> {
    let canonical = pane_display_path(path);

    state.pane_mut(0).current_path = canonical.clone();
    reset_directory_state(state);
    state.pane_mut(0).load_state = PaneLoadState::LoadingInitial;
    state.pane_mut(0).scroll_y = 0.0;
    state.pane_mut(0).push_to_history(canonical.clone());
    refresh_breadcrumbs(state);

    load_directory_progressively(canonical, state.show_hidden_files)
}

pub fn refresh_breadcrumbs(state: &mut AppState) {
    let current_path = state.pane(0).current_path.clone();
    state.pane_mut(0).breadcrumb_segments = app_runtime()
        .breadcrumbs_for_display_path(&current_path)
        .into_iter()
        .map(|(name, full_path)| crate::state::BreadcrumbSegment { name, full_path })
        .collect();
}

pub fn refresh_breadcrumbs_for_pane(state: &mut AppState, pane_idx: usize) {
    let current_path = state.pane(pane_idx).current_path.clone();
    state.pane_mut(pane_idx).breadcrumb_segments = app_runtime()
        .breadcrumbs_for_display_path(&current_path)
        .into_iter()
        .map(|(name, full_path)| crate::state::BreadcrumbSegment { name, full_path })
        .collect();
}

pub async fn read_directory(path: String, show_hidden: bool) -> Result<Vec<ListEntry>, String> {
    if app_runtime().is_virtual_display_path(&path) {
        return app_runtime()
            .read_directory_entries(&path, show_hidden, None)
            .await;
    }

    let path = PathBuf::from(&path);
    let mut rd = tokio::fs::read_dir(&path)
        .await
        .map_err(|error| folder_open_error(&error))?;

    let mut entries = Vec::new();
    let backend_id = app_runtime().backend_id_for_path(&path);
    let parent_stable_id = app_runtime().stable_id_for_path(&path);
    let mut snapshot_write =
        app_runtime().begin_directory_snapshot_for_path(path.to_string_lossy().as_ref());
    let mut snapshot_batch = Vec::with_capacity(DIRECTORY_BATCH_LIMIT);
    while let Some(de) = rd.next_entry().await.map_err(|e| e.to_string())? {
        let name = de.file_name().to_string_lossy().to_string();
        let hidden = name.starts_with('.');
        let entry = build_local_l0_entry_safe(de, name, &backend_id, &parent_stable_id).await;
        snapshot_batch.push(entry.clone());
        if snapshot_batch.len() >= DIRECTORY_BATCH_LIMIT {
            if snapshot_write.as_ref().is_some_and(|write| {
                !app_runtime().append_directory_snapshot_entries(write, &snapshot_batch)
            }) {
                snapshot_write = None;
            }
            snapshot_batch.clear();
        }
        if !show_hidden && hidden {
            continue;
        }

        entries.push(entry);
    }

    if !snapshot_batch.is_empty()
        && snapshot_write.as_ref().is_some_and(|write| {
            !app_runtime().append_directory_snapshot_entries(write, &snapshot_batch)
        })
    {
        snapshot_write = None;
    }
    if let Some(write) = snapshot_write {
        let _ = app_runtime().complete_directory_snapshot(write);
    }

    Ok(entries)
}

/// What the pane shows when a local folder cannot be listed. The pane already shows the path,
/// so the common causes get a plain sentence; anything else keeps the OS message.
pub(crate) fn folder_open_error(error: &std::io::Error) -> String {
    tracing::debug!("failed to open folder: {error}");
    match error.kind() {
        std::io::ErrorKind::NotFound => tr("folder-error-not-found"),
        std::io::ErrorKind::PermissionDenied => tr("folder-error-permission"),
        std::io::ErrorKind::NotADirectory => tr("folder-error-not-a-folder"),
        _ => error.to_string(),
    }
}

pub fn recursive_filter_task(
    pane_idx: usize,
    run_id: u64,
    path: String,
    show_hidden: bool,
    model: FilterModel,
    cancellation_token: CancellationToken,
    collect_gps_points: bool,
) -> Task<Message> {
    Task::run(
        stream::channel::<Message>(
            16,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let root = PathBuf::from(&path);
                let index_cache_path = app_runtime().filter_index_cache_path(&root, show_hidden);
                let walk_token = cancellation_token.clone();
                let walk_root = root.clone();
                let walk_result = tokio::task::spawn_blocking(move || {
                    walk_local_with_cancel(
                        &walk_root,
                        &WalkOptions {
                            recursive: true,
                            include_hidden: show_hidden,
                            max_entries: None,
                        },
                        || walk_token.is_cancelled(),
                    )
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));

                let Ok(walk_result) = walk_result else {
                    let error = walk_result
                        .err()
                        .unwrap_or_else(|| "recursive filter failed".to_string());
                    let _ = output
                        .send(Message::Pane(
                            pane_idx,
                            PaneMessage::RecursiveFilterFailed { run_id, error },
                        ))
                        .await;
                    return;
                };

                if cancellation_token.is_cancelled() {
                    return;
                }
                let cache_path_for_read = index_cache_path.clone();
                let records = walk_result.records;
                let segment = tokio::task::spawn_blocking(move || {
                    reconcile_records_with_index_cache(&cache_path_for_read, records)
                })
                .await
                .unwrap_or_default();
                if cancellation_token.is_cancelled() {
                    return;
                }
                let mut records = segment.into_records();
                enrich_recursive_records_if_needed(
                    &mut records,
                    &model,
                    &cancellation_token,
                    collect_gps_points,
                )
                .await;
                if cancellation_token.is_cancelled() {
                    return;
                }
                if collect_gps_points {
                    let points = facets_for_records(&records).gps_points;
                    if output
                        .send(Message::Pane(
                            pane_idx,
                            PaneMessage::RecursiveFilterGpsPoints { run_id, points },
                        ))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                let segment = Segment::new(records);
                let cache_segment = segment.clone();
                let cache_path_for_write = index_cache_path.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    cache_segment.write_atomic(&cache_path_for_write)
                })
                .await;
                if cancellation_token.is_cancelled() {
                    return;
                }
                let filter_token = cancellation_token.clone();
                let filter_text = model.text.clone();
                let events = filter_segment_incremental(&segment, model, 128, || {
                    filter_token.is_cancelled()
                });
                let mut scanned = 0;
                let mut matched = 0;
                for event in events {
                    if cancellation_token.is_cancelled() {
                        return;
                    }
                    match event {
                        FilterEvent::Started { .. } => {}
                        FilterEvent::Hits(hits) => {
                            let items = hits
                                .into_iter()
                                .map(|hit| {
                                    let annotation = recursive_content_annotation(
                                        hit.content_candidate,
                                        hit.record,
                                        &filter_text,
                                    );
                                    (hit.record.path.clone(), annotation)
                                })
                                .collect::<Vec<_>>();
                            if cancellation_token.is_cancelled() {
                                return;
                            }
                            let entries = list_entries_for_paths_with_annotations(items).await;
                            if cancellation_token.is_cancelled() {
                                return;
                            }
                            if !entries.is_empty()
                                && output
                                    .send(Message::Pane(
                                        pane_idx,
                                        PaneMessage::RecursiveFilterBatch {
                                            run_id,
                                            entries,
                                            scanned,
                                            matched,
                                            done: false,
                                        },
                                    ))
                                    .await
                                    .is_err()
                            {
                                return;
                            }
                        }
                        FilterEvent::Progress(stats) => {
                            scanned = stats.scanned;
                            matched = stats.matched;
                            if output
                                .send(Message::Pane(
                                    pane_idx,
                                    PaneMessage::RecursiveFilterProgress {
                                        run_id,
                                        scanned,
                                        matched,
                                    },
                                ))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        FilterEvent::Finished(stats) | FilterEvent::Cancelled(stats) => {
                            scanned = stats.scanned;
                            matched = stats.matched;
                        }
                    }
                }

                let _ = output
                    .send(Message::Pane(
                        pane_idx,
                        PaneMessage::RecursiveFilterBatch {
                            run_id,
                            entries: Vec::new(),
                            scanned,
                            matched,
                            done: true,
                        },
                    ))
                    .await;
            },
        ),
        |message| message,
    )
}

fn reconcile_records_with_index_cache(cache_path: &Path, records: Vec<SegmentRecord>) -> Segment {
    let existing = Segment::read_cache(cache_path)
        .ok()
        .flatten()
        .unwrap_or_default();
    reconcile_segment(&existing, records).segment
}

async fn enrich_recursive_records_if_needed(
    records: &mut [SegmentRecord],
    model: &FilterModel,
    cancellation_token: &CancellationToken,
    collect_gps_points: bool,
) {
    let needs_pdf_content = model.content && !model.text.trim().is_empty();
    if !collect_gps_points && !filter_needs_media_metadata(model) && !needs_pdf_content {
        return;
    }

    let semaphore = Arc::new(Semaphore::new(4));
    let mut tasks = JoinSet::new();
    for (index, record) in records.iter().enumerate() {
        if cancellation_token.is_cancelled() {
            return;
        }
        if record.kind != IndexedKind::Image {
            if record.kind == IndexedKind::Pdf && model.content && !model.text.trim().is_empty() {
                let Ok(permit) = semaphore.clone().acquire_owned().await else {
                    return;
                };
                let path = PathBuf::from(record.path.clone());
                tasks.spawn_blocking(move || {
                    let _permit = permit;
                    let signature = pdf_content_signature(&path);
                    (index, None::<MediaMetadata>, signature)
                });
            }
            continue;
        }
        let Ok(permit) = semaphore.clone().acquire_owned().await else {
            return;
        };
        let path = PathBuf::from(record.path.clone());
        tasks.spawn_blocking(move || {
            let _permit = permit;
            let metadata = iris_thumb_preview::metadata::extract_image_metadata_from_path(&path);
            (index, Some(metadata.media), None::<BloomSignature>)
        });
    }

    while let Some(result) = tasks.join_next().await {
        if cancellation_token.is_cancelled() {
            tasks.abort_all();
            return;
        }
        let Ok((index, media, content_signature)) = result else {
            continue;
        };
        if let Some(record) = records.get_mut(index) {
            if let Some(media) = media {
                apply_media_metadata_to_record(record, &media);
            }
            if let Some(content_signature) = content_signature {
                record.content_signature = Some(content_signature);
            }
        }
    }
}

fn pdf_content_signature(path: &Path) -> Option<BloomSignature> {
    let text = iris_thumb_preview::pdf_text::extract_pdf_text(path, 64).text;
    (!text.trim().is_empty()).then(|| BloomSignature::from_content(&text))
}

fn filter_needs_media_metadata(model: &FilterModel) -> bool {
    model.date.is_some()
        || model.camera.is_some()
        || model.lens.is_some()
        || !matches!(model.gps, GpsFilter::Any)
}

fn apply_media_metadata_to_record(record: &mut SegmentRecord, media: &MediaMetadata) {
    record.width = media.width;
    record.height = media.height;
    record.capture_secs = media.capture_naive_secs;
    record.capture_offset_min = media.capture_offset_min;
    record.camera = media.camera_label();
    record.lens = media.lens_model.clone();
    record.gps_lat = media.gps_lat.map(|value| value as f32);
    record.gps_lon = media.gps_lon.map(|value| value as f32);
}

async fn list_entries_for_paths_with_annotations(
    items: Vec<(String, Option<String>)>,
) -> Vec<ListEntry> {
    let mut entries = Vec::with_capacity(items.len());
    for (path, annotation) in items {
        let full_path = PathBuf::from(&path);
        let name = full_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        let backend_id = app_runtime().backend_id_for_path(&full_path);
        let parent_stable_id = full_path
            .parent()
            .map(|parent| app_runtime().stable_id_for_path(parent))
            .unwrap_or_else(|| StableId(path.clone()));
        let mut entry =
            build_local_list_entry_safe(full_path, name, &backend_id, &parent_stable_id).await;
        entry.annotation = annotation;
        entries.push(entry);
    }
    entries
}

fn recursive_content_annotation(
    content_candidate: bool,
    record: &SegmentRecord,
    query: &str,
) -> Option<String> {
    if !content_candidate || record.kind != IndexedKind::Pdf {
        return None;
    }
    pdf_content_snippet(Path::new(&record.path), query)
}

fn pdf_content_snippet(path: &Path, query: &str) -> Option<String> {
    let text = iris_thumb_preview::pdf_text::extract_pdf_text(path, 64).text;
    verify_content_match(&text, query, 18).map(|snippet| snippet.snippet)
}

/// Returns the entry even when metadata fails, marked `is_inaccessible`.
async fn build_local_list_entry_safe(
    full_path: PathBuf,
    name: String,
    backend_id: &BackendId,
    parent_stable_id: &StableId,
) -> ListEntry {
    let display_path = full_path.to_string_lossy().to_string();

    // symlink_metadata describes the link itself and usually succeeds even without permission.
    let link_meta = tokio::fs::symlink_metadata(&full_path).await;

    match link_meta {
        Ok(lm) => {
            let is_symlink = lm.is_symlink();
            let symlink_target = if is_symlink {
                tokio::fs::read_link(&full_path)
                    .await
                    .ok()
                    .map(|t| t.to_string_lossy().to_string())
            } else {
                None
            };
            // Use the link itself for broken links or unreadable targets.
            let metadata = if is_symlink {
                tokio::fs::metadata(&full_path).await.unwrap_or(lm)
            } else {
                lm
            };

            let is_dir = metadata.is_dir();
            let size = if is_dir { None } else { Some(metadata.len()) };
            let mtime = metadata.modified().ok();
            let ctime = metadata.created().ok();
            let stable_id = get_stable_id_from_metadata(&full_path, &metadata)
                .unwrap_or_else(|_| StableId(display_path.clone()));
            let object_ref = ObjectRef {
                key: ObjectKey {
                    backend_id: backend_id.clone(),
                    stable_id,
                },
                parent_stable_id: Some(parent_stable_id.clone()),
                display_path,
            };
            app_runtime().cache_list_entry_metadata(&object_ref, size, mtime, None);

            ListEntry {
                metadata_level: MetadataLevel::L1,
                size,
                mtime,
                ctime,
                is_symlink,
                symlink_target,
                ..ListEntry::new(object_ref, name, is_dir)
            }
        }
        Err(_) => {
            // Metadata could not be read (e.g. permission denied).
            let stable_id = StableId(display_path.clone());
            ListEntry {
                is_inaccessible: true,
                ..ListEntry::new(
                    ObjectRef {
                        key: ObjectKey {
                            backend_id: backend_id.clone(),
                            stable_id,
                        },
                        parent_stable_id: Some(parent_stable_id.clone()),
                        display_path,
                    },
                    name,
                    false,
                )
            }
        }
    }
}

async fn build_local_l0_entry_safe(
    entry: tokio::fs::DirEntry,
    name: String,
    backend_id: &BackendId,
    parent_stable_id: &StableId,
) -> ListEntry {
    let full_path = entry.path();
    let display_path = full_path.to_string_lossy().to_string();
    let file_type = match entry.file_type().await {
        Ok(file_type) => file_type,
        Err(_) => {
            return ListEntry {
                is_inaccessible: true,
                ..ListEntry::new(
                    ObjectRef {
                        key: ObjectKey {
                            backend_id: backend_id.clone(),
                            stable_id: StableId(display_path.clone()),
                        },
                        parent_stable_id: Some(parent_stable_id.clone()),
                        display_path,
                    },
                    name,
                    false,
                )
            };
        }
    };

    let is_symlink = file_type.is_symlink();
    let is_dir = file_type.is_dir();

    ListEntry {
        is_symlink,
        ..ListEntry::new(
            ObjectRef {
                key: ObjectKey {
                    backend_id: backend_id.clone(),
                    stable_id: StableId(display_path.clone()),
                },
                parent_stable_id: Some(parent_stable_id.clone()),
                display_path,
            },
            name,
            is_dir,
        )
    }
}

pub fn load_directory_progressively(path: String, show_hidden: bool) -> Task<Message> {
    use crate::messages::PaneMessage;
    Task::run(
        stream::channel::<Message>(
            2,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let cached_entries = app_runtime().cached_directory_entries(&path, show_hidden);
                let recent_complete_snapshot = app_runtime().is_virtual_display_path(&path)
                    && app_runtime().has_recent_complete_directory_snapshot(&path);
                if !cached_entries.is_empty()
                    && output
                        .send(Message::Pane(
                            0,
                            PaneMessage::DirectoryHeadLoaded {
                                path: path.clone(),
                                entries: cached_entries.clone(),
                            },
                        ))
                        .await
                        .is_err()
                {
                    return;
                }

                if app_runtime().is_virtual_display_path(&path) {
                    if recent_complete_snapshot {
                        let _ = output
                            .send(
                                DirectoryMessage::Polled {
                                    path: path.clone(),
                                    entries: cached_entries,
                                }
                                .into(),
                            )
                            .await;
                        return;
                    }

                    match app_runtime()
                        .read_directory_entries(&path, show_hidden, None)
                        .await
                    {
                        Ok(entries) => {
                            if !entries.is_empty()
                                && output
                                    .send(Message::Pane(
                                        0,
                                        PaneMessage::DirectoryHeadLoaded {
                                            path: path.clone(),
                                            entries: entries
                                                [..entries.len().min(DIRECTORY_HEAD_LIMIT)]
                                                .to_vec(),
                                        },
                                    ))
                                    .await
                                    .is_err()
                            {
                                return;
                            }

                            let _ = output
                                .send(DirectoryMessage::Polled { path, entries }.into())
                                .await;
                            return;
                        }
                        Err(error) => {
                            let _ = output
                                .send(
                                    DirectoryMessage::LoadFailed {
                                        path: path.clone(),
                                        error: error.to_string(),
                                    }
                                    .into(),
                                )
                                .await;
                            return;
                        }
                    }
                }

                let local_path = PathBuf::from(&path);
                let mut rd = match tokio::fs::read_dir(&local_path).await {
                    Ok(rd) => rd,
                    Err(error) => {
                        let _ = output
                            .send(
                                DirectoryMessage::LoadFailed {
                                    path: path.clone(),
                                    error: format!("{}: {}", local_path.display(), error),
                                }
                                .into(),
                            )
                            .await;
                        return;
                    }
                };

                let backend_id = app_runtime().backend_id_for_path(&local_path);
                let parent_stable_id = app_runtime().stable_id_for_path(&local_path);
                let mut snapshot_write = app_runtime().begin_directory_snapshot_for_path(&path);
                let mut snapshot_batch = Vec::with_capacity(DIRECTORY_BATCH_LIMIT);
                let mut batch = Vec::with_capacity(DIRECTORY_HEAD_LIMIT);
                let mut head_sent = false;
                loop {
                    let entry = match rd.next_entry().await {
                        Ok(entry) => entry,
                        Err(error) => {
                            let _ = output
                                .send(
                                    DirectoryMessage::LoadFailed {
                                        path: path.clone(),
                                        error: error.to_string(),
                                    }
                                    .into(),
                                )
                                .await;
                            return;
                        }
                    };
                    let Some(de) = entry else {
                        break;
                    };

                    let name = de.file_name().to_string_lossy().to_string();
                    let hidden = name.starts_with('.');
                    let entry =
                        build_local_l0_entry_safe(de, name, &backend_id, &parent_stable_id).await;
                    snapshot_batch.push(entry.clone());
                    if snapshot_batch.len() >= DIRECTORY_BATCH_LIMIT {
                        if snapshot_write.as_ref().is_some_and(|write| {
                            !app_runtime().append_directory_snapshot_entries(write, &snapshot_batch)
                        }) {
                            snapshot_write = None;
                        }
                        snapshot_batch.clear();
                    }
                    if !show_hidden && hidden {
                        continue;
                    }

                    batch.push(entry);

                    let batch_limit = if head_sent {
                        DIRECTORY_BATCH_LIMIT
                    } else {
                        DIRECTORY_HEAD_LIMIT
                    };
                    if batch.len() >= batch_limit {
                        let entries = std::mem::take(&mut batch);
                        let message = if head_sent {
                            Message::Pane(
                                0,
                                PaneMessage::DirectoryBatchLoaded {
                                    path: path.clone(),
                                    entries,
                                    done: false,
                                },
                            )
                        } else {
                            head_sent = true;
                            Message::Pane(
                                0,
                                PaneMessage::DirectoryHeadLoaded {
                                    path: path.clone(),
                                    entries,
                                },
                            )
                        };
                        if output.send(message).await.is_err() {
                            return;
                        }
                        batch = Vec::with_capacity(DIRECTORY_BATCH_LIMIT);
                    }
                }

                if !snapshot_batch.is_empty()
                    && snapshot_write.as_ref().is_some_and(|write| {
                        !app_runtime().append_directory_snapshot_entries(write, &snapshot_batch)
                    })
                {
                    snapshot_write = None;
                }
                if let Some(write) = snapshot_write {
                    let _ = app_runtime().complete_directory_snapshot(write);
                }

                if !batch.is_empty() {
                    let entries = std::mem::take(&mut batch);
                    let message = if head_sent {
                        Message::Pane(
                            0,
                            PaneMessage::DirectoryBatchLoaded {
                                path: path.clone(),
                                entries,
                                done: false,
                            },
                        )
                    } else {
                        Message::Pane(
                            0,
                            PaneMessage::DirectoryHeadLoaded {
                                path: path.clone(),
                                entries,
                            },
                        )
                    };
                    if output.send(message).await.is_err() {
                        return;
                    }
                }

                let _ = output
                    .send(Message::Pane(
                        0,
                        PaneMessage::DirectoryBatchLoaded {
                            path,
                            entries: Vec::new(),
                            done: true,
                        },
                    ))
                    .await;
            },
        ),
        |message| message,
    )
}

pub fn load_secondary_directory_task(path: String, show_hidden: bool) -> Task<Message> {
    use crate::messages::PaneMessage;
    Task::run(
        stream::channel::<Message>(
            2,
            move |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                let cached_entries = app_runtime().cached_directory_entries(&path, show_hidden);
                let recent_complete_snapshot = app_runtime().is_virtual_display_path(&path)
                    && app_runtime().has_recent_complete_directory_snapshot(&path);
                if recent_complete_snapshot {
                    let _ = output
                        .send(Message::Pane(
                            1,
                            PaneMessage::DirectoryLoaded {
                                path,
                                entries: cached_entries,
                            },
                        ))
                        .await;
                    return;
                }
                if !cached_entries.is_empty()
                    && output
                        .send(Message::Pane(
                            1,
                            PaneMessage::DirectoryLoaded {
                                path: path.clone(),
                                entries: cached_entries,
                            },
                        ))
                        .await
                        .is_err()
                {
                    return;
                }
                let message = match read_directory(path.clone(), show_hidden).await {
                    Ok(entries) => Message::Pane(
                        1,
                        PaneMessage::DirectoryLoaded {
                            path: path.clone(),
                            entries,
                        },
                    ),
                    Err(error) => Message::Pane(
                        1,
                        PaneMessage::DirectoryLoadFailed {
                            path: path.clone(),
                            error,
                        },
                    ),
                };
                let _ = output.send(message).await;
            },
        ),
        |message| message,
    )
}

/// Disk usage per category of the directories the app manages.
pub async fn calculate_storage_usage() -> StorageUsage {
    let dirs = app_runtime().storage_directories();
    tokio::task::spawn_blocking(move || {
        let mut usage = StorageUsage::default();
        for (category, dir) in &dirs {
            if dir.exists() {
                let size = match category {
                    StorageCategory::Quarantine => corrupt_size_recursive(dir),
                    _ => dir_size_recursive(dir),
                };
                match category {
                    StorageCategory::Thumbnail => {
                        usage.thumbnail_bytes = usage.thumbnail_bytes.saturating_add(size);
                    }
                    StorageCategory::Index => {
                        usage.index_bytes = usage.index_bytes.saturating_add(size);
                    }
                    StorageCategory::Tile => {
                        usage.tile_bytes = usage.tile_bytes.saturating_add(size);
                    }
                    StorageCategory::Log => {
                        usage.log_bytes = usage.log_bytes.saturating_add(size);
                    }
                    StorageCategory::Quarantine => {
                        usage.quarantined_bytes = usage.quarantined_bytes.saturating_add(size);
                    }
                }
            }
        }
        usage
    })
    .await
    .unwrap_or_default()
}

fn dir_size_recursive(path: &Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata();
            if let Ok(meta) = meta {
                if meta.is_dir() {
                    size += dir_size_recursive(&entry.path());
                } else {
                    size += meta.len();
                }
            }
        }
    }
    size
}

fn corrupt_size_recursive(path: &Path) -> u64 {
    let mut size = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata();
            if let Ok(meta) = meta {
                if meta.is_dir() {
                    size += corrupt_size_recursive(&entry.path());
                } else if entry.file_name().to_string_lossy().contains(".corrupt-") {
                    size += meta.len();
                }
            }
        }
    }
    size
}

pub async fn clear_disk_cache() -> Result<(), String> {
    let dirs = app_runtime().cache_directories();
    tokio::task::spawn_blocking(move || {
        clear_disk_cache_dirs(&dirs).map_err(|error| error.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn clear_disk_cache_dirs(dirs: &[PathBuf]) -> std::io::Result<()> {
    for dir in dirs {
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

pub fn is_compact_layout(state: &AppState) -> bool {
    crate::app::resolved_layout::ResolvedLayout::resolve(state)
        .pane(state.active_pane)
        .width
        < 720.0
}

pub fn is_ultra_compact_layout(state: &AppState) -> bool {
    crate::app::resolved_layout::ResolvedLayout::resolve(state)
        .pane(state.active_pane)
        .width
        < 420.0
}

pub fn pane_available_width(state: &AppState, pane_idx: usize) -> f32 {
    crate::app::resolved_layout::ResolvedLayout::resolve(state)
        .pane(pane_idx)
        .content_width
}

pub fn grid_items_per_row_for_pane(state: &AppState, pane_idx: usize) -> usize {
    use crate::views::theme;
    let available = pane_available_width(state, pane_idx);
    let cell_with_spacing = theme::GRID_CELL_WIDTH + 8.0 + 4.0;
    let per_row = ((available + 4.0) / cell_with_spacing).floor() as usize;
    per_row.clamp(2, 20)
}

pub fn pane_under_cursor(state: &AppState, position: iced::Point) -> usize {
    if !state.dual_pane_enabled {
        return 0;
    }
    let layout = crate::app::resolved_layout::ResolvedLayout::resolve(state);
    let divider_x = layout.main_left + layout.pane(0).width;

    if position.x < divider_x {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_folder_reports_a_plain_message() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("gone");

        let error = read_directory(missing.to_string_lossy().into_owned(), false)
            .await
            .expect_err("missing folder");

        assert_eq!(error, tr("folder-error-not-found"));
    }

    #[cfg(unix)]
    #[test]
    fn pane_paths_resolve_dot_segments_but_keep_symlinked_folders() {
        let temp = tempfile::tempdir().expect("tempdir");
        let real = temp.path().join("real");
        std::fs::create_dir(&real).expect("create real");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("create link");

        let through_link = link.join("sub").join("..");
        assert_eq!(
            pane_display_path(through_link.to_string_lossy().into_owned()),
            link.to_string_lossy()
        );
        // Nothing is read from disk: a path that does not exist normalizes the same way.
        assert_eq!(
            pane_display_path("/no/such/./dir/../folder".to_string()),
            "/no/such/folder"
        );
    }
    use iris_search::filter::GpsBounds;

    #[tokio::test]
    async fn read_directory_snapshots_hidden_entries_independently_of_current_filter() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("visible.txt"), b"visible").expect("visible file");
        std::fs::write(temp.path().join(".hidden.txt"), b"hidden").expect("hidden file");
        let root = temp.path().to_string_lossy().to_string();

        let visible = read_directory(root.clone(), false)
            .await
            .expect("read directory");
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "visible.txt");

        let cached = app_runtime().cached_directory_entries(&root, true);
        assert_eq!(cached.len(), 2);
        assert!(cached.iter().any(|entry| entry.name == ".hidden.txt"));
    }

    #[tokio::test]
    async fn read_directory_keeps_all_entries_across_progressive_batch_boundaries() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..500 {
            std::fs::File::create(temp.path().join(format!("file-{index:03}.txt")))
                .expect("create fixture");
        }
        let cjk_name = "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{9577}\u{3044}\
                        \u{30d5}\u{30a1}\u{30a4}\u{30eb}\u{540d}.txt";
        std::fs::File::create(temp.path().join(cjk_name)).expect("create CJK fixture");

        let entries = read_directory(temp.path().to_string_lossy().to_string(), false)
            .await
            .expect("read directory");

        assert_eq!(entries.len(), 501);
        assert!(entries.iter().any(|entry| entry.name == cjk_name));
    }

    #[test]
    fn media_metadata_need_includes_all_gps_predicates_except_any() {
        let mut model = FilterModel::default();
        assert!(!filter_needs_media_metadata(&model));

        model.gps = GpsFilter::Absent;
        assert!(filter_needs_media_metadata(&model));

        model.gps = GpsFilter::Bounds(GpsBounds {
            min_lat: 35.0,
            max_lat: 36.0,
            min_lon: 138.0,
            max_lon: 140.0,
        });
        assert!(filter_needs_media_metadata(&model));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_l0_entry_does_not_stat_symlink_target_kind() {
        let temp = tempfile::tempdir().expect("tempdir");
        tokio::fs::create_dir(temp.path().join("target-dir"))
            .await
            .expect("target dir");
        std::os::unix::fs::symlink("target-dir", temp.path().join("link-to-dir")).expect("symlink");

        let mut read_dir = tokio::fs::read_dir(temp.path()).await.expect("read dir");
        let mut symlink_entry = None;
        while let Some(entry) = read_dir.next_entry().await.expect("next entry") {
            if entry.file_name().to_string_lossy() == "link-to-dir" {
                symlink_entry = Some(entry);
                break;
            }
        }

        let entry = build_local_l0_entry_safe(
            symlink_entry.expect("symlink entry"),
            "link-to-dir".to_string(),
            &BackendId("local".to_string()),
            &StableId("parent".to_string()),
        )
        .await;

        assert!(entry.is_symlink);
        assert!(!entry.is_dir);
        assert_eq!(entry.metadata_level, MetadataLevel::L0);
        assert!(entry.metadata_pending());
    }

    #[test]
    fn media_metadata_updates_segment_record_filter_fields() {
        let mut record = SegmentRecord {
            path: "/tmp/photo.jpg".to_string(),
            name: "photo.jpg".to_string(),
            size: 10,
            mtime_secs: 1,
            kind: IndexedKind::Image,
            width: None,
            height: None,
            capture_secs: None,
            capture_offset_min: None,
            camera: None,
            lens: None,
            gps_lat: None,
            gps_lon: None,
            content_signature: None,
        };
        let media = MediaMetadata {
            width: Some(4000),
            height: Some(3000),
            capture_naive_secs: Some(1_700_000_000),
            capture_offset_min: Some(540),
            camera_make: Some("Canon".to_string()),
            camera_model: Some("Canon EOS R5".to_string()),
            lens_model: Some("RF 35mm".to_string()),
            gps_lat: Some(35.681236),
            gps_lon: Some(139.767125),
        };

        apply_media_metadata_to_record(&mut record, &media);

        assert_eq!(record.width, Some(4000));
        assert_eq!(record.height, Some(3000));
        assert_eq!(record.capture_secs, Some(1_700_000_000));
        assert_eq!(record.capture_offset_min, Some(540));
        assert_eq!(record.camera.as_deref(), Some("Canon EOS R5"));
        assert_eq!(record.lens.as_deref(), Some("RF 35mm"));
        assert_eq!(record.gps_lat, Some(35.681236_f32));
        assert_eq!(record.gps_lon, Some(139.767_12_f32));
    }

    #[test]
    fn clear_disk_cache_dirs_removes_all_cache_categories_and_recreates_dirs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let thumbnail_dir = temp.path().join("thumbnails");
        let index_dir = temp.path().join("index");
        let tile_dir = temp.path().join("tiles");
        std::fs::create_dir_all(&thumbnail_dir).expect("thumbnail dir");
        std::fs::create_dir_all(&index_dir).expect("index dir");
        std::fs::create_dir_all(&tile_dir).expect("tile dir");
        std::fs::write(thumbnail_dir.join("thumb.bin"), b"thumb").expect("thumb");
        std::fs::write(index_dir.join("root.idx"), b"index").expect("index");
        std::fs::write(tile_dir.join("8-227-100.png"), b"tile").expect("tile");

        clear_disk_cache_dirs(&[thumbnail_dir.clone(), index_dir.clone(), tile_dir.clone()])
            .expect("clear cache dirs");

        for dir in [thumbnail_dir, index_dir, tile_dir] {
            assert!(dir.is_dir());
            assert_eq!(
                std::fs::read_dir(&dir).expect("read dir").count(),
                0,
                "{} should be empty",
                dir.display()
            );
        }
    }

    #[test]
    fn reconcile_records_with_index_cache_reuses_fresh_content_signature() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_path = temp.path().join("root.idx");
        let mut cached = SegmentRecord {
            path: temp.path().join("doc.pdf").to_string_lossy().to_string(),
            name: "doc.pdf".to_string(),
            size: 42,
            mtime_secs: 100,
            kind: IndexedKind::Pdf,
            width: None,
            height: None,
            capture_secs: None,
            capture_offset_min: None,
            camera: None,
            lens: None,
            gps_lat: None,
            gps_lon: None,
            content_signature: Some(BloomSignature::from_content("invoice total")),
        };
        Segment::new(vec![cached.clone()])
            .write_atomic(&cache_path)
            .expect("write index cache");
        cached.content_signature = None;

        let segment = reconcile_records_with_index_cache(&cache_path, vec![cached]);

        assert!(segment.records()[0]
            .content_signature
            .as_ref()
            .is_some_and(|signature| signature.might_contain_query("invoice")));
    }

    #[test]
    fn pdf_content_signature_reads_text_layer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("doc.pdf");
        std::fs::write(
            &path,
            b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Contents 4 0 R >> endobj
4 0 obj << /Length 33 >> stream
BT (invoice total) Tj ET
endstream endobj
%%EOF",
        )
        .expect("write pdf");

        let signature = pdf_content_signature(&path).expect("signature");

        assert!(signature.might_contain_query("invoice"));
    }

    #[test]
    fn pdf_content_snippet_verifies_text_layer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("doc.pdf");
        std::fs::write(
            &path,
            b"%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Contents 4 0 R >> endobj
4 0 obj << /Length 46 >> stream
BT (invoice total due on Friday) Tj ET
endstream endobj
%%EOF",
        )
        .expect("write pdf");

        let snippet = pdf_content_snippet(&path, "total due").expect("snippet");

        assert!(snippet.contains("total due"));
    }
}
