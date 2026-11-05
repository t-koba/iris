use iced::advanced::clipboard;
use iced::advanced::renderer;
use iced::{mouse, Event, Font, Pixels, Point, Size, Theme};
use iced_runtime::user_interface::{self, UserInterface};
use iris_core_domain::types::list::ListEntry;
use iris_core_domain::types::metadata::MetadataLevel;
use iris_core_domain::types::object::{BackendId, ObjectKey, ObjectRef, StableId};
use iris_ui_shell::app;
use iris_ui_shell::state::{AppState, PaneState, ViewMode};
use std::time::{Duration, Instant, SystemTime};

const ENTRY_COUNT: usize = 100_000;
const WARMUP_FRAMES: usize = 30;
const MEASURED_FRAMES: usize = 240;
const LAYOUT_SCROLL_BUDGET: Duration = Duration::from_micros(16_700);
const SOFTWARE_FRAME_BUDGET: Duration = Duration::from_millis(50);
const VIEWPORT: Size = Size::new(1280.0, 800.0);
const SIMULATED_SCROLL_VELOCITY: f32 = 3_600.0;

fn object_ref(display_path: &str) -> ObjectRef {
    ObjectRef {
        key: ObjectKey {
            backend_id: BackendId("perf-local".to_string()),
            stable_id: StableId(display_path.to_string()),
        },
        parent_stable_id: None,
        display_path: display_path.to_string(),
    }
}

fn list_entry(index: usize) -> ListEntry {
    let name = format!("asset-{index:06}.jpg");
    let display_path = format!("/Users/test/Pictures/{name}");
    ListEntry {
        metadata_level: MetadataLevel::L1,
        size: Some(256 * 1024 + index as u64),
        mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        ctime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_900_000)),
        ..ListEntry::new(object_ref(&display_path), name, false)
    }
}

fn pane_with_entries() -> PaneState {
    let mut pane = PaneState::default();
    pane.current_path = "/Users/test/Pictures".to_string();
    pane.entries = (0..ENTRY_COUNT).map(list_entry).collect();
    pane.rebuild_visible_indices();
    pane.navigation_history.push(pane.current_path.clone());
    pane.history_index = pane.navigation_history.len();
    pane.rebuild_breadcrumbs();
    pane
}

fn state_with_entries(view_mode: ViewMode) -> AppState {
    let mut state = AppState::default();
    state.sidebar_visible = false;
    state.preview_panel_visible = false;
    state.properties_panel_visible = false;
    state.dual_pane_enabled = false;
    *state.pane_mut(0) = pane_with_entries();
    state.pane_mut(0).view_mode = view_mode;
    state
}

fn render_scroll_frame(
    state: &mut AppState,
    renderer: &mut iced::Renderer,
    cache: user_interface::Cache,
) -> (user_interface::Cache, usize, Duration, Duration) {
    let layout_started = Instant::now();
    let mut ui = UserInterface::build(app::view_for_test(state), VIEWPORT, cache, renderer);
    let mut clipboard = clipboard::Null;
    let mut messages = Vec::new();
    let cursor = mouse::Cursor::Available(Point::new(640.0, 400.0));
    let events = [Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -60.0 },
    })];

    let _ = ui.update(&events, cursor, renderer, &mut clipboard, &mut messages);
    let layout_scroll_elapsed = layout_started.elapsed();
    let draw_started = Instant::now();
    ui.draw(renderer, &Theme::Dark, &renderer::Style::default(), cursor);
    let draw_elapsed = draw_started.elapsed();
    let cache = ui.into_cache();
    let message_count = messages.len();
    let state_update_started = Instant::now();
    for message in messages {
        drop(iris_ui_shell::update::update(state, message));
    }
    // The tight benchmark loop has no compositor wait. Keep the next frame's
    // velocity-dependent overscan equivalent to 60 px per 60 Hz frame instead
    // of treating the loop as physically impossible wheel acceleration.
    state.pane_mut(0).scroll_velocity_y = SIMULATED_SCROLL_VELOCITY;
    let layout_scroll_elapsed = layout_scroll_elapsed + state_update_started.elapsed();

    (cache, message_count, layout_scroll_elapsed, draw_elapsed)
}

fn percentile_95(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * 95).div_ceil(100) - 1]
}

fn run_scenario(label: &str, view_mode: ViewMode) {
    let mut state = state_with_entries(view_mode);
    let tiny = iced_tiny_skia::Renderer::new(Font::DEFAULT, Pixels(16.0));
    let mut renderer = iced::Renderer::Secondary(tiny);
    let mut cache = user_interface::Cache::new();
    let mut scroll_messages = 0usize;

    for _ in 0..WARMUP_FRAMES {
        let frame = render_scroll_frame(&mut state, &mut renderer, cache);
        cache = frame.0;
        scroll_messages += frame.1;
    }

    let mut layout_scroll_samples = Vec::with_capacity(MEASURED_FRAMES);
    let mut software_frame_samples = Vec::with_capacity(MEASURED_FRAMES);
    for _ in 0..MEASURED_FRAMES {
        let frame = render_scroll_frame(&mut state, &mut renderer, cache);
        layout_scroll_samples.push(frame.2);
        software_frame_samples.push(frame.2 + frame.3);
        cache = frame.0;
        scroll_messages += frame.1;
    }

    let layout_scroll_p95 = percentile_95(&mut layout_scroll_samples);
    let software_frame_p95 = percentile_95(&mut software_frame_samples);
    println!(
        "100k {label} virtual scroll: layout+scroll p95={layout_scroll_p95:?} \
         (budget={LAYOUT_SCROLL_BUDGET:?}), software frame p95={software_frame_p95:?} \
         (budget={SOFTWARE_FRAME_BUDGET:?}), samples={MEASURED_FRAMES}"
    );

    assert!(
        scroll_messages >= WARMUP_FRAMES + MEASURED_FRAMES,
        "the synthetic wheel input did not reach the virtual {label}"
    );
    assert!(
        layout_scroll_p95 <= LAYOUT_SCROLL_BUDGET,
        "100k-entry {label} layout+scroll p95 {layout_scroll_p95:?} exceeded \
         {LAYOUT_SCROLL_BUDGET:?}"
    );
    assert!(
        software_frame_p95 <= SOFTWARE_FRAME_BUDGET,
        "100k-entry {label} software frame p95 {software_frame_p95:?} exceeded \
         {SOFTWARE_FRAME_BUDGET:?}"
    );
}

fn main() {
    run_scenario("list", ViewMode::List);
    run_scenario("grid", ViewMode::Grid);
}
