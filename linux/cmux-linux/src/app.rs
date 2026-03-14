use crate::capabilities::linux_v1_capabilities;
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

struct WindowShell {
    window: adw::ApplicationWindow,
    sidebar_box: gtk::Box,
    content_box: gtk::Box,
}

pub fn run() -> gtk::glib::ExitCode {
    let socket_path = configured_socket_path();
    let session_path = configured_session_path();
    let (terminal_bridge, terminal_receiver) = TerminalBridge::new();
    let restored_state = match load_state(&session_path) {
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
        app.connect_activate(move |app| {
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
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
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
        gtk::glib::timeout_add_local(Duration::from_millis(60), move || {
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

fn persist_session_snapshot(session_path: &PathBuf, model: &SharedModel) {
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
    app.set_accels_for_action("app.window-new", &["<Primary><Alt>n"]);
    app.set_accels_for_action("app.window-close", &["<Primary><Alt>w"]);
    app.set_accels_for_action("app.workspace-new", &["<Primary>n"]);
    app.set_accels_for_action("app.workspace-close", &["<Primary><Shift>w"]);
    app.set_accels_for_action("app.workspace-next", &["<Primary><Control>bracketright"]);
    app.set_accels_for_action("app.workspace-previous", &["<Primary><Control>bracketleft"]);
    app.set_accels_for_action("app.workspace-last", &["<Primary><Control>grave"]);
    app.set_accels_for_action("app.split-right", &["<Primary>d"]);
    app.set_accels_for_action("app.split-down", &["<Primary><Shift>d"]);
    app.set_accels_for_action("app.surface-new", &["<Primary>t"]);
    app.set_accels_for_action("app.surface-close", &["<Primary>w"]);
    app.set_accels_for_action("app.surface-next", &["<Primary><Shift>bracketright"]);
    app.set_accels_for_action("app.surface-previous", &["<Primary><Shift>bracketleft"]);
    app.set_accels_for_action("app.pane-last", &["<Primary><Alt>grave"]);
    app.set_accels_for_action("app.pane-focus-left", &["<Primary><Shift>Left"]);
    app.set_accels_for_action("app.pane-focus-right", &["<Primary><Shift>Right"]);
    app.set_accels_for_action("app.pane-focus-up", &["<Primary><Shift>Up"]);
    app.set_accels_for_action("app.pane-focus-down", &["<Primary><Shift>Down"]);
}

fn install_css() {
    let css = gtk::CssProvider::new();
    css.load_from_data(
        "paned > separator { min-width: 6px; min-height: 6px; background: @borders; }",
    );
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
        shell
            .window
            .set_title(Some(&format!("cmux Linux {}", index + 1)));
        render_sidebar(
            &shell.sidebar_box,
            model,
            terminal_runtime,
            snapshot,
            window_state.id,
        );
        render_workspace_content(
            &shell.content_box,
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
    banner_text: String,
) -> WindowShell {
    let title = gtk::Label::new(Some("cmux Linux"));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.set_margin_top(8);
    controls.set_margin_bottom(8);
    controls.set_margin_start(12);
    controls.set_margin_end(12);

    let new_window = gtk::Button::with_label("New Window");
    let close_window = gtk::Button::with_label("Close Window");
    let new_workspace = gtk::Button::with_label("New Workspace");
    let close_workspace = gtk::Button::with_label("Close Workspace");
    let split_right = gtk::Button::with_label("Split Right");
    let split_down = gtk::Button::with_label("Split Down");
    let new_surface = gtk::Button::with_label("New Surface");
    let close_surface = gtk::Button::with_label("Close Surface");

    for button in [
        &new_window,
        &close_window,
        &new_workspace,
        &close_workspace,
        &split_right,
        &split_down,
        &new_surface,
        &close_surface,
    ] {
        controls.append(button);
    }

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    sidebar_box.set_margin_top(8);
    sidebar_box.set_margin_bottom(8);
    sidebar_box.set_margin_start(8);
    sidebar_box.set_margin_end(8);

    let sidebar = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(280)
        .child(&sidebar_box)
        .build();

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content_box.set_hexpand(true);
    content_box.set_vexpand(true);

    let main_split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .wide_handle(true)
        .build();
    main_split.set_position(280);
    main_split.set_start_child(Some(&sidebar));
    main_split.set_end_child(Some(&content_box));

    let banner = gtk::Label::new(Some(&banner_text));
    banner.add_css_class("dim-label");
    banner.set_margin_top(8);
    banner.set_margin_bottom(8);
    banner.set_margin_start(12);
    banner.set_margin_end(12);
    banner.set_xalign(0.0);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&controls);
    root.append(&banner);
    root.append(&main_split);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("cmux Linux")
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

    connect_window_action_button(
        &new_window,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "window-new",
    );
    connect_window_action_button(
        &close_window,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "window-close",
    );
    connect_window_action_button(
        &new_workspace,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "workspace-new",
    );
    connect_window_action_button(
        &close_workspace,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "workspace-close",
    );
    connect_window_action_button(
        &split_right,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "split-right",
    );
    connect_window_action_button(
        &split_down,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "split-down",
    );
    connect_window_action_button(
        &new_surface,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "surface-new",
    );
    connect_window_action_button(
        &close_surface,
        app,
        &model,
        &terminal_runtime,
        window_id,
        "surface-close",
    );

    window.present();
    WindowShell {
        window,
        sidebar_box,
        content_box,
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

fn render_sidebar(
    sidebar_box: &gtk::Box,
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    snapshot: &AppState,
    window_id: Uuid,
) {
    clear_box(sidebar_box);
    let selected_workspace_id = snapshot
        .window(window_id)
        .map(|window| window.selected_workspace_id)
        .unwrap_or(Uuid::nil());

    let rows: Vec<(
        WorkspaceId,
        String,
        Option<String>,
        usize,
        usize,
        usize,
        bool,
    )> = snapshot
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
            (
                workspace.id,
                workspace.title.clone(),
                workspace.current_directory.clone(),
                workspace.pane_count(),
                workspace.surface_count(),
                unread_count,
                workspace.id == selected_workspace_id,
            )
        })
        .collect();

    for (
        workspace_id,
        title,
        current_directory,
        pane_count,
        surface_count,
        unread_count,
        selected,
    ) in rows
    {
        let title_label = gtk::Label::new(Some(&title));
        title_label.set_xalign(0.0);
        title_label.add_css_class("heading");

        let meta_label = gtk::Label::new(Some(&format!(
            "{pane_count} panes | {surface_count} surfaces | {unread_count} unread"
        )));
        meta_label.set_xalign(0.0);
        meta_label.add_css_class("dim-label");

        let id_label = gtk::Label::new(Some(&format!("workspace {}", short_id(workspace_id))));
        id_label.set_xalign(0.0);
        id_label.add_css_class("caption");

        let cwd_label = current_directory.map(|cwd| {
            let label = gtk::Label::new(Some(&cwd));
            label.set_xalign(0.0);
            label.add_css_class("caption");
            label
        });

        let row_content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row_content.set_margin_top(10);
        row_content.set_margin_bottom(10);
        row_content.set_margin_start(12);
        row_content.set_margin_end(12);
        row_content.append(&title_label);
        row_content.append(&meta_label);
        row_content.append(&id_label);
        if let Some(cwd_label) = cwd_label.as_ref() {
            row_content.append(cwd_label);
        }

        let button = gtk::ToggleButton::new();
        button.set_active(selected);
        button.set_child(Some(&row_content));
        button.set_halign(gtk::Align::Fill);
        button.set_hexpand(true);

        let model = model.clone();
        let terminal_runtime = terminal_runtime.clone();
        button.connect_clicked(move |_| {
            let _ = model.lock().state.select_workspace(workspace_id);
            focus_selected_surface(&model, &terminal_runtime);
        });

        sidebar_box.append(&button);
    }
}

fn render_workspace_content(
    content_box: &gtk::Box,
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    snapshot: &AppState,
    window_id: Uuid,
) {
    clear_box(content_box);

    let workspace = snapshot
        .window(window_id)
        .and_then(|window| snapshot.workspace(window.selected_workspace_id))
        .cloned();
    let Some(workspace) = workspace else {
        return;
    };

    let selected_pane_summary = workspace
        .selected_pane()
        .map(|pane| format!("selected pane {}", short_id(pane.id)))
        .unwrap_or_else(|| "no selected pane".to_string());

    let header = gtk::Label::new(Some(&format!(
        "{} | {} panes | {} surfaces | {}{}",
        workspace.title,
        workspace.pane_count(),
        workspace.surface_count(),
        selected_pane_summary,
        workspace
            .current_directory
            .as_ref()
            .map(|cwd| format!(" | cwd={cwd}"))
            .unwrap_or_default(),
    )));
    header.add_css_class("title-3");
    header.set_xalign(0.0);
    header.set_margin_top(12);
    header.set_margin_bottom(12);
    header.set_margin_start(12);
    header.set_margin_end(12);
    content_box.append(&header);

    let layout_widget =
        build_workspace_layout(model, terminal_runtime, &workspace, &workspace.layout);
    layout_widget.set_hexpand(true);
    layout_widget.set_vexpand(true);
    content_box.append(&layout_widget);
}

fn build_workspace_layout(
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    workspace: &Workspace,
    layout: &WorkspaceLayout,
) -> gtk::Widget {
    match layout {
        WorkspaceLayout::Pane(pane_id) => workspace
            .pane(*pane_id)
            .map(|pane| build_pane_view(model, terminal_runtime, workspace.id, pane))
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
            paned.set_start_child(Some(&build_workspace_layout(
                model,
                terminal_runtime,
                workspace,
                first,
            )));
            paned.set_end_child(Some(&build_workspace_layout(
                model,
                terminal_runtime,
                workspace,
                second,
            )));
            paned.upcast::<gtk::Widget>()
        }
    }
}

fn build_pane_view(
    model: &SharedModel,
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    workspace_id: WorkspaceId,
    pane: &Pane,
) -> gtk::Widget {
    let notebook = gtk::Notebook::new();
    notebook.set_scrollable(true);
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    let selected_surface_title = pane
        .selected_surface()
        .map(|surface| surface.title.clone())
        .unwrap_or_else(|| "No surface".to_string());
    let unread_count = pane
        .surfaces
        .iter()
        .filter(|surface| surface.unread)
        .count();

    for surface in &pane.surfaces {
        let page = build_surface_view(terminal_runtime, surface);
        let label = gtk::Label::new(Some(&surface_tab_label(surface)));
        notebook.append_page(&page, Some(&label));
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

    let frame = gtk::Frame::new(Some(&format!(
        "Pane {} | workspace {} | selected: {} | unread={}",
        short_id(pane.id),
        short_id(workspace_id),
        selected_surface_title,
        unread_count,
    )));
    frame.set_margin_top(12);
    frame.set_margin_bottom(12);
    frame.set_margin_start(12);
    frame.set_margin_end(12);
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    frame.set_child(Some(&notebook));
    frame.upcast::<gtk::Widget>()
}

fn build_surface_view(
    terminal_runtime: &Rc<RefCell<TerminalRuntime>>,
    surface: &Surface,
) -> gtk::Widget {
    let (backend_name, host_widget) = {
        let mut runtime = terminal_runtime.borrow_mut();
        (
            runtime.backend_name().to_string(),
            runtime.widget_for_surface(surface.id),
        )
    };
    if host_widget.parent().is_some() {
        host_widget.unparent();
    }
    host_widget.set_hexpand(true);
    host_widget.set_vexpand(true);

    let title = gtk::Label::new(Some(&format!(
        "{} | unread={} | flash_count={}",
        surface.title, surface.unread, surface.flash_count
    )));
    title.set_xalign(0.0);
    title.add_css_class("heading");

    let backend = gtk::Label::new(Some(&format!(
        "backend={} | supported=vte | surface_id={} | readback_bytes={}",
        backend_name,
        short_id(surface.id),
        surface.transcript.len(),
    )));
    backend.set_xalign(0.0);
    backend.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&title);
    content.append(&backend);
    content.append(&host_widget);

    content.upcast::<gtk::Widget>()
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

fn surface_tab_label(surface: &Surface) -> String {
    let mut label = surface.title.clone();
    if surface.unread {
        label.push_str(" •");
    }
    if surface.flash_count > 0 {
        label.push_str(&format!(" ({})", surface.flash_count));
    }
    label
}

fn short_id(id: Uuid) -> String {
    let raw = id.as_simple().to_string();
    raw[..8].to_string()
}
