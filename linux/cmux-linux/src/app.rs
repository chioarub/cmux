use crate::capabilities::linux_v1_capabilities;
use crate::config::ColorScheme;
use crate::model::{AppModel, SharedModel};
use crate::session::{configured_session_path, load_state, save_state};
use crate::socket::{configured_socket_path, remove_socket_file, spawn_server};
use crate::state::{
    AppState, FocusDirection, Pane, PaneId, SplitOrientation, Surface, Workspace, WorkspaceId,
    WorkspaceLayout,
};
use crate::terminal_host::{TerminalBridge, TerminalRuntime};
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use uuid::Uuid;

const APP_ID: &str = "dev.cmux.linux";

/// Cached widget references for a single pane, enabling in-place updates
/// without tearing down and rebuilding the widget tree.
struct CachedPaneWidgets {
    frame: gtk::Frame,
    notebook: gtk::Notebook,
    tab_labels: HashMap<Uuid, gtk::Label>,
}

/// Cached widget tree for a workspace's full layout.
struct CachedWorkspaceLayout {
    root_widget: gtk::Widget,
    fingerprint: String,
    pane_widgets: HashMap<Uuid, CachedPaneWidgets>,
}

/// Cached sidebar button references for a single workspace row.
struct CachedSidebarRow {
    button: gtk::ToggleButton,
    title_label: gtk::Label,
    meta_label: gtk::Label,
}

struct WindowShell {
    window: adw::ApplicationWindow,
    sidebar_box: gtk::Box,
    content_box: gtk::Box,
    // Incremental rendering state
    current_workspace_id: Option<Uuid>,
    workspace_layouts: HashMap<Uuid, CachedWorkspaceLayout>,
    sidebar_rows: Vec<(Uuid, CachedSidebarRow)>,
}

pub fn run() -> gtk::glib::ExitCode {
    let socket_path = configured_socket_path();
    let session_path = configured_session_path();
    let (terminal_bridge, terminal_receiver) = TerminalBridge::new();
    let mut restored_state = match load_state(&session_path) {
        Ok(Some(state)) => state,
        Ok(None) => AppState::new(),
        Err(error) => {
            eprintln!(
                "cmux-linux: failed to load session {}: {error}",
                session_path.display()
            );
            AppState::new()
        }
    };
    // Collapse multiple windows into a single window on startup.
    // Multi-window state can persist from previous sessions and causes a
    // confusing double-window on launch.
    collapse_to_single_window(&mut restored_state);
    let model = AppModel::shared_with_state(socket_path.clone(), terminal_bridge, restored_state);
    let terminal_runtime = Rc::new(RefCell::new(TerminalRuntime::new(
        model.clone(),
        terminal_receiver,
    )));
    let server_status = match spawn_server(model.clone()) {
        Ok(runtime) => format!("socket={}", runtime.socket_path),
        Err(error) => format!("socket_error={error}"),
    };

    let app = adw::Application::builder().application_id(APP_ID).build();
    {
        let socket_path = socket_path.clone();
        let session_path = session_path.clone();
        let model = model.clone();
        app.connect_shutdown(move |_| {
            persist_session_snapshot(&session_path, &model);
            remove_socket_file(&socket_path);
        });
    }
    {
        let model = model.clone();
        let terminal_runtime = terminal_runtime.clone();
        let server_status = server_status.clone();
        let session_path = session_path.clone();
        let activated = Rc::new(Cell::new(false));
        app.connect_activate(move |app| {
            if activated.get() {
                if let Some(window) = app.active_window() {
                    window.present();
                }
                return;
            }
            activated.set(true);
            build_ui(
                app,
                model.clone(),
                terminal_runtime.clone(),
                server_status.clone(),
                session_path.clone(),
            )
        });
    }

    app.run()
}

fn build_ui(
    app: &adw::Application,
    model: SharedModel,
    terminal_runtime: Rc<RefCell<TerminalRuntime>>,
    server_status: String,
    session_path: PathBuf,
) {
    let config = model.lock().config.clone();
    let color_scheme = match config.color_scheme {
        ColorScheme::System => adw::ColorScheme::Default,
        ColorScheme::Dark => adw::ColorScheme::ForceDark,
        ColorScheme::Light => adw::ColorScheme::ForceLight,
    };
    adw::StyleManager::default().set_color_scheme(color_scheme);
    install_css();
    let capabilities = linux_v1_capabilities();
    let feature_summary = capabilities.enabled_feature_labels().join(",");
    let backend_status = terminal_runtime.borrow().backend_status().clone();
    let banner_text = format!(
        "platform={} frontend={} browser={} window_multi={} terminal_backend={} features={} {}{}",
        capabilities.platform.id,
        capabilities.platform.frontend,
        capabilities.features.browser,
        capabilities.platform.window_multi,
        backend_status.active,
        feature_summary,
        server_status,
        backend_status
            .note
            .as_ref()
            .map(|note| format!(" | {note}"))
            .unwrap_or_default(),
    );

    let window_shells: Rc<RefCell<HashMap<Uuid, WindowShell>>> =
        Rc::new(RefCell::new(HashMap::new()));

    let render: Rc<dyn Fn()> = {
        let app = app.clone();
        let model = model.clone();
        let terminal_runtime = terminal_runtime.clone();
        let banner_text = banner_text.clone();
        let window_shells = window_shells.clone();
        Rc::new(move || {
            if let Some(snapshot) = snapshot_state(&model) {
                terminal_runtime.borrow_mut().reconcile(&snapshot);
                reconcile_window_shells(
                    &app,
                    &model,
                    &terminal_runtime,
                    &snapshot,
                    &window_shells,
                    &banner_text,
                );
            }
        })
    };

    install_state_action(
        app,
        "window-new",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.create_window_with_focus(true);
        },
    );
    install_state_action(
        app,
        "window-close",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.close_window(state.window_id);
        },
    );
    install_state_action(
        app,
        "workspace-new",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            state.create_workspace();
        },
    );
    install_state_action(
        app,
        "workspace-close",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let workspace_id = state.selected_workspace_id;
            let _ = state.close_workspace(workspace_id);
        },
    );
    install_state_action(
        app,
        "workspace-next",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.select_next_workspace();
        },
    );
    install_state_action(
        app,
        "workspace-previous",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.select_previous_workspace();
        },
    );
    install_state_action(
        app,
        "workspace-last",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.select_last_workspace();
        },
    );
    install_state_action(
        app,
        "split-right",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.split_selected_pane(SplitOrientation::Horizontal, false);
        },
    );
    install_state_action(
        app,
        "split-down",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.split_selected_pane(SplitOrientation::Vertical, false);
        },
    );
    install_state_action(
        app,
        "surface-new",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.create_surface_in_selected_pane();
        },
    );
    install_state_action(
        app,
        "surface-close",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.close_selected_surface();
        },
    );
    install_state_action(
        app,
        "surface-next",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_relative_surface(1);
        },
    );
    install_state_action(
        app,
        "surface-previous",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_relative_surface(-1);
        },
    );
    install_state_action(
        app,
        "pane-last",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_last_pane();
        },
    );
    install_state_action(
        app,
        "pane-focus-left",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_adjacent_pane(FocusDirection::Left);
        },
    );
    install_state_action(
        app,
        "pane-focus-right",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_adjacent_pane(FocusDirection::Right);
        },
    );
    install_state_action(
        app,
        "pane-focus-up",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_adjacent_pane(FocusDirection::Up);
        },
    );
    install_state_action(
        app,
        "pane-focus-down",
        model.clone(),
        terminal_runtime.clone(),
        render.clone(),
        |state| {
            let _ = state.focus_adjacent_pane(FocusDirection::Down);
        },
    );
    install_default_accelerators(app);

    let last_revision = Rc::new(RefCell::new(current_revision(&model)));
    let session_dirty = Rc::new(Cell::new(false));
    {
        let model = model.clone();
        let render = render.clone();
        let terminal_runtime = terminal_runtime.clone();
        let last_revision = last_revision.clone();
        let session_dirty = session_dirty.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            terminal_runtime.borrow_mut().pump_commands();
            let revision = current_revision(&model);
            let mut observed_revision = last_revision.borrow_mut();
            if revision != *observed_revision {
                *observed_revision = revision;
                render.as_ref()();
                session_dirty.set(true);
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    {
        let model = model.clone();
        let session_dirty = session_dirty.clone();
        let session_path = session_path.clone();
        gtk::glib::timeout_add_seconds_local(3, move || {
            if session_dirty.replace(false) {
                persist_session_snapshot(&session_path, &model);
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    render.as_ref()();
    focus_selected_surface(&model, &terminal_runtime);
}

fn snapshot_state(model: &SharedModel) -> Option<AppState> {
    Some(model.lock().snapshot_state())
}

fn current_revision(model: &SharedModel) -> u64 {
    model.lock().revision()
}

fn persist_session_snapshot(session_path: &std::path::Path, model: &SharedModel) {
    let snapshot = model.lock().snapshot_state();

    if let Err(error) = save_state(session_path, &snapshot) {
        eprintln!(
            "cmux-linux: failed to save session {}: {error}",
            session_path.display()
        );
    }
}

fn install_state_action<F>(
    app: &adw::Application,
    name: &str,
    model: SharedModel,
    terminal_runtime: Rc<RefCell<TerminalRuntime>>,
    render: Rc<dyn Fn()>,
    handler: F,
) where
    F: Fn(&mut AppState) + 'static,
{
    let action = gtk::gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| {
        handler(&mut model.lock().state);
        render.as_ref()();
        focus_selected_surface(&model, &terminal_runtime);
    });
    app.add_action(&action);
}

fn install_default_accelerators(app: &adw::Application) {
    // Linux-friendly shortcuts: Ctrl+Shift prefix avoids conflicts with
    // terminal control sequences (Ctrl+C/D/W/Z/L etc.).
    app.set_accels_for_action("app.workspace-new", &["<Primary><Shift>n"]);
    app.set_accels_for_action("app.workspace-close", &["<Primary><Shift>q"]);
    app.set_accels_for_action("app.workspace-next", &["<Primary><Alt>Page_Down"]);
    app.set_accels_for_action("app.workspace-previous", &["<Primary><Alt>Page_Up"]);
    app.set_accels_for_action("app.workspace-last", &["<Primary><Shift>grave"]);
    app.set_accels_for_action("app.split-right", &["<Primary><Shift>d"]);
    app.set_accels_for_action("app.split-down", &["<Primary><Shift>e"]);
    app.set_accels_for_action("app.surface-new", &["<Primary><Shift>t"]);
    app.set_accels_for_action("app.surface-close", &["<Primary><Shift>w"]);
    app.set_accels_for_action("app.surface-next", &["<Primary>Tab", "<Primary>Page_Down"]);
    app.set_accels_for_action(
        "app.surface-previous",
        &["<Primary><Shift>ISO_Left_Tab", "<Primary>Page_Up"],
    );
    app.set_accels_for_action("app.pane-last", &["<Primary><Alt>grave"]);
    app.set_accels_for_action("app.pane-focus-left", &["<Primary><Shift>Left"]);
    app.set_accels_for_action("app.pane-focus-right", &["<Primary><Shift>Right"]);
    app.set_accels_for_action("app.pane-focus-up", &["<Primary><Shift>Up"]);
    app.set_accels_for_action("app.pane-focus-down", &["<Primary><Shift>Down"]);
    app.set_accels_for_action("app.window-new", &["<Primary><Alt>n"]);
    app.set_accels_for_action("app.window-close", &["<Primary><Alt>w"]);
}

fn install_css() {
    let css = gtk::CssProvider::new();
    css.load_from_data(concat!(
        /* Pane split separators */
        "paned > separator { min-width: 4px; min-height: 4px; background: @borders; }\n",
        /* Sidebar workspace buttons */
        ".sidebar-workspace { border-radius: 8px; padding: 8px 12px; }\n",
        ".sidebar-workspace:checked { background: alpha(@accent_bg_color, 0.15); }\n",
        ".workspace-title { font-weight: bold; font-size: 13px; }\n",
        ".workspace-meta { font-size: 11px; opacity: 0.55; }\n",
        /* Notebook tab bar — compact, modern */
        ".surface-notebook > header.top { min-height: 0; padding: 0; }\n",
        ".surface-notebook tab { padding: 2px 6px; min-height: 0; }\n",
        ".surface-notebook tab label { font-size: 12px; padding: 0; margin: 0; }\n",
        ".surface-notebook tab:checked { background: alpha(@accent_bg_color, 0.08); }\n",
        /* Tab close button */
        ".tab-close-btn { min-width: 18px; min-height: 18px; padding: 0; margin: 0; ",
        "border-radius: 50%; opacity: 0.35; }\n",
        ".tab-close-btn:hover { opacity: 1.0; background: alpha(@error_color, 0.15); }\n",
        /* Pane frame: focus indicator */
        ".pane-frame { border: none; }\n",
        ".pane-focused { border: 2px solid alpha(@accent_bg_color, 0.5); border-radius: 3px; }\n",
        ".pane-unfocused { border: 2px solid transparent; border-radius: 3px; }\n",
    ));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn reconcile_window_shells(
    app: &adw::Application,
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    snapshot: &AppState,
    window_shells: &Rc<RefCell<HashMap<Uuid, WindowShell>>>,
    banner_text: &str,
) {
    let live_window_ids = snapshot
        .windows
        .iter()
        .map(|window| window.id)
        .collect::<HashSet<_>>();
    let mut shells = window_shells.borrow_mut();

    let stale_window_ids = shells
        .keys()
        .copied()
        .filter(|window_id| !live_window_ids.contains(window_id))
        .collect::<Vec<_>>();
    for window_id in stale_window_ids {
        if let Some(shell) = shells.remove(&window_id) {
            shell.window.close();
        }
    }

    for (index, window_state) in snapshot.windows.iter().enumerate() {
        let shell = shells.entry(window_state.id).or_insert_with(|| {
            create_window_shell(
                app,
                model.clone(),
                terminal_runtime.clone(),
                window_state.id,
                banner_text.to_string(),
            )
        });
        let workspace_title = snapshot
            .workspace(window_state.selected_workspace_id)
            .map(|ws| ws.title.as_str())
            .unwrap_or("cmux");
        if snapshot.windows.len() > 1 {
            shell
                .window
                .set_title(Some(&format!("{workspace_title} — cmux ({})", index + 1)));
        } else {
            shell
                .window
                .set_title(Some(&format!("{workspace_title} — cmux")));
        }
        reconcile_sidebar(
            shell,
            model,
            terminal_runtime,
            snapshot,
            window_state.id,
        );
        reconcile_workspace_content(
            shell,
            model,
            terminal_runtime,
            snapshot,
            window_state.id,
        );
    }
}

fn create_window_shell(
    app: &adw::Application,
    model: SharedModel,
    terminal_runtime: Rc<RefCell<TerminalRuntime>>,
    window_id: Uuid,
    _banner_text: String,
) -> WindowShell {
    let header = adw::HeaderBar::new();

    // Left side: workspace controls
    let ws_new = gtk::Button::from_icon_name("tab-new-symbolic");
    ws_new.set_tooltip_text(Some("New Workspace (Ctrl+Shift+N)"));
    let ws_close = gtk::Button::from_icon_name("window-close-symbolic");
    ws_close.set_tooltip_text(Some("Close Workspace (Ctrl+Shift+Q)"));
    let ws_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    ws_box.append(&ws_new);
    ws_box.append(&ws_close);
    header.pack_start(&ws_box);

    // Right side: split / surface controls
    let split_h = gtk::Button::from_icon_name("object-flip-horizontal-symbolic");
    split_h.set_tooltip_text(Some("Split Right (Ctrl+Shift+D)"));
    let split_v = gtk::Button::from_icon_name("object-flip-vertical-symbolic");
    split_v.set_tooltip_text(Some("Split Down (Ctrl+Shift+E)"));
    let surf_new = gtk::Button::from_icon_name("list-add-symbolic");
    surf_new.set_tooltip_text(Some("New Tab (Ctrl+Shift+T)"));
    let surf_close = gtk::Button::from_icon_name("list-remove-symbolic");
    surf_close.set_tooltip_text(Some("Close Tab (Ctrl+Shift+W)"));

    let right_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    right_box.append(&split_h);
    right_box.append(&split_v);
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.set_margin_start(4);
    sep.set_margin_end(4);
    right_box.append(&sep);
    right_box.append(&surf_new);
    right_box.append(&surf_close);
    header.pack_end(&right_box);

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar_box.set_margin_top(6);
    sidebar_box.set_margin_bottom(6);
    sidebar_box.set_margin_start(6);
    sidebar_box.set_margin_end(6);

    let sidebar_width = model.lock().config.sidebar_width;

    let sidebar = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(sidebar_width)
        .child(&sidebar_box)
        .build();

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.set_hexpand(true);
    content_box.set_vexpand(true);

    let main_split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .wide_handle(true)
        .build();
    main_split.set_position(sidebar_width);
    main_split.set_start_child(Some(&sidebar));
    main_split.set_end_child(Some(&content_box));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&main_split);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("cmux")
        .default_width(1400)
        .default_height(900)
        .content(&root)
        .build();

    {
        let model = model.clone();
        let terminal_runtime = terminal_runtime.clone();
        window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                return;
            }
            let _ = model.lock().state.focus_window(window_id);
            focus_selected_surface(&model, &terminal_runtime);
        });
    }

    {
        let model = model.clone();
        window.connect_close_request(move |_| {
            let _ = model.lock().state.close_window(window_id);
            gtk::glib::Propagation::Proceed
        });
    }

    // Window-level CAPTURE-phase keyboard handler.
    // Ensures Ctrl+Shift shortcuts work even when VTE has focus (VTE eats
    // plain Ctrl+letter combos for terminal control sequences).
    {
        let app = app.clone();
        let model = model.clone();
        let terminal_runtime = terminal_runtime.clone();
        let key_controller = gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        key_controller.connect_key_pressed(move |_, keyval, _keycode, modifiers| {
            let ctrl = modifiers & gtk::gdk::ModifierType::CONTROL_MASK
                == gtk::gdk::ModifierType::CONTROL_MASK;
            let shift = modifiers & gtk::gdk::ModifierType::SHIFT_MASK
                == gtk::gdk::ModifierType::SHIFT_MASK;
            let alt = modifiers & gtk::gdk::ModifierType::ALT_MASK
                == gtk::gdk::ModifierType::ALT_MASK;

            if !ctrl && !alt {
                return gtk::glib::Propagation::Proceed;
            }

            // Alt+1..9: select workspace by index (direct state manipulation)
            if alt && !ctrl && !shift {
                let index = match keyval {
                    gtk::gdk::Key::_1 => Some(0usize),
                    gtk::gdk::Key::_2 => Some(1),
                    gtk::gdk::Key::_3 => Some(2),
                    gtk::gdk::Key::_4 => Some(3),
                    gtk::gdk::Key::_5 => Some(4),
                    gtk::gdk::Key::_6 => Some(5),
                    gtk::gdk::Key::_7 => Some(6),
                    gtk::gdk::Key::_8 => Some(7),
                    gtk::gdk::Key::_9 => Some(8),
                    _ => None,
                };
                if let Some(index) = index {
                    let mut guard = model.lock();
                    let ws_ids: Vec<Uuid> = guard
                        .state
                        .workspaces_in_window(window_id)
                        .iter()
                        .map(|ws| ws.id)
                        .collect();
                    if let Some(&ws_id) = ws_ids.get(index) {
                        let _ = guard.state.select_workspace(ws_id);
                    }
                    drop(guard);
                    focus_selected_surface(&model, &terminal_runtime);
                    return gtk::glib::Propagation::Stop;
                }
            }

            // Ctrl+Shift combos — main shortcut layer
            if ctrl && shift && !alt {
                // Let copy/paste through to VTE's own handler
                match keyval {
                    gtk::gdk::Key::C | gtk::gdk::Key::c => {
                        return gtk::glib::Propagation::Proceed
                    }
                    gtk::gdk::Key::V | gtk::gdk::Key::v => {
                        return gtk::glib::Propagation::Proceed
                    }
                    _ => {}
                }
                let action = match keyval {
                    gtk::gdk::Key::T | gtk::gdk::Key::t => Some("surface-new"),
                    gtk::gdk::Key::W | gtk::gdk::Key::w => Some("surface-close"),
                    gtk::gdk::Key::D | gtk::gdk::Key::d => Some("split-right"),
                    gtk::gdk::Key::E | gtk::gdk::Key::e => Some("split-down"),
                    gtk::gdk::Key::N | gtk::gdk::Key::n => Some("workspace-new"),
                    gtk::gdk::Key::Q | gtk::gdk::Key::q => Some("workspace-close"),
                    gtk::gdk::Key::bracketright | gtk::gdk::Key::braceright => {
                        Some("surface-next")
                    }
                    gtk::gdk::Key::bracketleft | gtk::gdk::Key::braceleft => {
                        Some("surface-previous")
                    }
                    gtk::gdk::Key::ISO_Left_Tab | gtk::gdk::Key::Tab => {
                        Some("surface-previous")
                    }
                    gtk::gdk::Key::Left => Some("pane-focus-left"),
                    gtk::gdk::Key::Right => Some("pane-focus-right"),
                    gtk::gdk::Key::Up => Some("pane-focus-up"),
                    gtk::gdk::Key::Down => Some("pane-focus-down"),
                    gtk::gdk::Key::grave | gtk::gdk::Key::asciitilde => Some("workspace-last"),
                    _ => None,
                };
                if let Some(action_name) = action {
                    app.activate_action(action_name, None);
                    return gtk::glib::Propagation::Stop;
                }
            }

            // Ctrl-only (no shift, no alt)
            if ctrl && !shift && !alt {
                let action = match keyval {
                    gtk::gdk::Key::Tab => Some("surface-next"),
                    gtk::gdk::Key::Page_Down => Some("surface-next"),
                    gtk::gdk::Key::Page_Up => Some("surface-previous"),
                    _ => None,
                };
                if let Some(action_name) = action {
                    app.activate_action(action_name, None);
                    return gtk::glib::Propagation::Stop;
                }
            }

            // Ctrl+Alt combos
            if ctrl && alt && !shift {
                let action = match keyval {
                    gtk::gdk::Key::n | gtk::gdk::Key::N => Some("window-new"),
                    gtk::gdk::Key::w | gtk::gdk::Key::W => Some("window-close"),
                    gtk::gdk::Key::Page_Down => Some("workspace-next"),
                    gtk::gdk::Key::Page_Up => Some("workspace-previous"),
                    gtk::gdk::Key::grave => Some("pane-last"),
                    _ => None,
                };
                if let Some(action_name) = action {
                    app.activate_action(action_name, None);
                    return gtk::glib::Propagation::Stop;
                }
            }

            gtk::glib::Propagation::Proceed
        });
        window.add_controller(key_controller);
    }

    connect_window_action_button(&ws_new, app, &model, &terminal_runtime, window_id, "workspace-new");
    connect_window_action_button(&ws_close, app, &model, &terminal_runtime, window_id, "workspace-close");
    connect_window_action_button(&split_h, app, &model, &terminal_runtime, window_id, "split-right");
    connect_window_action_button(&split_v, app, &model, &terminal_runtime, window_id, "split-down");
    connect_window_action_button(&surf_new, app, &model, &terminal_runtime, window_id, "surface-new");
    connect_window_action_button(&surf_close, app, &model, &terminal_runtime, window_id, "surface-close");

    window.present();
    WindowShell {
        window,
        sidebar_box,
        content_box,
        current_workspace_id: None,
        workspace_layouts: HashMap::new(),
        sidebar_rows: Vec::new(),
    }
}

fn connect_window_action_button(
    button: &gtk::Button,
    app: &adw::Application,
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    window_id: Uuid,
    action_name: &'static str,
) {
    let app = app.clone();
    let model = model.clone();
    let terminal_runtime = terminal_runtime.clone();
    button.connect_clicked(move |_| {
        let _ = model.lock().state.focus_window(window_id);
        focus_selected_surface(&model, &terminal_runtime);
        app.activate_action(action_name, None);
    });
}

fn focus_selected_surface(model: &SharedModel, terminal_runtime: &Rc<RefCell<TerminalRuntime>>) {
    let model = model.clone();
    let terminal_runtime = terminal_runtime.clone();
    gtk::glib::idle_add_local_once(move || {
        let surface_id = model.lock().state.current_surface_id();
        if let Some(surface_id) = surface_id {
            terminal_runtime.borrow_mut().focus_surface(surface_id);
        }
    });
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

// ---------------------------------------------------------------------------
// Sidebar — incremental reconciliation
// ---------------------------------------------------------------------------

type WorkspaceRowData = (
    Uuid,      // workspace_id
    String,    // title
    String,    // meta text
    bool,      // selected
);

fn collect_sidebar_rows(snapshot: &AppState, window_id: Uuid) -> Vec<WorkspaceRowData> {
    let selected_workspace_id = snapshot
        .window(window_id)
        .map(|window| window.selected_workspace_id)
        .unwrap_or(Uuid::nil());

    snapshot
        .workspaces
        .iter()
        .filter(|workspace| workspace.window_id == window_id)
        .map(|workspace| {
            let unread_count = workspace
                .panes
                .iter()
                .flat_map(|pane| &pane.surfaces)
                .filter(|surface| surface.unread)
                .count();
            let surface_count = workspace.surface_count();
            let mut meta_parts = Vec::new();
            if surface_count > 1 {
                meta_parts.push(format!("{surface_count} tabs"));
            }
            if unread_count > 0 {
                meta_parts.push(format!("{unread_count} unread"));
            }
            if let Some(ref cwd) = workspace.current_directory {
                if let Some(dir_name) = std::path::Path::new(cwd).file_name() {
                    meta_parts.push(dir_name.to_string_lossy().into_owned());
                }
            }
            let meta_text = meta_parts.join(" · ");
            (
                workspace.id,
                workspace.title.clone(),
                meta_text,
                workspace.id == selected_workspace_id,
            )
        })
        .collect()
}

fn reconcile_sidebar(
    shell: &mut WindowShell,
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    snapshot: &AppState,
    window_id: Uuid,
) {
    let rows = collect_sidebar_rows(snapshot, window_id);
    let row_ids: Vec<Uuid> = rows.iter().map(|(id, _, _, _)| *id).collect();

    // If the workspace list (order + IDs) changed, do a full rebuild.
    let existing_ids: Vec<Uuid> = shell.sidebar_rows.iter().map(|(id, _)| *id).collect();
    if existing_ids != row_ids {
        // Full rebuild needed
        clear_box(&shell.sidebar_box);
        shell.sidebar_rows.clear();
        for (workspace_id, title, meta_text, selected) in &rows {
            let row = create_sidebar_row(model, terminal_runtime, *workspace_id);
            row.title_label.set_label(title);
            row.meta_label.set_label(meta_text);
            row.meta_label.set_visible(!meta_text.is_empty());
            row.button.set_active(*selected);
            shell.sidebar_box.append(&row.button);
            shell.sidebar_rows.push((*workspace_id, row));
        }
        return;
    }

    // In-place update: same workspace list, just update labels + selection
    for ((_, cached_row), (_, title, meta_text, selected)) in
        shell.sidebar_rows.iter().zip(rows.iter())
    {
        cached_row.title_label.set_label(title);
        cached_row.meta_label.set_label(meta_text);
        cached_row.meta_label.set_visible(!meta_text.is_empty());
        // Avoid signal recursion: only toggle if state actually differs
        if cached_row.button.is_active() != *selected {
            cached_row.button.set_active(*selected);
        }
    }
}

fn create_sidebar_row(
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    workspace_id: Uuid,
) -> CachedSidebarRow {
    let title_label = gtk::Label::new(None);
    title_label.set_xalign(0.0);
    title_label.add_css_class("workspace-title");

    let meta_label = gtk::Label::new(None);
    meta_label.set_xalign(0.0);
    meta_label.add_css_class("workspace-meta");
    meta_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let row_content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row_content.append(&title_label);
    row_content.append(&meta_label);

    let button = gtk::ToggleButton::new();
    button.set_child(Some(&row_content));
    button.set_halign(gtk::Align::Fill);
    button.set_hexpand(true);
    button.add_css_class("sidebar-workspace");
    button.add_css_class("flat");

    let model = model.clone();
    let terminal_runtime = terminal_runtime.clone();
    button.connect_clicked(move |_| {
        let _ = model.lock().state.select_workspace(workspace_id);
        focus_selected_surface(&model, &terminal_runtime);
    });

    CachedSidebarRow {
        button,
        title_label,
        meta_label,
    }
}

// ---------------------------------------------------------------------------
// Content area — incremental reconciliation
// ---------------------------------------------------------------------------

/// Compute a structural fingerprint for a workspace layout.
/// This encodes the tree structure (pane IDs, surface IDs, orientations) but
/// NOT dynamic state like focus, titles, or unread counts.
fn layout_fingerprint(workspace: &Workspace) -> String {
    let mut parts = Vec::new();
    layout_fingerprint_recursive(&workspace.layout, &workspace.panes, &mut parts);
    parts.join("|")
}

fn layout_fingerprint_recursive(
    layout: &WorkspaceLayout,
    panes: &[Pane],
    parts: &mut Vec<String>,
) {
    match layout {
        WorkspaceLayout::Pane(pane_id) => {
            let surface_ids: Vec<String> = panes
                .iter()
                .find(|p| p.id == *pane_id)
                .map(|pane| {
                    pane.surfaces
                        .iter()
                        .map(|s| short_id(s.id))
                        .collect()
                })
                .unwrap_or_default();
            parts.push(format!("P{}[{}]", short_id(*pane_id), surface_ids.join(",")));
        }
        WorkspaceLayout::Split {
            orientation,
            first,
            second,
        } => {
            let orient = match orientation {
                SplitOrientation::Horizontal => "H",
                SplitOrientation::Vertical => "V",
            };
            parts.push(format!("S{orient}("));
            layout_fingerprint_recursive(first, panes, parts);
            parts.push(",".to_string());
            layout_fingerprint_recursive(second, panes, parts);
            parts.push(")".to_string());
        }
    }
}

fn reconcile_workspace_content(
    shell: &mut WindowShell,
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    snapshot: &AppState,
    window_id: Uuid,
) {
    let workspace = snapshot
        .window(window_id)
        .and_then(|window| snapshot.workspace(window.selected_workspace_id))
        .cloned();
    let Some(workspace) = workspace else {
        clear_box(&shell.content_box);
        shell.current_workspace_id = None;
        return;
    };

    let workspace_id = workspace.id;
    let fingerprint = layout_fingerprint(&workspace);
    let same_workspace = shell.current_workspace_id == Some(workspace_id);

    // Check if we have a cached layout with matching fingerprint
    if same_workspace {
        if let Some(cached) = shell.workspace_layouts.get(&workspace_id) {
            if cached.fingerprint == fingerprint {
                // Structure unchanged — do in-place updates only
                update_workspace_in_place(cached, &workspace);
                return;
            }
        }
    }

    // Structure changed or workspace switched — need to build/rebuild.

    // If switching workspaces, remove the old root widget from content_box
    // but keep the cached layout alive for quick switching back.
    if !same_workspace {
        clear_box(&shell.content_box);
    } else {
        // Same workspace but structure changed — remove old root
        clear_box(&shell.content_box);
        shell.workspace_layouts.remove(&workspace_id);
    }

    // Check if we already have a cached layout for this workspace (switching back)
    if let Some(cached) = shell.workspace_layouts.get(&workspace_id) {
        if cached.fingerprint == fingerprint {
            shell.content_box.append(&cached.root_widget);
            update_workspace_in_place(cached, &workspace);
            shell.current_workspace_id = Some(workspace_id);
            return;
        } else {
            // Stale cache — remove it
            shell.workspace_layouts.remove(&workspace_id);
        }
    }

    // Build fresh layout
    let mut pane_widgets = HashMap::new();
    let layout_widget = build_workspace_layout_cached(
        model,
        terminal_runtime,
        &workspace,
        &workspace.layout,
        &mut pane_widgets,
    );
    layout_widget.set_hexpand(true);
    layout_widget.set_vexpand(true);
    shell.content_box.append(&layout_widget);

    // Apply initial dynamic state
    let cached = CachedWorkspaceLayout {
        root_widget: layout_widget,
        fingerprint,
        pane_widgets,
    };
    update_workspace_in_place(&cached, &workspace);
    shell.workspace_layouts.insert(workspace_id, cached);
    shell.current_workspace_id = Some(workspace_id);

    // Prune layouts for workspaces that no longer exist
    let live_workspace_ids: HashSet<Uuid> = snapshot
        .workspaces
        .iter()
        .map(|ws| ws.id)
        .collect();
    shell
        .workspace_layouts
        .retain(|id, _| live_workspace_ids.contains(id));
}

/// Update dynamic state (focus, tab selection, tab labels) without rebuilding widgets.
fn update_workspace_in_place(cached: &CachedWorkspaceLayout, workspace: &Workspace) {
    for pane in &workspace.panes {
        let Some(cached_pane) = cached.pane_widgets.get(&pane.id) else {
            continue;
        };

        let is_selected = pane.id == workspace.selected_pane_id;
        // Update pane focus CSS
        if is_selected {
            cached_pane.frame.remove_css_class("pane-unfocused");
            cached_pane.frame.add_css_class("pane-focused");
        } else {
            cached_pane.frame.remove_css_class("pane-focused");
            cached_pane.frame.add_css_class("pane-unfocused");
        }

        // Update tab labels and tooltips
        for surface in &pane.surfaces {
            if let Some(label) = cached_pane.tab_labels.get(&surface.id) {
                let new_text = surface_tab_label(surface);
                if label.label() != new_text {
                    label.set_label(&new_text);
                }
                // Update tooltip with full title
                let full_title = &surface.title;
                if label
                    .tooltip_text()
                    .map_or(true, |t| t.as_str() != full_title)
                {
                    label.set_tooltip_text(Some(full_title));
                }
            }
        }

        // Update selected tab (without rebuilding the notebook)
        if let Some(index) = pane
            .surfaces
            .iter()
            .position(|surface| surface.id == pane.selected_surface_id)
        {
            let target = index as u32;
            if cached_pane.notebook.current_page() != Some(target) {
                cached_pane.notebook.set_current_page(Some(target));
            }
        }

        // Update tab bar visibility
        cached_pane.notebook.set_show_tabs(pane.surfaces.len() > 1);
    }
}

fn build_workspace_layout_cached(
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    workspace: &Workspace,
    layout: &WorkspaceLayout,
    pane_widgets: &mut HashMap<Uuid, CachedPaneWidgets>,
) -> gtk::Widget {
    match layout {
        WorkspaceLayout::Pane(pane_id) => workspace
            .pane(*pane_id)
            .map(|pane| {
                build_pane_view_cached(model, terminal_runtime, workspace.id, pane, pane_widgets)
            })
            .unwrap_or_else(|| missing_pane_widget(*pane_id)),
        WorkspaceLayout::Split {
            orientation,
            first,
            second,
        } => {
            let paned = gtk::Paned::builder()
                .orientation(match orientation {
                    SplitOrientation::Horizontal => gtk::Orientation::Horizontal,
                    SplitOrientation::Vertical => gtk::Orientation::Vertical,
                })
                .wide_handle(true)
                .shrink_start_child(false)
                .shrink_end_child(false)
                .resize_start_child(true)
                .resize_end_child(true)
                .build();
            paned.set_start_child(Some(&build_workspace_layout_cached(
                model,
                terminal_runtime,
                workspace,
                first,
                pane_widgets,
            )));
            paned.set_end_child(Some(&build_workspace_layout_cached(
                model,
                terminal_runtime,
                workspace,
                second,
                pane_widgets,
            )));
            paned.upcast::<gtk::Widget>()
        }
    }
}

fn build_pane_view_cached(
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    _workspace_id: WorkspaceId,
    pane: &Pane,
    pane_widgets: &mut HashMap<Uuid, CachedPaneWidgets>,
) -> gtk::Widget {
    let notebook = gtk::Notebook::new();
    notebook.set_scrollable(true);
    notebook.set_show_tabs(pane.surfaces.len() > 1);
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    notebook.add_css_class("surface-notebook");

    let mut tab_labels = HashMap::new();
    for surface in &pane.surfaces {
        let page = build_surface_view(terminal_runtime, surface);

        // Tab header: compact label + close button
        let label = gtk::Label::new(Some(&surface_tab_label(surface)));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(22);
        label.set_tooltip_text(Some(&surface.title));

        let close_btn = gtk::Button::from_icon_name("window-close-symbolic");
        close_btn.add_css_class("tab-close-btn");
        close_btn.add_css_class("flat");
        close_btn.set_tooltip_text(Some("Close tab"));
        {
            let model = model.clone();
            let surface_id = surface.id;
            close_btn.connect_clicked(move |_| {
                let mut guard = model.lock();
                if let Some((workspace_id, _)) = guard.state.locate_surface(surface_id) {
                    let _ = guard.state.close_surface(workspace_id, surface_id);
                }
            });
        }

        let tab_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab_box.append(&label);
        tab_box.append(&close_btn);

        notebook.append_page(&page, Some(&tab_box));
        tab_labels.insert(surface.id, label);
    }

    let surface_ids = pane
        .surfaces
        .iter()
        .map(|surface| surface.id)
        .collect::<Vec<_>>();
    {
        let model = model.clone();
        let terminal_runtime = terminal_runtime.clone();
        notebook.connect_switch_page(move |_, _, index| {
            if let Some(surface_id) = surface_ids.get(index as usize) {
                let _ = model.lock().state.focus_surface(*surface_id);
                focus_selected_surface(&model, &terminal_runtime);
            }
        });
    }

    if let Some(index) = pane
        .surfaces
        .iter()
        .position(|surface| surface.id == pane.selected_surface_id)
    {
        notebook.set_current_page(Some(index as u32));
    }

    let frame = gtk::Frame::new(None::<&str>);
    frame.add_css_class("pane-frame");
    frame.add_css_class("pane-unfocused");
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    frame.set_child(Some(&notebook));

    pane_widgets.insert(
        pane.id,
        CachedPaneWidgets {
            frame: frame.clone(),
            notebook,
            tab_labels,
        },
    );

    frame.upcast::<gtk::Widget>()
}

fn build_surface_view(
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    surface: &Surface,
) -> gtk::Widget {
    let host_widget = {
        let mut runtime = terminal_runtime.borrow_mut();
        runtime.widget_for_surface(surface.id)
    };
    if host_widget.parent().is_some() {
        host_widget.unparent();
    }
    host_widget.set_hexpand(true);
    host_widget.set_vexpand(true);
    host_widget.upcast::<gtk::Widget>()
}

fn missing_pane_widget(pane_id: PaneId) -> gtk::Widget {
    let label = gtk::Label::new(Some(&format!("Missing pane {}", short_id(pane_id))));
    label.set_xalign(0.0);
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    label.set_margin_start(24);
    label.set_margin_end(24);
    label.upcast::<gtk::Widget>()
}

// ---------------------------------------------------------------------------
// Window collapse — merge multiple windows into one
// ---------------------------------------------------------------------------

fn collapse_to_single_window(state: &mut AppState) {
    if state.windows.len() <= 1 {
        return;
    }
    // Keep the first window; move all workspaces into it.
    let primary_window_id = state.windows[0].id;
    for workspace in &mut state.workspaces {
        workspace.window_id = primary_window_id;
    }
    state.windows.truncate(1);
    state.window_id = primary_window_id;
    // Ensure the window selection is valid
    if !state
        .workspaces
        .iter()
        .any(|ws| ws.id == state.selected_workspace_id)
    {
        state.selected_workspace_id = state
            .workspaces
            .first()
            .map(|ws| ws.id)
            .unwrap_or(Uuid::nil());
    }
    if let Some(window) = state.windows.first_mut() {
        window.selected_workspace_id = state.selected_workspace_id;
    }
}

fn surface_tab_label(surface: &Surface) -> String {
    let display = smart_display_title(&surface.title);
    let mut label = display;
    if surface.unread {
        label.push_str(" •");
    }
    if surface.flash_count > 0 {
        label.push_str(&format!(" ({})", surface.flash_count));
    }
    label
}

/// Extract a short, human-friendly display name from VTE terminal titles.
///
/// Common VTE title formats:
///   "user@hostname: /long/path/to/dir" → "dir"
///   "user@hostname:/long/path"         → "path"
///   "zsh: ~/projects"                  → "projects"
///   "/home/user/Desktop"               → "Desktop"
///   "vim file.txt"                     → "vim file.txt"
///   "htop"                             → "htop"
///   "~"                                → "~"
fn smart_display_title(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Terminal".to_string();
    }

    // Extract path portion after "prefix: path" or "prefix:path" patterns
    let path_candidate = extract_path_from_title(trimmed);

    // If it looks like a filesystem path, return just the last component
    if path_candidate.starts_with('/') || path_candidate.starts_with('~') {
        if path_candidate == "~" || path_candidate == "/" {
            return path_candidate.to_string();
        }
        if let Some(name) = std::path::Path::new(&path_candidate).file_name() {
            let n = name.to_string_lossy();
            if !n.is_empty() {
                return n.into_owned();
            }
        }
    }

    path_candidate
}

/// Strip common terminal title prefixes (user@host:, shell:) to find the
/// path or command name.
fn extract_path_from_title(title: &str) -> String {
    // "user@host: /path" → "/path"
    if let Some(idx) = title.find(": ") {
        let after = title[idx + 2..].trim();
        if !after.is_empty() && (after.starts_with('/') || after.starts_with('~')) {
            return after.to_string();
        }
    }
    // "user@host:/path" → "/path"
    if let Some(idx) = title.find(':') {
        let before = &title[..idx];
        let after = title[idx + 1..].trim();
        if (before.contains('@') || before.len() <= 6)
            && !after.is_empty()
            && (after.starts_with('/') || after.starts_with('~'))
        {
            return after.to_string();
        }
    }
    title.to_string()
}

fn short_id(id: Uuid) -> String {
    let raw = id.as_simple().to_string();
    raw[..8].to_string()
}


