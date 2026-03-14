use crate::capabilities::{linux_v1_capabilities, CapabilityProfile};
use crate::model::{
    AppFocusOverride, AppModel, CommandPaletteMode, CommandPaletteState, RenameTarget, SharedModel,
};
use crate::state::{Pane, SplitOrientation, Surface, SurfaceKind, Workspace};
use crate::terminal_host::terminal_backend_status;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::thread;
use uuid::Uuid;

pub const DEFAULT_SOCKET_PATH: &str = "/tmp/cmux-linux.sock";

const WINDOW_MULTI_UNSUPPORTED_METHODS: &[&str] = &[];

const BROWSER_UNSUPPORTED_METHODS: &[&str] = &[
    "browser.open_split",
    "browser.navigate",
    "browser.back",
    "browser.forward",
    "browser.reload",
    "browser.url.get",
    "browser.snapshot",
    "browser.eval",
    "browser.wait",
    "browser.click",
    "browser.dblclick",
    "browser.hover",
    "browser.focus",
    "browser.type",
    "browser.fill",
    "browser.press",
    "browser.keydown",
    "browser.keyup",
    "browser.check",
    "browser.uncheck",
    "browser.select",
    "browser.scroll",
    "browser.scroll_into_view",
    "browser.screenshot",
    "browser.get.text",
    "browser.get.html",
    "browser.get.value",
    "browser.get.attr",
    "browser.get.title",
    "browser.get.count",
    "browser.get.box",
    "browser.get.styles",
    "browser.is.visible",
    "browser.is.enabled",
    "browser.is.checked",
    "browser.focus_webview",
    "browser.is_webview_focused",
    "browser.find.role",
    "browser.find.text",
    "browser.find.label",
    "browser.find.placeholder",
    "browser.find.alt",
    "browser.find.title",
    "browser.find.testid",
    "browser.find.first",
    "browser.find.last",
    "browser.find.nth",
    "browser.frame.select",
    "browser.frame.main",
    "browser.dialog.accept",
    "browser.dialog.dismiss",
    "browser.download.wait",
    "browser.cookies.get",
    "browser.cookies.set",
    "browser.cookies.clear",
    "browser.storage.get",
    "browser.storage.set",
    "browser.storage.clear",
    "browser.tab.new",
    "browser.tab.list",
    "browser.tab.switch",
    "browser.tab.close",
    "browser.console.list",
    "browser.console.clear",
    "browser.errors.list",
    "browser.highlight",
    "browser.state.save",
    "browser.state.load",
    "browser.addinitscript",
    "browser.addscript",
    "browser.addstyle",
    "browser.viewport.set",
    "browser.geolocation.set",
    "browser.offline.set",
    "browser.trace.start",
    "browser.trace.stop",
    "browser.network.route",
    "browser.network.unroute",
    "browser.network.requests",
    "browser.screencast.start",
    "browser.screencast.stop",
    "browser.input_mouse",
    "browser.input_keyboard",
    "browser.input_touch",
];

#[derive(Debug)]
pub struct SocketServerRuntime {
    pub socket_path: String,
}

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default = "default_null_value")]
    id: Value,
    method: String,
    #[serde(default)]
    params: Map<String, Value>,
}

#[derive(Debug)]
struct MethodError {
    code: &'static str,
    message: String,
    data: Option<Value>,
}

type MethodResult = Result<Value, MethodError>;

fn default_null_value() -> Value {
    Value::Null
}

impl MethodError {
    fn new(code: &'static str, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self::new("invalid_params", message, None)
    }

    fn not_found(message: impl Into<String>, data: Option<Value>) -> Self {
        Self::new("not_found", message, data)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new("unavailable", message, None)
    }

    fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message, None)
    }
}

pub fn configured_socket_path() -> String {
    std::env::var("CMUX_SOCKET_PATH")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("CMUX_SOCKET")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_SOCKET_PATH.to_string())
}

pub fn spawn_server(shared_model: SharedModel) -> io::Result<SocketServerRuntime> {
    let socket_path = {
        let model = shared_model
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "linux app model lock poisoned"))?;
        model.socket_path.clone()
    };

    if fs::metadata(&socket_path).is_ok() {
        let _ = fs::remove_file(&socket_path);
    }

    let listener = UnixListener::bind(&socket_path)?;
    thread::Builder::new()
        .name("cmux-linux-socket".to_string())
        .spawn(move || accept_loop(listener, shared_model))
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;

    Ok(SocketServerRuntime { socket_path })
}

pub fn remove_socket_file(socket_path: &str) {
    if fs::metadata(socket_path).is_ok() {
        let _ = fs::remove_file(socket_path);
    }
}

fn accept_loop(listener: UnixListener, shared_model: SharedModel) {
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else {
            continue;
        };
        let model = shared_model.clone();
        let _ = thread::Builder::new()
            .name("cmux-linux-socket-client".to_string())
            .spawn(move || handle_client(stream, model));
    }
}

fn handle_client(stream: UnixStream, shared_model: SharedModel) {
    let reader_stream = match stream.try_clone() {
        Ok(reader_stream) => reader_stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = match reader.read_line(&mut line) {
            Ok(bytes_read) => bytes_read,
            Err(_) => break,
        };
        if bytes_read == 0 {
            break;
        }

        let response = handle_request_line(&shared_model, &line);
        if writer.write_all(response.as_bytes()).is_err() {
            break;
        }
    }
}

pub fn handle_request_line(shared_model: &SharedModel, line: &str) -> String {
    let parsed = serde_json::from_str::<Request>(line.trim());
    let request = match parsed {
        Ok(request) => request,
        Err(error) => {
            return encode_response(error_response(
                Value::Null,
                "invalid_request",
                &format!("Invalid JSON request: {error}"),
                None,
            ));
        }
    };

    let mut model = match shared_model.lock() {
        Ok(model) => model,
        Err(_) => {
            return encode_response(error_response(
                request.id,
                "internal_error",
                "linux app model lock poisoned",
                None,
            ));
        }
    };

    encode_response(dispatch_request(&mut model, request))
}

fn encode_response(value: Value) -> String {
    match serde_json::to_string(&value) {
        Ok(encoded) => format!("{encoded}\n"),
        Err(_) => "{\"id\":null,\"ok\":false,\"error\":{\"code\":\"encode_error\",\"message\":\"Failed to encode JSON\"}}\n".to_string(),
    }
}

fn dispatch_request(model: &mut AppModel, request: Request) -> Value {
    let id = request.id.clone();
    let profile = merged_capability_profile();
    let advertised_methods = advertised_methods();

    if profile
        .unsupported_methods
        .contains(request.method.as_str())
    {
        return error_response(
            id,
            "not_supported",
            &format!(
                "{} is not supported on {}",
                request.method, profile.platform.frontend
            ),
            Some(json!({
                "method": request.method,
                "platform": profile.platform.id,
                "frontend": profile.platform.frontend,
            })),
        );
    }

    let result = match request.method.as_str() {
        "system.ping" => Ok(json!({ "pong": true })),
        "system.capabilities" => Ok(capabilities_payload(model, &profile, &advertised_methods)),
        "system.identify" => identify_payload(model, &request.params),
        "window.list" => window_list_payload(model),
        "window.current" => window_current_payload(model),
        "window.create" => window_create_payload(model),
        "window.focus" => window_focus_payload(model, &request.params),
        "window.close" => window_close_payload(model, &request.params),
        "workspace.list" => workspace_list_payload(model, &request.params),
        "workspace.create" => workspace_create_payload(model, &request.params),
        "workspace.rename" => workspace_rename_payload(model, &request.params),
        "workspace.reorder" => workspace_reorder_payload(model, &request.params),
        "workspace.select" => workspace_select_payload(model, &request.params),
        "workspace.current" => workspace_current_payload(model, &request.params),
        "workspace.next" => workspace_next_payload(model, &request.params),
        "workspace.previous" => workspace_previous_payload(model, &request.params),
        "workspace.last" => workspace_last_payload(model, &request.params),
        "workspace.move_to_window" => workspace_move_to_window_payload(model, &request.params),
        "workspace.close" => workspace_close_payload(model, &request.params),
        "pane.list" => pane_list_payload(model, &request.params),
        "pane.surfaces" => pane_surfaces_payload(model, &request.params),
        "pane.focus" => pane_focus_payload(model, &request.params),
        "pane.swap" => pane_swap_payload(model, &request.params),
        "pane.break" => pane_break_payload(model, &request.params),
        "pane.join" => pane_join_payload(model, &request.params),
        "pane.last" => pane_last_payload(model, &request.params),
        "pane.create" => pane_create_payload(model, &request.params),
        "surface.list" => surface_list_payload(model, &request.params),
        "surface.action" => surface_action_payload(model, &request.params),
        "surface.current" => surface_current_payload(model, &request.params),
        "surface.focus" => surface_focus_payload(model, &request.params),
        "surface.split" => surface_split_payload(model, &request.params),
        "surface.create" => surface_create_payload(model, &request.params),
        "surface.close" => surface_close_payload(model, &request.params),
        "surface.move" => surface_move_payload(model, &request.params),
        "surface.reorder" => surface_reorder_payload(model, &request.params),
        "surface.drag_to_split" => surface_drag_to_split_payload(model, &request.params),
        "surface.refresh" => surface_refresh_payload(model, &request.params),
        "surface.clear_history" => surface_clear_history_payload(model, &request.params),
        "surface.send_text" => surface_send_text_payload(model, &request.params),
        "surface.send_key" => surface_send_key_payload(model, &request.params),
        "surface.read_text" => surface_read_text_payload(model, &request.params),
        "surface.health" => surface_health_payload(model, &request.params),
        "surface.trigger_flash" => surface_trigger_flash_payload(model, &request.params),
        "notification.create" => notification_create_payload(model, &request.params),
        "notification.create_for_surface" => {
            notification_create_for_surface_payload(model, &request.params)
        }
        "notification.list" => notification_list_payload(model),
        "notification.clear" => notification_clear_payload(model),
        "app.focus_override.set" => app_focus_override_set_payload(model, &request.params),
        "app.simulate_active" => app_simulate_active_payload(model),
        "workspace.action" => workspace_action_payload(model, &request.params),
        "tab.action" => tab_action_payload(model, &request.params),
        "debug.app.activate" => debug_app_activate_payload(model),
        "debug.bonsplit_underflow.count" => Ok(json!({ "count": 0 })),
        "debug.bonsplit_underflow.reset" => Ok(json!({})),
        "debug.command_palette.rename_input.select_all" => {
            debug_command_palette_rename_select_all_payload(model, &request.params)
        }
        "debug.command_palette.rename_input.selection" => {
            debug_command_palette_rename_input_selection_payload(model, &request.params)
        }
        "debug.command_palette.rename_input.interact" => {
            debug_command_palette_rename_input_interact_payload(model, &request.params)
        }
        "debug.command_palette.rename_input.delete_backward" => {
            debug_command_palette_rename_input_delete_backward_payload(model, &request.params)
        }
        "debug.command_palette.rename_tab.open" => {
            debug_command_palette_rename_tab_open_payload(model, &request.params)
        }
        "debug.command_palette.results" => {
            debug_command_palette_results_payload(model, &request.params)
        }
        "debug.command_palette.selection" => {
            debug_command_palette_selection_payload(model, &request.params)
        }
        "debug.command_palette.toggle" => {
            debug_command_palette_toggle_payload(model, &request.params)
        }
        "debug.command_palette.visible" => {
            debug_command_palette_visible_payload(model, &request.params)
        }
        "debug.empty_panel.count" => Ok(json!({ "count": 0 })),
        "debug.empty_panel.reset" => Ok(json!({})),
        "debug.layout" => debug_layout_payload(model),
        "debug.notification.focus" => debug_notification_focus_payload(model, &request.params),
        "debug.panel_snapshot" => debug_panel_snapshot_payload(model, &request.params),
        "debug.panel_snapshot.reset" => Ok(json!({})),
        "debug.portal.stats" => debug_portal_stats_payload(),
        "debug.shortcut.set" => debug_shortcut_set_payload(model, &request.params),
        "debug.shortcut.simulate" => debug_shortcut_simulate_payload(model, &request.params),
        "debug.sidebar.visible" => debug_sidebar_visible_payload(model, &request.params),
        "debug.terminal.is_focused" => debug_terminal_is_focused_payload(model, &request.params),
        "debug.terminal.read_text" => debug_terminal_read_text_payload(model, &request.params),
        "debug.terminal.render_stats" => Ok(json!({ "stats": {} })),
        "debug.type" => debug_type_payload(model, &request.params),
        "debug.window.screenshot" => debug_window_screenshot_payload(&request.params),
        "debug.flash.count" => debug_flash_count_payload(model, &request.params),
        "debug.flash.reset" => debug_flash_reset_payload(model),
        _ if advertised_methods.contains(&request.method.as_str()) => {
            Err(MethodError::unavailable(format!(
                "{} is advertised but not implemented yet",
                request.method
            )))
        }
        _ => return error_response(id, "method_not_found", "Unknown method", None),
    };

    match result {
        Ok(payload) => ok_response(id, payload),
        Err(error) => error_response(id, error.code, &error.message, error.data),
    }
}

fn merged_capability_profile() -> CapabilityProfile {
    let mut profile = linux_v1_capabilities();
    let mut unsupported_methods: BTreeSet<&'static str> =
        WINDOW_MULTI_UNSUPPORTED_METHODS.iter().copied().collect();
    unsupported_methods.extend(BROWSER_UNSUPPORTED_METHODS.iter().copied());
    profile.unsupported_methods = unsupported_methods;
    profile
}

fn advertised_methods() -> Vec<&'static str> {
    let mut methods = vec![
        "system.ping",
        "system.capabilities",
        "system.identify",
        "window.list",
        "window.current",
        "window.focus",
        "window.create",
        "window.close",
        "workspace.list",
        "workspace.create",
        "workspace.rename",
        "workspace.reorder",
        "workspace.select",
        "workspace.current",
        "workspace.next",
        "workspace.previous",
        "workspace.last",
        "workspace.move_to_window",
        "workspace.action",
        "workspace.close",
        "pane.list",
        "pane.surfaces",
        "pane.swap",
        "pane.break",
        "pane.join",
        "pane.focus",
        "pane.last",
        "pane.create",
        "surface.list",
        "surface.current",
        "surface.action",
        "surface.focus",
        "surface.split",
        "surface.create",
        "surface.close",
        "surface.move",
        "surface.reorder",
        "surface.drag_to_split",
        "surface.refresh",
        "surface.clear_history",
        "surface.send_text",
        "surface.send_key",
        "surface.read_text",
        "surface.health",
        "surface.trigger_flash",
        "notification.create",
        "notification.create_for_surface",
        "notification.list",
        "notification.clear",
        "app.focus_override.set",
        "app.simulate_active",
        "debug.app.activate",
        "debug.bonsplit_underflow.count",
        "debug.bonsplit_underflow.reset",
        "debug.command_palette.rename_input.select_all",
        "debug.command_palette.rename_input.selection",
        "debug.command_palette.rename_input.interact",
        "debug.command_palette.rename_input.delete_backward",
        "debug.command_palette.rename_tab.open",
        "debug.command_palette.results",
        "debug.command_palette.selection",
        "debug.command_palette.toggle",
        "debug.command_palette.visible",
        "debug.empty_panel.count",
        "debug.empty_panel.reset",
        "debug.layout",
        "debug.notification.focus",
        "debug.panel_snapshot",
        "debug.panel_snapshot.reset",
        "debug.portal.stats",
        "debug.shortcut.set",
        "debug.shortcut.simulate",
        "debug.sidebar.visible",
        "debug.terminal.is_focused",
        "debug.terminal.read_text",
        "debug.terminal.render_stats",
        "debug.type",
        "debug.window.screenshot",
        "debug.flash.count",
        "debug.flash.reset",
        "tab.action",
    ];
    methods.extend(WINDOW_MULTI_UNSUPPORTED_METHODS.iter().copied());
    methods.extend(BROWSER_UNSUPPORTED_METHODS.iter().copied());
    methods.sort_unstable();
    methods.dedup();
    methods
}

fn capabilities_payload(
    model: &mut AppModel,
    profile: &CapabilityProfile,
    methods: &[&str],
) -> Value {
    let terminal_backend = terminal_backend_status();
    json!({
        "protocol": "cmux-socket",
        "version": 2,
        "socket_path": model.socket_path,
        "access_mode": "cmuxOnly",
        "platform": {
            "id": profile.platform.id,
            "frontend": profile.platform.frontend,
            "variant": "wayland-v1",
            "window_multi": profile.platform.window_multi,
        },
        "features": {
            "terminal": profile.features.terminal,
            "workspace": profile.features.workspace,
            "pane": profile.features.pane,
            "surface": profile.features.surface,
            "notification": profile.features.notification,
            "session_restore": profile.features.session_restore,
            "window_multi": profile.features.window_multi,
            "browser": profile.features.browser,
            "debug": profile.features.debug,
        },
        "terminal_backend": {
            "active": terminal_backend.active,
            "requested": terminal_backend.requested,
            "supported": terminal_backend.supported,
            "note": terminal_backend.note,
        },
        "unsupported_methods": profile.unsupported_methods.iter().copied().collect::<Vec<_>>(),
        "methods": methods,
    })
}

fn identify_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let snapshot = model.snapshot_state();
    let caller = params.get("caller").cloned().unwrap_or(Value::Null);
    let terminal_backend = terminal_backend_status();

    let focused = snapshot.selected_workspace().map(|workspace| {
        let pane_id = workspace.selected_pane_id;
        let surface_id = workspace.selected_surface_id();
        json!({
            "window_id": snapshot.window_id,
            "window_ref": model.window_ref(snapshot.window_id),
            "workspace_id": workspace.id,
            "workspace_ref": model.workspace_ref(workspace.id),
            "pane_id": pane_id,
            "pane_ref": model.pane_ref(pane_id),
            "surface_id": surface_id,
            "surface_ref": surface_id.map(|id| model.surface_ref(id)),
            "tab_id": surface_id,
            "tab_ref": surface_id.map(|id| model.tab_ref(id)),
            "surface_type": surface_id.and_then(|id| workspace.surface(id)).map(surface_type_name),
            "is_browser_surface": false,
        })
    });

    Ok(json!({
        "socket_path": model.socket_path,
        "terminal_backend": {
            "active": terminal_backend.active,
            "requested": terminal_backend.requested,
            "supported": terminal_backend.supported,
            "note": terminal_backend.note,
        },
        "focused": focused.unwrap_or(Value::Null),
        "caller": caller,
    }))
}

fn window_list_payload(model: &mut AppModel) -> MethodResult {
    let snapshot = model.snapshot_state();
    Ok(json!({
        "windows": snapshot
            .windows
            .iter()
            .enumerate()
            .map(|(index, window)| {
                let workspace_count = snapshot
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.window_id == window.id)
                    .count();
                json!({
                    "id": window.id,
                    "ref": model.window_ref(window.id),
                    "index": index,
                    "key": window.id == snapshot.window_id,
                    "visible": true,
                    "workspace_count": workspace_count,
                    "selected_workspace_id": window.selected_workspace_id,
                    "selected_workspace_ref": (!window.selected_workspace_id.is_nil())
                        .then(|| model.workspace_ref(window.selected_workspace_id)),
                })
            })
            .collect::<Vec<_>>()
    }))
}

fn window_current_payload(model: &mut AppModel) -> MethodResult {
    let snapshot = model.snapshot_state();
    Ok(json!({
        "window_id": snapshot.window_id,
        "window_ref": model.window_ref(snapshot.window_id),
    }))
}

fn window_create_payload(model: &mut AppModel) -> MethodResult {
    let (window_id, workspace_id) = model.state.create_window_with_focus(true);
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn window_focus_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = require_window_id(model, params, "window_id")?;
    let workspace_id = model
        .state
        .focus_window(window_id)
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn window_close_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = require_window_id(model, params, "window_id")?;
    model
        .state
        .close_window(window_id)
        .map_err(MethodError::internal_error)?;
    let snapshot = model.snapshot_state();
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "current_window_id": snapshot.window_id,
        "current_window_ref": model.window_ref(snapshot.window_id),
    }))
}

fn workspace_list_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let snapshot = model.snapshot_state();

    let workspaces = snapshot
        .workspaces
        .iter()
        .filter(|workspace| workspace.window_id == window_id)
        .enumerate()
        .map(|(index, workspace)| {
            json!({
                "id": workspace.id,
                "ref": model.workspace_ref(workspace.id),
                "index": index,
                "title": workspace.title,
                "selected": snapshot
                    .window(window_id)
                    .map(|window| workspace.id == window.selected_workspace_id)
                    .unwrap_or(false),
                "pinned": false,
                "current_directory": workspace.current_directory,
                "custom_color": Value::Null,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspaces": workspaces,
    }))
}

fn workspace_create_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let cwd = optional_string_param(params, "cwd")?;

    let workspace_id = model
        .state
        .create_workspace_in_window_with_focus_and_cwd(window_id, false, cwd);
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_rename_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| MethodError::invalid_params("Missing or invalid title"))?
        .to_string();
    model
        .state
        .rename_workspace(workspace_id, title)
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_reorder_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    let window_id = if param_string(params, "window_id").is_some() {
        require_window_id(model, params, "window_id")?
    } else {
        model
            .state
            .workspace_window_id(workspace_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?
    };
    let target_index = workspace_target_index(model, workspace_id, window_id, params)?;
    model
        .state
        .reorder_workspace_in_window(workspace_id, window_id, target_index)
        .map_err(MethodError::invalid_params)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_select_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    model
        .state
        .select_workspace(workspace_id)
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_current_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let snapshot = model.snapshot_state();
    let selected_workspace_id = snapshot
        .window(window_id)
        .map(|window| window.selected_workspace_id)
        .unwrap_or(Uuid::nil());
    if selected_workspace_id.is_nil() {
        return Err(MethodError::not_found("No workspace selected", None));
    }

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": selected_workspace_id,
        "workspace_ref": model.workspace_ref(selected_workspace_id),
        "current_directory": snapshot
            .workspace(selected_workspace_id)
            .and_then(|workspace| workspace.current_directory.clone()),
    }))
}

fn workspace_next_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    if model.state.window_id != window_id {
        model
            .state
            .focus_window(window_id)
            .map_err(MethodError::internal_error)?;
    }
    let workspace_id = model
        .state
        .select_next_workspace()
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_previous_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    if model.state.window_id != window_id {
        model
            .state
            .focus_window(window_id)
            .map_err(MethodError::internal_error)?;
    }
    let workspace_id = model
        .state
        .select_previous_workspace()
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_last_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    if model.state.window_id != window_id {
        model
            .state
            .focus_window(window_id)
            .map_err(MethodError::internal_error)?;
    }
    let workspace_id = model
        .state
        .select_last_workspace()
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn workspace_move_to_window_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    let target_window_id = require_window_id(model, params, "window_id")?;
    let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(true);
    model
        .state
        .move_workspace_to_window(workspace_id, target_window_id, focus)
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": target_window_id,
        "window_ref": model.window_ref(target_window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "focused": focus,
    }))
}

fn workspace_close_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| {
            MethodError::not_found(
                "Workspace not found",
                Some(json!({ "workspace_id": workspace_id })),
            )
        })?;
    model
        .state
        .close_workspace(workspace_id)
        .map_err(|message| {
            MethodError::not_found(message, Some(json!({ "workspace_id": workspace_id })))
        })?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn pane_list_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspace(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    let window_id = workspace.window_id;

    let panes = workspace
        .ordered_panes()
        .into_iter()
        .enumerate()
        .map(|(index, pane)| {
            json!({
                "id": pane.id,
                "ref": model.pane_ref(pane.id),
                "index": index,
                "focused": pane.id == workspace.selected_pane_id && workspace.id == snapshot.selected_workspace_id,
                "surface_ids": pane.surfaces.iter().map(|surface| surface.id).collect::<Vec<_>>(),
                "surface_refs": pane.surfaces.iter().map(|surface| model.surface_ref(surface.id)).collect::<Vec<_>>(),
                "selected_surface_id": pane.selected_surface_id,
                "selected_surface_ref": model.surface_ref(pane.selected_surface_id),
                "surface_count": pane.surfaces.len(),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace.id,
        "workspace_ref": model.workspace_ref(workspace.id),
        "panes": panes,
    }))
}

fn pane_surfaces_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let pane_id = if let Some(raw_pane_id) = param_string(params, "pane_id") {
        model
            .resolve_pane_id(raw_pane_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid pane_id"))?
    } else {
        let workspace_id = workspace_target_id(model, params)?;
        model
            .state
            .workspace(workspace_id)
            .map(|workspace| workspace.selected_pane_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?
    };
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.pane(pane_id).is_some())
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
    let pane = workspace
        .pane(pane_id)
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;

    Ok(json!({
        "window_id": workspace.window_id,
        "window_ref": model.window_ref(workspace.window_id),
        "workspace_id": workspace.id,
        "workspace_ref": model.workspace_ref(workspace.id),
        "pane_id": pane.id,
        "pane_ref": model.pane_ref(pane.id),
        "surfaces": pane
            .surfaces
            .iter()
            .enumerate()
            .map(|(index, surface)| {
                json!({
                    "index": index,
                    "id": surface.id,
                    "ref": model.surface_ref(surface.id),
                    "title": surface.title,
                    "selected": pane.selected_surface_id == surface.id,
                })
            })
            .collect::<Vec<_>>(),
    }))
}

fn pane_swap_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let pane_id = require_pane_id(model, params, "pane_id")?;
    let target_pane_id = require_pane_id(model, params, "target_pane_id")?;
    let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(true);
    let workspace_id = model
        .state
        .swap_panes(pane_id, target_pane_id, focus)
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "target_pane_id": target_pane_id,
        "target_pane_ref": model.pane_ref(target_pane_id),
    }))
}

fn pane_break_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = if let Some(surface_id) = optional_surface_id(model, params, "surface_id")? {
        surface_id
    } else if let Some(raw_pane_id) = param_string(params, "pane_id") {
        let pane_id = model
            .resolve_pane_id(raw_pane_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid pane_id"))?;
        let snapshot = model.snapshot_state();
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.pane(pane_id).is_some())
            .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
        workspace
            .pane(pane_id)
            .map(|pane| pane.selected_surface_id)
            .ok_or_else(|| MethodError::not_found("Pane not found", None))?
    } else {
        surface_target_id(model, params)?
    };
    let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(true);
    let workspace_id = model
        .state
        .break_surface_to_workspace(surface_id, focus)
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn pane_join_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let target_pane_id = require_pane_id(model, params, "target_pane_id")?;
    let surface_id = if let Some(surface_id) = optional_surface_id(model, params, "surface_id")? {
        surface_id
    } else if let Some(raw_pane_id) = param_string(params, "pane_id") {
        let pane_id = model
            .resolve_pane_id(raw_pane_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid pane_id"))?;
        let snapshot = model.snapshot_state();
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.pane(pane_id).is_some())
            .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
        workspace
            .pane(pane_id)
            .map(|pane| pane.selected_surface_id)
            .ok_or_else(|| MethodError::not_found("Pane not found", None))?
    } else {
        surface_target_id(model, params)?
    };
    let destination_workspace_id = model
        .state
        .workspaces
        .iter()
        .find(|workspace| workspace.pane(target_pane_id).is_some())
        .map(|workspace| workspace.id)
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
    let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(true);
    let target_index = model
        .state
        .workspace(destination_workspace_id)
        .and_then(|workspace| workspace.pane(target_pane_id))
        .map(|pane| pane.surfaces.len())
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
    let (workspace_id, pane_id, surface_id) = model
        .state
        .move_surface_to_pane(
            surface_id,
            destination_workspace_id,
            target_pane_id,
            target_index,
            focus,
        )
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn pane_focus_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let pane_id = require_pane_id(model, params, "pane_id")?;
    let workspace_id = model
        .state
        .focus_pane(pane_id)
        .map_err(|message| MethodError::not_found(message, Some(json!({ "pane_id": pane_id }))))?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
    }))
}

fn pane_last_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    if workspace_id != model.state.selected_workspace_id {
        model
            .state
            .select_workspace(workspace_id)
            .map_err(MethodError::internal_error)?;
    }
    let (workspace_id, pane_id) = model
        .state
        .focus_last_pane()
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
    }))
}

fn pane_create_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let (orientation, insert_first) = parse_direction(params)?;
    ensure_terminal_type_supported(params)?;
    let workspace_id = workspace_target_id(model, params)?;
    let surface_id = {
        let snapshot = model.snapshot_state();
        let workspace = snapshot
            .workspace(workspace_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
        workspace
            .selected_surface_id()
            .ok_or_else(|| MethodError::not_found("No focused surface to split", None))?
    };
    let (workspace_id, pane_id, surface_id) = model
        .state
        .split_surface_with_focus(surface_id, orientation, insert_first, false)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "type": "terminal",
    }))
}

fn surface_list_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspace(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    let current_surface_id = workspace.selected_surface_id();
    let window_id = workspace.window_id;

    let surfaces = workspace_surface_rows(workspace)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            json!({
                "id": row.surface.id,
                "ref": model.surface_ref(row.surface.id),
                "index": index,
                "type": surface_type_name(row.surface),
                "title": row.surface.title,
                "unread": row.surface.unread,
                "flash_count": row.surface.flash_count,
                "focused": Some(row.surface.id) == current_surface_id && workspace.id == snapshot.selected_workspace_id,
                "pane_id": row.pane.id,
                "pane_ref": model.pane_ref(row.pane.id),
                "index_in_pane": row.index_in_pane,
                "selected_in_pane": row.pane.selected_surface_id == row.surface.id,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace.id,
        "workspace_ref": model.workspace_ref(workspace.id),
        "surfaces": surfaces,
    }))
}

fn surface_current_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspace(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    let surface_id = workspace
        .selected_surface_id()
        .ok_or_else(|| MethodError::not_found("No surface selected", None))?;
    let pane_id = workspace
        .pane_id_for_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;

    let window_id = workspace.window_id;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace.id,
        "workspace_ref": model.workspace_ref(workspace.id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "surface_type": "terminal",
    }))
}

fn surface_focus_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let (workspace_id, pane_id) = model.state.focus_surface(surface_id).map_err(|message| {
        MethodError::not_found(message, Some(json!({ "surface_id": surface_id })))
    })?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn surface_split_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let (orientation, insert_first) = parse_direction(params)?;
    let surface_id = if let Some(raw_surface_id) = param_string(params, "surface_id") {
        model
            .resolve_surface_id(raw_surface_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid surface_id"))?
    } else {
        let workspace_id = workspace_target_id(model, params)?;
        let snapshot = model.snapshot_state();
        snapshot
            .workspace(workspace_id)
            .and_then(Workspace::selected_surface_id)
            .ok_or_else(|| MethodError::not_found("No focused surface", None))?
    };

    let (workspace_id, pane_id, surface_id) = model
        .state
        .split_surface_with_focus(surface_id, orientation, insert_first, false)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "type": "terminal",
    }))
}

fn surface_create_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    ensure_terminal_type_supported(params)?;
    let workspace_id = workspace_target_id(model, params)?;
    let pane_id = if let Some(raw_pane_id) = param_string(params, "pane_id") {
        model
            .resolve_pane_id(raw_pane_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid pane_id"))?
    } else {
        let snapshot = model.snapshot_state();
        snapshot
            .workspace(workspace_id)
            .map(|workspace| workspace.selected_pane_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?
    };

    let (workspace_id, pane_id, surface_id) = model
        .state
        .create_surface_in_pane_with_focus(workspace_id, pane_id, false)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "type": "terminal",
    }))
}

fn surface_close_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    let surface_id = if let Some(raw_surface_id) = param_string(params, "surface_id") {
        model
            .resolve_surface_id(raw_surface_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid surface_id"))?
    } else {
        let snapshot = model.snapshot_state();
        snapshot
            .workspace(workspace_id)
            .and_then(Workspace::selected_surface_id)
            .ok_or_else(|| MethodError::not_found("No focused surface", None))?
    };

    let (_, pane_id, _) = model
        .state
        .close_surface(workspace_id, surface_id)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn surface_move_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let (source_workspace_id, _) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let destination_workspace_id = if let Some(raw_pane_id) = param_string(params, "pane_id") {
        let pane_id = model
            .resolve_pane_id(raw_pane_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid pane_id"))?;
        model
            .state
            .workspaces
            .iter()
            .find(|workspace| workspace.pane(pane_id).is_some())
            .map(|workspace| workspace.id)
            .ok_or_else(|| MethodError::not_found("Pane not found", None))?
    } else if let Some(raw_workspace_id) = param_string(params, "workspace_id") {
        model
            .resolve_workspace_id(raw_workspace_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid workspace_id"))?
    } else if let Some(raw_window_id) = param_string(params, "window_id") {
        let window_id = model
            .resolve_window_id(raw_window_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid window_id"))?;
        model
            .state
            .window(window_id)
            .map(|window| window.selected_workspace_id)
            .filter(|workspace_id| !workspace_id.is_nil())
            .ok_or_else(|| MethodError::not_found("Window has no selected workspace", None))?
    } else {
        source_workspace_id
    };
    let destination_pane_id = if let Some(raw_pane_id) = param_string(params, "pane_id") {
        model
            .resolve_pane_id(raw_pane_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid pane_id"))?
    } else {
        model
            .state
            .workspace(destination_workspace_id)
            .map(|workspace| workspace.selected_pane_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?
    };
    let target_index = surface_target_index(model, surface_id, destination_pane_id, params)?;
    let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(true);
    let (workspace_id, pane_id, surface_id) = model
        .state
        .move_surface_to_pane(
            surface_id,
            destination_workspace_id,
            destination_pane_id,
            target_index,
            focus,
        )
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn surface_reorder_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let (workspace_id, pane_id) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let target_index = surface_target_index(model, surface_id, pane_id, params)?;
    let (workspace_id, pane_id, surface_id) = model
        .state
        .move_surface_to_pane(surface_id, workspace_id, pane_id, target_index, false)
        .map_err(MethodError::invalid_params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn surface_drag_to_split_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    surface_split_payload(model, params)
}

fn surface_refresh_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn surface_clear_history_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    if let Some(surface_id) = optional_surface_id(model, params, "surface_id")? {
        model
            .state
            .clear_surface_history(surface_id)
            .map_err(MethodError::invalid_params)?;
        let (workspace_id, _) = model
            .state
            .locate_surface(surface_id)
            .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
        let window_id = model
            .state
            .workspace_window_id(workspace_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
        return Ok(json!({
            "window_id": window_id,
            "window_ref": model.window_ref(window_id),
            "workspace_id": workspace_id,
            "workspace_ref": model.workspace_ref(workspace_id),
            "surface_id": surface_id,
            "surface_ref": model.surface_ref(surface_id),
        }));
    }

    let workspace_id = workspace_target_id(model, params)?;
    let surface_ids = model
        .state
        .workspace(workspace_id)
        .map(|workspace| {
            workspace
                .panes
                .iter()
                .flat_map(|pane| pane.surfaces.iter())
                .map(|surface| surface.id)
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    for surface_id in surface_ids {
        let _ = model.state.clear_surface_history(surface_id);
    }
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn surface_send_text_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let text = raw_string_param(params, "text")
        .ok_or_else(|| MethodError::invalid_params("Missing text"))?
        .to_string();
    let surface_id = surface_target_id(model, params)?;
    let (workspace_id, _) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    model
        .terminal_bridge
        .send_text(surface_id, text)
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "queued": true,
    }))
}

fn surface_send_key_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let key =
        param_string(params, "key").ok_or_else(|| MethodError::invalid_params("Missing key"))?;
    let surface_id = surface_target_id(model, params)?;
    let text = translate_terminal_key(key)?;
    let (workspace_id, _) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    model
        .terminal_bridge
        .send_text(surface_id, text)
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn surface_read_text_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let line_limit = param_usize(params, "lines")?;
    if let Some(line_limit) = line_limit {
        if line_limit == 0 {
            return Err(MethodError::invalid_params("lines must be greater than 0"));
        }
    }
    let surface_id = surface_target_id(model, params)?;
    let (workspace_id, text) = model
        .state
        .read_terminal_text(surface_id, line_limit)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "text": text,
        "base64": BASE64_STANDARD.encode(text.as_bytes()),
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
    }))
}

fn surface_health_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = workspace_target_id(model, params)?;
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspace(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    let window_id = workspace.window_id;
    let surfaces = workspace_surface_rows(workspace)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            json!({
                "index": index,
                "id": row.surface.id,
                "ref": model.surface_ref(row.surface.id),
                "type": surface_type_name(row.surface),
                "in_window": true,
                "terminal_health": {
                    "realized": row.surface.terminal_health.realized,
                    "startup_error": row.surface.terminal_health.startup_error,
                    "io_thread_main_started": row.surface.terminal_health.io_thread_main_started,
                    "io_thread_entered": row.surface.terminal_health.io_thread_entered,
                    "subprocess_start_attempted": row.surface.terminal_health.subprocess_start_attempted,
                    "child_pid": row.surface.terminal_health.child_pid,
                    "child_exited": row.surface.terminal_health.child_exited,
                    "child_exit_code": row.surface.terminal_health.child_exit_code,
                    "child_runtime_ms": row.surface.terminal_health.child_runtime_ms,
                },
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surfaces": surfaces,
    }))
}

fn surface_trigger_flash_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let surface_id = surface_target_id(model, params)?;
    let (workspace_id, _, flash_count) = model
        .state
        .trigger_flash(surface_id)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "flash_count": flash_count,
    }))
}

fn notification_create_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Notification")
        .to_string();
    let subtitle = params
        .get("subtitle")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = params
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let workspace_id = workspace_target_id(model, params)?;
    let surface_id = {
        let snapshot = model.snapshot_state();
        snapshot
            .workspace(workspace_id)
            .and_then(Workspace::selected_surface_id)
    };
    if should_suppress_notification(model, workspace_id, surface_id) {
        return Ok(json!({
            "notification_id": Value::Null,
            "workspace_id": workspace_id,
            "workspace_ref": model.workspace_ref(workspace_id),
            "surface_id": surface_id,
            "surface_ref": surface_id.map(|id| model.surface_ref(id)),
            "delivered": false,
            "suppressed": true,
        }));
    }
    let delivered = deliver_notification(&title, &subtitle, &body);
    let notification_id = model
        .state
        .create_notification(workspace_id, surface_id, title, subtitle, body, delivered)
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "notification_id": notification_id,
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": surface_id.map(|id| model.surface_ref(id)),
        "delivered": delivered,
    }))
}

fn notification_create_for_surface_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let title = params
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Notification")
        .to_string();
    let subtitle = params
        .get("subtitle")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = params
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let (workspace_id, _) = model.state.locate_surface(surface_id).ok_or_else(|| {
        MethodError::not_found(
            "Surface not found",
            Some(json!({ "surface_id": surface_id })),
        )
    })?;
    if should_suppress_notification(model, workspace_id, Some(surface_id)) {
        return Ok(json!({
            "notification_id": Value::Null,
            "workspace_id": workspace_id,
            "workspace_ref": model.workspace_ref(workspace_id),
            "surface_id": surface_id,
            "surface_ref": model.surface_ref(surface_id),
            "delivered": false,
            "suppressed": true,
        }));
    }
    let delivered = deliver_notification(&title, &subtitle, &body);
    let notification_id = model
        .state
        .create_notification(
            workspace_id,
            Some(surface_id),
            title,
            subtitle,
            body,
            delivered,
        )
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "notification_id": notification_id,
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "delivered": delivered,
    }))
}

fn notification_list_payload(model: &mut AppModel) -> MethodResult {
    let snapshot = model.snapshot_state();
    let notifications = snapshot
        .notifications
        .iter()
        .map(|notification| {
            json!({
                "id": notification.id,
                "workspace_id": notification.workspace_id,
                "surface_id": notification.surface_id,
                "is_read": notification.is_read,
                "title": notification.title,
                "subtitle": notification.subtitle,
                "body": notification.body,
                "delivered": notification.delivered,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({ "notifications": notifications }))
}

fn notification_clear_payload(model: &mut AppModel) -> MethodResult {
    model.state.clear_notifications();
    Ok(json!({}))
}

fn app_focus_override_set_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let state = param_string(params, "state")
        .ok_or_else(|| MethodError::invalid_params("Missing or invalid state"))?;
    model.app_focus_override = match state {
        "active" => Some(AppFocusOverride::Active),
        "inactive" => Some(AppFocusOverride::Inactive),
        "clear" => None,
        _ => {
            return Err(MethodError::invalid_params(
                "state must be active, inactive, or clear",
            ))
        }
    };
    Ok(json!({
        "state": state,
    }))
}

fn app_simulate_active_payload(model: &mut AppModel) -> MethodResult {
    model.app_focus_override = Some(AppFocusOverride::Active);
    let workspace_id = model.state.selected_workspace_id;
    let window_id = model.state.window_id;
    if !workspace_id.is_nil() {
        let _ = model.state.select_workspace(workspace_id);
    }
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": (!workspace_id.is_nil()).then(|| model.workspace_ref(workspace_id)),
    }))
}

fn debug_app_activate_payload(model: &mut AppModel) -> MethodResult {
    app_simulate_active_payload(model)
}

#[derive(Debug, Clone)]
struct PaletteRow {
    command_id: String,
    title: String,
    trailing_label: Option<String>,
    shortcut_hint: Option<String>,
    surface_id: Option<String>,
    workspace_id: Option<String>,
    search_terms: Vec<String>,
    kind_rank: usize,
    order: usize,
}

fn json_map_from_pairs<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn palette_effective_mode(state: &CommandPaletteState) -> CommandPaletteMode {
    if state.mode != CommandPaletteMode::RenameInput && state.text.trim_start().starts_with('>') {
        CommandPaletteMode::Commands
    } else {
        state.mode
    }
}

fn palette_query_text(state: &CommandPaletteState) -> String {
    match palette_effective_mode(state) {
        CommandPaletteMode::Commands => state
            .text
            .trim_start()
            .strip_prefix('>')
            .unwrap_or(state.text.as_str())
            .trim_start()
            .to_string(),
        _ => state.text.clone(),
    }
}

fn palette_normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut previous_was_space = false;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            normalized.push(character);
            previous_was_space = false;
        } else if !previous_was_space {
            normalized.push(' ');
            previous_was_space = true;
        }
    }
    normalized.trim().to_string()
}

fn palette_subsequence_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let mut score = 0i32;
    let mut candidate_iter = candidate.chars().enumerate();
    let mut previous_index = None;
    for query_char in query.chars() {
        let (index, _) =
            candidate_iter.find(|(_, candidate_char)| *candidate_char == query_char)?;
        score += match previous_index {
            Some(previous) if index == previous + 1 => 18,
            Some(previous) => (10 - (index.saturating_sub(previous + 1) as i32)).max(1),
            None => (15 - index as i32).max(1),
        };
        previous_index = Some(index);
    }
    Some(score)
}

fn palette_match_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }

    let normalized_query = palette_normalize(query);
    let normalized_candidate = palette_normalize(candidate);
    if normalized_query.is_empty() {
        return Some(0);
    }

    let compact_query = normalized_query.replace(' ', "");
    let compact_candidate = normalized_candidate.replace(' ', "");

    if compact_candidate == compact_query {
        return Some(1_000);
    }
    if compact_candidate.starts_with(&compact_query) {
        return Some(900 - compact_candidate.len() as i32);
    }
    if let Some(position) = compact_candidate.find(&compact_query) {
        return Some(800 - position as i32);
    }
    if normalized_candidate.starts_with(&normalized_query) {
        return Some(760 - normalized_candidate.len() as i32);
    }
    if let Some(position) = normalized_candidate.find(&normalized_query) {
        return Some(700 - position as i32);
    }

    palette_subsequence_score(&compact_query, &compact_candidate).map(|score| 500 + score)
}

fn palette_score_row(query: &str, row: &PaletteRow) -> Option<i32> {
    if query.trim().is_empty() {
        return Some(0);
    }

    row.search_terms
        .iter()
        .filter_map(|term| palette_match_score(query, term))
        .max()
}

fn current_window_workspace_id(model: &AppModel, window_id: Uuid) -> Option<Uuid> {
    model
        .state
        .window(window_id)
        .map(|window| window.selected_workspace_id)
        .filter(|workspace_id| !workspace_id.is_nil())
}

fn current_window_surface_id(model: &AppModel, window_id: Uuid) -> Option<Uuid> {
    let workspace_id = current_window_workspace_id(model, window_id)?;
    model
        .state
        .workspace(workspace_id)
        .and_then(Workspace::selected_surface_id)
}

fn command_palette_command_rows(model: &mut AppModel, window_id: Uuid) -> Vec<PaletteRow> {
    let workspace_id = current_window_workspace_id(model, window_id);
    let surface_id = current_window_surface_id(model, window_id);
    let mut order = 0usize;
    let mut push = |rows: &mut Vec<PaletteRow>,
                    command_id: &str,
                    title: &str,
                    shortcut_name: Option<&str>,
                    search_terms: &[&str]| {
        rows.push(PaletteRow {
            command_id: command_id.to_string(),
            title: title.to_string(),
            trailing_label: None,
            shortcut_hint: shortcut_name
                .map(|name| model.shortcut_hint(name))
                .filter(|hint| !hint.is_empty()),
            surface_id: surface_id.map(|id| id.to_string()),
            workspace_id: workspace_id.map(|id| id.to_string()),
            search_terms: search_terms
                .iter()
                .map(|term| (*term).to_string())
                .collect(),
            kind_rank: 0,
            order,
        });
        order += 1;
    };

    let mut rows = Vec::new();
    push(
        &mut rows,
        "palette.newWindow",
        "New Window",
        Some("new_window"),
        &["new window", "window new", "create window"],
    );
    push(
        &mut rows,
        "palette.closeWindow",
        "Close Window",
        Some("close_window"),
        &["close window", "window close"],
    );
    push(
        &mut rows,
        "palette.renameTab",
        "Rename Tab",
        Some("rename_tab"),
        &["rename tab", "tab rename", "retab", "rename terminal"],
    );
    push(
        &mut rows,
        "palette.renameWorkspace",
        "Rename Workspace",
        None,
        &["rename workspace", "workspace rename", "rename ws"],
    );
    push(
        &mut rows,
        "palette.terminalOpenDirectory",
        "Open Directory in Terminal",
        None,
        &[
            "open directory",
            "open terminal",
            "terminal open directory",
            "open",
        ],
    );
    push(
        &mut rows,
        "palette.newWorkspace",
        "New Workspace",
        None,
        &["new workspace", "workspace new", "create workspace"],
    );
    push(
        &mut rows,
        "palette.nextWorkspace",
        "Next Workspace",
        None,
        &["next workspace", "workspace next"],
    );
    push(
        &mut rows,
        "palette.previousWorkspace",
        "Previous Workspace",
        None,
        &["previous workspace", "workspace previous"],
    );
    push(
        &mut rows,
        "palette.splitRight",
        "Split Right",
        None,
        &["split right", "right split", "vertical split"],
    );
    push(
        &mut rows,
        "palette.splitDown",
        "Split Down",
        None,
        &["split down", "down split", "horizontal split"],
    );
    push(
        &mut rows,
        "palette.newTerminal",
        "New Terminal",
        None,
        &["new terminal", "terminal new", "new tab"],
    );
    push(
        &mut rows,
        "palette.closeTab",
        "Close Tab",
        None,
        &["close tab", "close terminal", "tab close"],
    );
    push(
        &mut rows,
        "palette.clearTerminalHistory",
        "Clear Terminal History",
        None,
        &[
            "clear terminal history",
            "clear history",
            "terminal history",
        ],
    );
    push(
        &mut rows,
        "palette.focusLastPane",
        "Focus Last Pane",
        None,
        &["focus last pane", "last pane", "pane last"],
    );
    rows
}

fn command_palette_switcher_rows(model: &mut AppModel) -> Vec<PaletteRow> {
    let snapshot = model.snapshot_state();
    let mut rows = Vec::new();
    let mut order = 0usize;

    for workspace in &snapshot.workspaces {
        rows.push(PaletteRow {
            command_id: format!(
                "switcher.workspace.{}",
                workspace.id.to_string().to_ascii_lowercase()
            ),
            title: workspace.title.clone(),
            trailing_label: Some("Workspace".to_string()),
            shortcut_hint: None,
            surface_id: None,
            workspace_id: Some(workspace.id.to_string()),
            search_terms: vec![
                workspace.title.clone(),
                workspace.current_directory.clone().unwrap_or_default(),
            ],
            kind_rank: 1,
            order,
        });
        order += 1;

        for row in workspace_surface_rows(workspace) {
            rows.push(PaletteRow {
                command_id: format!(
                    "switcher.surface.{}.{}",
                    workspace.id.to_string().to_ascii_lowercase(),
                    row.surface.id.to_string().to_ascii_lowercase()
                ),
                title: row.surface.title.clone(),
                trailing_label: Some("Surface".to_string()),
                shortcut_hint: None,
                surface_id: Some(row.surface.id.to_string()),
                workspace_id: Some(workspace.id.to_string()),
                search_terms: vec![
                    row.surface.title.clone(),
                    workspace.title.clone(),
                    row.surface.current_directory.clone().unwrap_or_default(),
                    workspace.current_directory.clone().unwrap_or_default(),
                ],
                kind_rank: 0,
                order,
            });
            order += 1;
        }
    }

    rows
}

fn command_palette_rows(
    model: &mut AppModel,
    window_id: Uuid,
    state: &CommandPaletteState,
) -> Vec<PaletteRow> {
    let effective_mode = palette_effective_mode(state);
    let query = palette_query_text(state);
    let mut rows = match effective_mode {
        CommandPaletteMode::Commands => command_palette_command_rows(model, window_id),
        CommandPaletteMode::Switcher => command_palette_switcher_rows(model),
        CommandPaletteMode::RenameInput => Vec::new(),
    };

    rows.retain(|row| palette_score_row(&query, row).is_some());
    rows.sort_by(|left, right| {
        let left_score = palette_score_row(&query, left).unwrap_or_default();
        let right_score = palette_score_row(&query, right).unwrap_or_default();
        right_score
            .cmp(&left_score)
            .then_with(|| left.kind_rank.cmp(&right.kind_rank))
            .then_with(|| left.order.cmp(&right.order))
    });
    rows
}

fn palette_move_selection(model: &mut AppModel, window_id: Uuid, delta: isize) {
    let palette = model.palette_state(window_id);
    let row_count = command_palette_rows(model, window_id, &palette).len();
    let state = model.palette_state_mut(window_id);
    if row_count == 0 {
        state.selected_index = 0;
        return;
    }

    let current = state.selected_index.min(row_count.saturating_sub(1)) as isize;
    let next = (current + delta).rem_euclid(row_count as isize) as usize;
    state.selected_index = next;
}

fn palette_current_row(model: &mut AppModel, window_id: Uuid) -> Option<PaletteRow> {
    let state = model.palette_state(window_id);
    let rows = command_palette_rows(model, window_id, &state);
    let index = state.selected_index.min(rows.len().saturating_sub(1));
    rows.get(index).cloned()
}

fn debug_command_palette_rename_tab_open_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let surface_id = current_window_surface_id(model, window_id)
        .ok_or_else(|| MethodError::not_found("No focused surface", None))?;
    let (workspace_id, _) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let title = model
        .state
        .workspace(workspace_id)
        .and_then(|workspace| workspace.surface(surface_id))
        .map(|surface| surface.title.clone())
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    model.open_palette_rename_input(window_id, RenameTarget::Surface(surface_id), title);
    debug_command_palette_visible_payload(
        model,
        &json_map_from_pairs([("window_id", json!(window_id))]),
    )
}

fn execute_command_palette_row(
    model: &mut AppModel,
    window_id: Uuid,
    row: &PaletteRow,
) -> MethodResult {
    match row.command_id.as_str() {
        "palette.newWindow" => {
            model.close_palette(window_id);
            window_create_payload(model)
        }
        "palette.closeWindow" => {
            model.close_palette(window_id);
            window_close_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "palette.renameTab" => {
            let surface_id = current_window_surface_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No focused surface", None))?;
            let title = model
                .state
                .locate_surface(surface_id)
                .and_then(|(workspace_id, _)| model.state.workspace(workspace_id))
                .and_then(|workspace| workspace.surface(surface_id))
                .map(|surface| surface.title.clone())
                .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
            model.open_palette_rename_input(window_id, RenameTarget::Surface(surface_id), title);
            debug_command_palette_visible_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "palette.renameWorkspace" => {
            let workspace_id = current_window_workspace_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No workspace selected", None))?;
            let title = model
                .state
                .workspace(workspace_id)
                .map(|workspace| workspace.title.clone())
                .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
            model.open_palette_rename_input(
                window_id,
                RenameTarget::Workspace(workspace_id),
                title,
            );
            debug_command_palette_visible_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "palette.terminalOpenDirectory" => {
            model.close_palette(window_id);
            let cwd = current_window_workspace_id(model, window_id)
                .and_then(|workspace_id| model.state.workspace(workspace_id))
                .and_then(|workspace| workspace.current_directory.clone());
            workspace_create_payload(
                model,
                &json_map_from_pairs([
                    ("window_id", json!(window_id)),
                    ("cwd", cwd.map(Value::String).unwrap_or(Value::Null)),
                ]),
            )
        }
        "palette.newWorkspace" => {
            model.close_palette(window_id);
            workspace_create_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "palette.nextWorkspace" => {
            model.close_palette(window_id);
            workspace_next_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "palette.previousWorkspace" => {
            model.close_palette(window_id);
            workspace_previous_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "palette.splitRight" => {
            model.close_palette(window_id);
            let surface_id = current_window_surface_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No focused surface", None))?;
            surface_split_payload(
                model,
                &json_map_from_pairs([
                    ("surface_id", json!(surface_id)),
                    ("direction", json!("right")),
                ]),
            )
        }
        "palette.splitDown" => {
            model.close_palette(window_id);
            let surface_id = current_window_surface_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No focused surface", None))?;
            surface_split_payload(
                model,
                &json_map_from_pairs([
                    ("surface_id", json!(surface_id)),
                    ("direction", json!("down")),
                ]),
            )
        }
        "palette.newTerminal" => {
            model.close_palette(window_id);
            let workspace_id = current_window_workspace_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No workspace selected", None))?;
            surface_create_payload(
                model,
                &json_map_from_pairs([("workspace_id", json!(workspace_id))]),
            )
        }
        "palette.closeTab" => {
            model.close_palette(window_id);
            let surface_id = current_window_surface_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No focused surface", None))?;
            surface_close_payload(
                model,
                &json_map_from_pairs([("surface_id", json!(surface_id))]),
            )
        }
        "palette.clearTerminalHistory" => {
            model.close_palette(window_id);
            let surface_id = current_window_surface_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No focused surface", None))?;
            surface_clear_history_payload(
                model,
                &json_map_from_pairs([("surface_id", json!(surface_id))]),
            )
        }
        "palette.focusLastPane" => {
            model.close_palette(window_id);
            let workspace_id = current_window_workspace_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No workspace selected", None))?;
            pane_last_payload(
                model,
                &json_map_from_pairs([("workspace_id", json!(workspace_id))]),
            )
        }
        command_id if command_id.starts_with("switcher.workspace.") => {
            let workspace_id = row
                .workspace_id
                .as_deref()
                .and_then(|raw| Uuid::parse_str(raw).ok())
                .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
            model
                .state
                .select_workspace(workspace_id)
                .map_err(MethodError::internal_error)?;
            model.close_palette(window_id);
            let target_window_id = model
                .state
                .workspace_window_id(workspace_id)
                .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
            Ok(json!({
                "window_id": target_window_id,
                "window_ref": model.window_ref(target_window_id),
                "workspace_id": workspace_id,
                "workspace_ref": model.workspace_ref(workspace_id),
            }))
        }
        command_id if command_id.starts_with("switcher.surface.") => {
            let surface_id = row
                .surface_id
                .as_deref()
                .and_then(|raw| Uuid::parse_str(raw).ok())
                .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
            let (workspace_id, pane_id) = model
                .state
                .focus_surface(surface_id)
                .map_err(MethodError::internal_error)?;
            model.close_palette(window_id);
            let target_window_id = model
                .state
                .workspace_window_id(workspace_id)
                .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
            Ok(json!({
                "window_id": target_window_id,
                "window_ref": model.window_ref(target_window_id),
                "workspace_id": workspace_id,
                "workspace_ref": model.workspace_ref(workspace_id),
                "pane_id": pane_id,
                "pane_ref": model.pane_ref(pane_id),
                "surface_id": surface_id,
                "surface_ref": model.surface_ref(surface_id),
            }))
        }
        _ => {
            model.close_palette(window_id);
            Ok(json!({
                "window_id": window_id,
                "window_ref": model.window_ref(window_id),
                "command_id": row.command_id,
            }))
        }
    }
}

fn execute_command_palette_selection(model: &mut AppModel, window_id: Uuid) -> MethodResult {
    let state = model.palette_state(window_id);
    if !state.visible {
        return Ok(json!({}));
    }

    if state.mode == CommandPaletteMode::RenameInput {
        let text = state.text.trim().to_string();
        let target = state.rename_target;
        match target {
            Some(RenameTarget::Surface(surface_id)) => {
                model
                    .state
                    .rename_surface(surface_id, text)
                    .map_err(MethodError::invalid_params)?;
            }
            Some(RenameTarget::Workspace(workspace_id)) => {
                model
                    .state
                    .rename_workspace(workspace_id, text)
                    .map_err(MethodError::invalid_params)?;
            }
            None => {}
        }
        model.close_palette(window_id);
        return debug_command_palette_visible_payload(
            model,
            &json_map_from_pairs([("window_id", json!(window_id))]),
        );
    }

    let Some(row) = palette_current_row(model, window_id) else {
        model.close_palette(window_id);
        return Ok(json!({}));
    };
    execute_command_palette_row(model, window_id, &row)
}

fn debug_shortcut_set_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let name = param_string(params, "name")
        .ok_or_else(|| MethodError::invalid_params("Missing or invalid name"))?;
    let combo = param_string(params, "combo")
        .ok_or_else(|| MethodError::invalid_params("Missing or invalid combo"))?;
    model.set_shortcut_override(name, combo);
    Ok(json!({
        "name": name,
        "combo": combo,
        "shortcut_hint": model.shortcut_hint(name),
    }))
}

fn debug_shortcut_simulate_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let combo = param_string(params, "combo")
        .ok_or_else(|| MethodError::invalid_params("Missing or invalid combo"))?
        .to_ascii_lowercase();
    let window_id = model.state.window_id;

    match combo.as_str() {
        "cmd+b" | "command+b" => {
            let visible = model.toggle_sidebar(window_id);
            Ok(json!({
                "window_id": window_id,
                "window_ref": model.window_ref(window_id),
                "visible": visible,
            }))
        }
        "cmd+t" | "command+t" => {
            let workspace_id = current_window_workspace_id(model, window_id)
                .ok_or_else(|| MethodError::not_found("No workspace selected", None))?;
            surface_create_payload(
                model,
                &json_map_from_pairs([("workspace_id", json!(workspace_id))]),
            )
        }
        "cmd+p" | "command+p" => {
            let palette = model.palette_state(window_id);
            if palette.visible && palette_effective_mode(&palette) == CommandPaletteMode::Switcher {
                model.close_palette(window_id);
            } else {
                model.open_palette_mode(window_id, CommandPaletteMode::Switcher);
            }
            debug_command_palette_visible_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "cmd+shift+p" | "command+shift+p" => {
            let palette = model.palette_state(window_id);
            if palette.visible && palette_effective_mode(&palette) == CommandPaletteMode::Commands {
                model.close_palette(window_id);
            } else {
                model.open_palette_mode(window_id, CommandPaletteMode::Commands);
            }
            debug_command_palette_visible_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "cmd+a" | "command+a" if model.palette_state(window_id).visible => {
            model.select_all_palette_text(window_id);
            debug_command_palette_rename_input_selection_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "down" | "ctrl+n" | "control+n" | "ctrl+j" | "control+j"
            if model.palette_state(window_id).visible =>
        {
            palette_move_selection(model, window_id, 1);
            debug_command_palette_selection_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "up" | "ctrl+p" | "control+p" | "ctrl+k" | "control+k"
            if model.palette_state(window_id).visible =>
        {
            palette_move_selection(model, window_id, -1);
            debug_command_palette_selection_payload(
                model,
                &json_map_from_pairs([("window_id", json!(window_id))]),
            )
        }
        "enter" | "return" if model.palette_state(window_id).visible => {
            execute_command_palette_selection(model, window_id)
        }
        other => Err(MethodError::invalid_params(format!(
            "unsupported shortcut {other}"
        ))),
    }
}

fn debug_sidebar_visible_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "visible": model.sidebar_visible(window_id),
    }))
}

fn debug_portal_stats_payload() -> MethodResult {
    Ok(json!({
        "totals": {
            "orphan_terminal_subview_count": 0,
            "visible_orphan_terminal_subview_count": 0,
            "stale_entry_count": 0,
        }
    }))
}

fn workspace_action_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    let action = param_string(params, "action")
        .ok_or_else(|| MethodError::invalid_params("Missing action"))?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;

    match action {
        "rename" => {
            let title = param_string(params, "title")
                .ok_or_else(|| MethodError::invalid_params("Missing title"))?
                .to_string();
            model
                .state
                .rename_workspace(workspace_id, title)
                .map_err(MethodError::invalid_params)?;
        }
        "close" => {
            model
                .state
                .close_workspace(workspace_id)
                .map_err(MethodError::invalid_params)?;
        }
        other => {
            return Err(MethodError::invalid_params(format!(
                "unsupported workspace action {other}"
            )))
        }
    }

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "action": action,
    }))
}

fn surface_action_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = surface_target_id(model, params)?;
    let action = param_string(params, "action")
        .ok_or_else(|| MethodError::invalid_params("Missing action"))?;
    let (workspace_id, pane_id) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;

    match action {
        "rename" => {
            let title = param_string(params, "title")
                .ok_or_else(|| MethodError::invalid_params("Missing title"))?
                .to_string();
            model
                .state
                .rename_surface(surface_id, title)
                .map_err(MethodError::invalid_params)?;
        }
        "clear_name" => {
            let _ = model
                .state
                .clear_surface_title(surface_id)
                .map_err(MethodError::invalid_params)?;
        }
        "mark_unread" => {
            let _ = model
                .state
                .mark_surface_unread(surface_id)
                .map_err(MethodError::invalid_params)?;
        }
        "mark_read" => {
            let _ = model
                .state
                .mark_surface_read(surface_id)
                .map_err(MethodError::invalid_params)?;
        }
        other => {
            return Err(MethodError::invalid_params(format!(
                "unsupported surface action {other}"
            )))
        }
    }

    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspace(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    let surface = workspace
        .surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;

    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "tab_id": surface_id,
        "tab_ref": model.tab_ref(surface_id),
        "title": surface.title,
        "unread": surface.unread,
        "flash_count": surface.flash_count,
        "action": action,
    }))
}

fn tab_action_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let mut translated = params.clone();
    if !translated.contains_key("surface_id") {
        if let Some(tab_id) = translated.get("tab_id").cloned() {
            translated.insert("surface_id".to_string(), tab_id);
        }
    }
    let action = param_string(&translated, "action")
        .ok_or_else(|| MethodError::invalid_params("Missing action"))?;
    if matches!(action, "pin" | "unpin") {
        let surface_id = surface_target_id(model, &translated)?;
        let (workspace_id, pane_id) = model
            .state
            .locate_surface(surface_id)
            .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
        let window_id = model
            .state
            .workspace_window_id(workspace_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
        return Ok(json!({
            "window_id": window_id,
            "window_ref": model.window_ref(window_id),
            "workspace_id": workspace_id,
            "workspace_ref": model.workspace_ref(workspace_id),
            "pane_id": pane_id,
            "pane_ref": model.pane_ref(pane_id),
            "surface_id": surface_id,
            "surface_ref": model.surface_ref(surface_id),
            "tab_id": surface_id,
            "tab_ref": model.tab_ref(surface_id),
            "pinned": action == "pin",
            "action": action,
        }));
    }

    let mut payload = surface_action_payload(model, &translated)?;
    if let Some(result) = payload.as_object_mut() {
        result.insert("action".to_string(), Value::String(action.to_string()));
        result.insert("pinned".to_string(), Value::Bool(false));
    }

    Ok(payload)
}

fn debug_command_palette_rename_select_all_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    if let Some(enabled) = params.get("enabled").and_then(Value::as_bool) {
        model.rename_select_all = enabled;
    }
    Ok(json!({
        "enabled": model.rename_select_all,
    }))
}

fn debug_command_palette_visible_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let state = model.palette_state(window_id);
    let mode = palette_effective_mode(&state);
    Ok(json!({
        "id": window_id,
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "visible": state.visible,
        "mode": mode.as_str(),
    }))
}

fn debug_command_palette_toggle_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let visible = model.palette_state(window_id).visible;
    if visible {
        model.close_palette(window_id);
    } else {
        model.open_palette_mode(window_id, CommandPaletteMode::Switcher);
    }
    debug_command_palette_visible_payload(
        model,
        &json_map_from_pairs([("window_id", json!(window_id))]),
    )
}

fn debug_command_palette_results_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let limit = param_usize(params, "limit")?.unwrap_or(20);
    let palette = model.palette_state(window_id);
    let mode = palette_effective_mode(&palette);
    let rows = command_palette_rows(model, window_id, &palette);
    let selected_index = palette.selected_index.min(rows.len().saturating_sub(1));
    let results = rows
        .into_iter()
        .take(limit)
        .enumerate()
        .map(|(index, row)| {
            let mut object = Map::new();
            object.insert("command_id".to_string(), Value::String(row.command_id));
            object.insert("title".to_string(), Value::String(row.title));
            object.insert(
                "trailing_label".to_string(),
                row.trailing_label.map(Value::String).unwrap_or(Value::Null),
            );
            object.insert(
                "shortcut_hint".to_string(),
                row.shortcut_hint.map(Value::String).unwrap_or(Value::Null),
            );
            object.insert(
                "surface_id".to_string(),
                row.surface_id.map(Value::String).unwrap_or(Value::Null),
            );
            object.insert(
                "workspace_id".to_string(),
                row.workspace_id.map(Value::String).unwrap_or(Value::Null),
            );
            object.insert("selected".to_string(), Value::Bool(index == selected_index));
            Value::Object(object)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "id": window_id,
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "visible": palette.visible,
        "mode": mode.as_str(),
        "query": palette.text,
        "results": results,
    }))
}

fn debug_command_palette_selection_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let state = model.palette_state(window_id);
    let mode = palette_effective_mode(&state);
    let rows = command_palette_rows(model, window_id, &state);
    let selected_index = state.selected_index.min(rows.len().saturating_sub(1));
    Ok(json!({
        "id": window_id,
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "selected_index": selected_index,
        "visible": state.visible,
        "mode": mode.as_str(),
    }))
}

fn debug_command_palette_rename_input_selection_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let state = model.palette_state(window_id);
    let focused = state.visible;
    Ok(json!({
        "id": window_id,
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "focused": focused,
        "text_length": state.text.len(),
        "selection_location": state.selection_location,
        "selection_length": state.selection_length,
    }))
}

fn debug_command_palette_rename_input_interact_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let state = model.palette_state(window_id);
    if state.mode == CommandPaletteMode::RenameInput {
        if model.rename_select_all && !state.text.is_empty() {
            model.select_all_palette_text(window_id);
        }
    }
    debug_command_palette_rename_input_selection_payload(
        model,
        &json_map_from_pairs([("window_id", json!(window_id))]),
    )
}

fn debug_command_palette_rename_input_delete_backward_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let window_id = window_target_id(model, params)?;
    let state = model.palette_state(window_id);
    if state.mode == CommandPaletteMode::RenameInput {
        if state.text.is_empty() {
            model.open_palette_mode(window_id, CommandPaletteMode::Commands);
        } else {
            model.delete_backward_palette_text(window_id);
        }
    }
    debug_command_palette_visible_payload(
        model,
        &json_map_from_pairs([("window_id", json!(window_id))]),
    )
}

fn debug_notification_focus_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let workspace_id = require_workspace_id(model, params, "workspace_id")?;
    if let Some(surface_id) = optional_surface_id(model, params, "surface_id")? {
        let (focused_workspace_id, pane_id) = model
            .state
            .focus_surface(surface_id)
            .map_err(MethodError::internal_error)?;
        let window_id = model
            .state
            .workspace_window_id(focused_workspace_id)
            .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
        return Ok(json!({
            "window_id": window_id,
            "window_ref": model.window_ref(window_id),
            "workspace_id": focused_workspace_id,
            "workspace_ref": model.workspace_ref(focused_workspace_id),
            "pane_id": pane_id,
            "pane_ref": model.pane_ref(pane_id),
            "surface_id": surface_id,
            "surface_ref": model.surface_ref(surface_id),
        }));
    }
    model
        .state
        .select_workspace(workspace_id)
        .map_err(MethodError::internal_error)?;
    let window_id = model
        .state
        .workspace_window_id(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
    }))
}

fn debug_terminal_is_focused_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let focused = model.state.current_surface_id() == Some(surface_id)
        && !model.palette_state(model.state.window_id).visible;
    Ok(json!({
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "focused": focused,
    }))
}

fn debug_terminal_read_text_payload(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> MethodResult {
    surface_read_text_payload(model, params)
}

fn debug_type_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let window_id = model.state.window_id;
    let palette = model.palette_state(window_id);
    if palette.visible {
        let text = raw_string_param(params, "text")
            .ok_or_else(|| MethodError::invalid_params("Missing text"))?;
        model.replace_palette_text(window_id, text);
        return debug_command_palette_results_payload(
            model,
            &json_map_from_pairs([("window_id", json!(window_id))]),
        );
    }
    surface_send_text_payload(model, params)
}

fn debug_layout_payload(model: &mut AppModel) -> MethodResult {
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .selected_workspace()
        .ok_or_else(|| MethodError::not_found("No workspace selected", None))?;
    let panes = workspace
        .debug_pane_regions()
        .into_iter()
        .map(|(pane_id, x, y, width, height)| {
            json!({
                "paneId": pane_id,
                "pane_id": pane_id,
                "frame": {
                    "x": x,
                    "y": y,
                    "width": width,
                    "height": height,
                }
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "layout": {
            "layout": {
                "workspace_id": workspace.id,
                "window_id": workspace.window_id,
                "panes": panes,
            }
        }
    }))
}

fn debug_panel_snapshot_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let (workspace_id, pane_id) = model
        .state
        .locate_surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspace(workspace_id)
        .ok_or_else(|| MethodError::not_found("Workspace not found", None))?;
    let surface = workspace
        .surface(surface_id)
        .ok_or_else(|| MethodError::not_found("Surface not found", None))?;
    let window_id = workspace.window_id;
    Ok(json!({
        "window_id": window_id,
        "window_ref": model.window_ref(window_id),
        "workspace_id": workspace_id,
        "workspace_ref": model.workspace_ref(workspace_id),
        "pane_id": pane_id,
        "pane_ref": model.pane_ref(pane_id),
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "panel_id": surface_id,
        "title": surface.title,
        "transcript_bytes": surface.transcript.len(),
        "unread": surface.unread,
        "flash_count": surface.flash_count,
    }))
}

fn debug_window_screenshot_payload(params: &Map<String, Value>) -> MethodResult {
    Ok(json!({
        "path": Value::Null,
        "label": params.get("label").cloned().unwrap_or(Value::Null),
    }))
}

fn debug_flash_count_payload(model: &mut AppModel, params: &Map<String, Value>) -> MethodResult {
    let surface_id = require_surface_id(model, params, "surface_id")?;
    let count = model
        .state
        .flash_count(surface_id)
        .map_err(MethodError::internal_error)?;
    Ok(json!({
        "surface_id": surface_id,
        "surface_ref": model.surface_ref(surface_id),
        "count": count,
    }))
}

fn debug_flash_reset_payload(model: &mut AppModel) -> MethodResult {
    model.state.reset_flash_counts();
    Ok(json!({}))
}

fn should_suppress_notification(
    model: &mut AppModel,
    workspace_id: uuid::Uuid,
    surface_id: Option<uuid::Uuid>,
) -> bool {
    if !model.app_focus_active() {
        return false;
    }
    let snapshot = model.snapshot_state();
    if snapshot.selected_workspace_id != workspace_id {
        return false;
    }
    match surface_id {
        Some(surface_id) => snapshot.current_surface_id() == Some(surface_id),
        None => true,
    }
}

fn window_target_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> Result<uuid::Uuid, MethodError> {
    if let Some(raw_window_id) = param_string(params, "window_id") {
        let window_id = model
            .resolve_window_id(raw_window_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid window_id"))?;
        if model.state.window(window_id).is_none() {
            return Err(MethodError::not_found(
                "Window not found",
                Some(json!({ "window_id": window_id })),
            ));
        }
        return Ok(window_id);
    }
    if model.state.window_id.is_nil() || model.state.window(model.state.window_id).is_none() {
        return Err(MethodError::not_found("No window selected", None));
    }
    Ok(model.state.window_id)
}

fn workspace_target_index(
    model: &mut AppModel,
    workspace_id: uuid::Uuid,
    window_id: uuid::Uuid,
    params: &Map<String, Value>,
) -> Result<usize, MethodError> {
    let snapshot = model.snapshot_state();
    let mut workspace_ids = snapshot
        .workspaces
        .iter()
        .filter(|workspace| workspace.window_id == window_id && workspace.id != workspace_id)
        .map(|workspace| workspace.id)
        .collect::<Vec<_>>();

    let mut targets = 0;
    let mut resolved_index = workspace_ids.len();
    if let Some(index) = param_usize(params, "index")? {
        resolved_index = index.min(workspace_ids.len());
        targets += 1;
    }
    if let Some(before_workspace_id) = optional_workspace_id(model, params, "before_workspace_id")?
    {
        resolved_index = workspace_ids
            .iter()
            .position(|candidate| *candidate == before_workspace_id)
            .ok_or_else(|| MethodError::not_found("before_workspace_id not found", None))?;
        targets += 1;
    }
    if let Some(after_workspace_id) = optional_workspace_id(model, params, "after_workspace_id")? {
        resolved_index = workspace_ids
            .iter()
            .position(|candidate| *candidate == after_workspace_id)
            .map(|index| index + 1)
            .ok_or_else(|| MethodError::not_found("after_workspace_id not found", None))?;
        targets += 1;
    }
    if targets != 1 {
        return Err(MethodError::invalid_params(
            "workspace.reorder requires exactly one target: index|before_workspace_id|after_workspace_id",
        ));
    }
    workspace_ids.truncate(workspace_ids.len());
    Ok(resolved_index)
}

fn surface_target_index(
    model: &mut AppModel,
    surface_id: uuid::Uuid,
    pane_id: uuid::Uuid,
    params: &Map<String, Value>,
) -> Result<usize, MethodError> {
    let snapshot = model.snapshot_state();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.pane(pane_id).is_some())
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
    let pane = workspace
        .pane(pane_id)
        .ok_or_else(|| MethodError::not_found("Pane not found", None))?;
    let mut surface_ids = pane
        .surfaces
        .iter()
        .map(|surface| surface.id)
        .filter(|candidate| *candidate != surface_id)
        .collect::<Vec<_>>();

    let mut targets = 0;
    let mut resolved_index = surface_ids.len();
    if let Some(index) = param_usize(params, "index")? {
        resolved_index = index.min(surface_ids.len());
        targets += 1;
    }
    if let Some(before_surface_id) = optional_surface_id(model, params, "before_surface_id")? {
        resolved_index = surface_ids
            .iter()
            .position(|candidate| *candidate == before_surface_id)
            .ok_or_else(|| MethodError::not_found("before_surface_id not found", None))?;
        targets += 1;
    }
    if let Some(after_surface_id) = optional_surface_id(model, params, "after_surface_id")? {
        resolved_index = surface_ids
            .iter()
            .position(|candidate| *candidate == after_surface_id)
            .map(|index| index + 1)
            .ok_or_else(|| MethodError::not_found("after_surface_id not found", None))?;
        targets += 1;
    }
    if targets == 0 {
        return Ok(surface_ids.len());
    }
    if targets != 1 {
        return Err(MethodError::invalid_params(
            "surface.move/reorder requires exactly one target: index|before_surface_id|after_surface_id",
        ));
    }
    surface_ids.truncate(surface_ids.len());
    Ok(resolved_index)
}

fn workspace_target_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> Result<uuid::Uuid, MethodError> {
    if let Some(raw_workspace_id) = param_string(params, "workspace_id") {
        let workspace_id = model
            .resolve_workspace_id(raw_workspace_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid workspace_id"))?;
        if model.state.workspace(workspace_id).is_none() {
            return Err(MethodError::not_found(
                "Workspace not found",
                Some(json!({ "workspace_id": workspace_id })),
            ));
        }
        return Ok(workspace_id);
    }

    let window_id = window_target_id(model, params)?;
    let selected_workspace_id = model
        .state
        .window(window_id)
        .map(|window| window.selected_workspace_id)
        .unwrap_or(uuid::Uuid::nil());
    if selected_workspace_id.is_nil() {
        return Err(MethodError::not_found("No workspace selected", None));
    }
    Ok(selected_workspace_id)
}

fn surface_target_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
) -> Result<uuid::Uuid, MethodError> {
    if let Some(raw_surface_id) = param_string(params, "surface_id") {
        return model
            .resolve_surface_id(raw_surface_id)
            .ok_or_else(|| MethodError::invalid_params("Missing or invalid surface_id"));
    }

    let workspace_id = workspace_target_id(model, params)?;
    let snapshot = model.snapshot_state();
    snapshot
        .workspace(workspace_id)
        .and_then(Workspace::selected_surface_id)
        .ok_or_else(|| MethodError::not_found("No focused surface", None))
}

fn require_workspace_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
    key: &str,
) -> Result<uuid::Uuid, MethodError> {
    let raw_workspace_id = param_string(params, key)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))?;
    model
        .resolve_workspace_id(raw_workspace_id)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))
}

fn optional_workspace_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
    key: &str,
) -> Result<Option<uuid::Uuid>, MethodError> {
    let Some(raw_workspace_id) = param_string(params, key) else {
        return Ok(None);
    };
    let workspace_id = model
        .resolve_workspace_id(raw_workspace_id)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))?;
    Ok(Some(workspace_id))
}

fn require_window_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
    key: &str,
) -> Result<uuid::Uuid, MethodError> {
    let raw_window_id = param_string(params, key)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))?;
    model
        .resolve_window_id(raw_window_id)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))
}

fn require_pane_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
    key: &str,
) -> Result<uuid::Uuid, MethodError> {
    let raw_pane_id = param_string(params, key)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))?;
    model
        .resolve_pane_id(raw_pane_id)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))
}

fn require_surface_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
    key: &str,
) -> Result<uuid::Uuid, MethodError> {
    let raw_surface_id = param_string(params, key)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))?;
    model
        .resolve_surface_id(raw_surface_id)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))
}

fn optional_surface_id(
    model: &mut AppModel,
    params: &Map<String, Value>,
    key: &str,
) -> Result<Option<uuid::Uuid>, MethodError> {
    let Some(raw_surface_id) = param_string(params, key) else {
        return Ok(None);
    };
    model
        .resolve_surface_id(raw_surface_id)
        .map(Some)
        .ok_or_else(|| MethodError::invalid_params(format!("Missing or invalid {key}")))
}

fn parse_direction(params: &Map<String, Value>) -> Result<(SplitOrientation, bool), MethodError> {
    let direction = param_string(params, "direction").ok_or_else(|| {
        MethodError::invalid_params("Missing or invalid direction (left|right|up|down)")
    })?;

    match direction.to_ascii_lowercase().as_str() {
        "left" => Ok((SplitOrientation::Horizontal, true)),
        "right" => Ok((SplitOrientation::Horizontal, false)),
        "up" => Ok((SplitOrientation::Vertical, true)),
        "down" => Ok((SplitOrientation::Vertical, false)),
        _ => Err(MethodError::invalid_params(
            "Missing or invalid direction (left|right|up|down)",
        )),
    }
}

fn ensure_terminal_type_supported(params: &Map<String, Value>) -> Result<(), MethodError> {
    if let Some(surface_type) = param_string(params, "type") {
        if surface_type.eq_ignore_ascii_case("terminal") {
            return Ok(());
        }

        return Err(MethodError::new(
            "not_supported",
            format!("{surface_type} surfaces are not supported on gtk4-libadwaita"),
            Some(json!({
                "platform": "linux",
                "frontend": "gtk4-libadwaita",
                "type": surface_type,
            })),
        ));
    }
    Ok(())
}

fn deliver_notification(title: &str, subtitle: &str, body: &str) -> bool {
    let mut summary = title.to_string();
    if !subtitle.trim().is_empty() {
        summary.push_str(" - ");
        summary.push_str(subtitle);
    }
    let status = Command::new("notify-send").arg(summary).arg(body).status();
    matches!(status, Ok(status) if status.success())
}

fn param_string<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    params
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn raw_string_param<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    params.get(key)?.as_str()
}

fn optional_string_param(
    params: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, MethodError> {
    match params.get(key) {
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(_) => Err(MethodError::invalid_params(format!(
            "{key} must be a string"
        ))),
        None => Ok(None),
    }
}

fn translate_terminal_key(key: &str) -> Result<String, MethodError> {
    let normalized = key.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(MethodError::invalid_params("key must not be empty"));
    }

    if let Some(control) = translate_ctrl_key(&normalized) {
        return Ok(control);
    }

    if let Some(meta) = translate_meta_key(&normalized) {
        return Ok(meta);
    }

    let translated = match normalized.as_str() {
        "enter" | "return" => "\r",
        "tab" => "\t",
        "backtab" | "shift-tab" => "\u{1b}[Z",
        "space" => " ",
        "escape" | "esc" => "\u{1b}",
        "backspace" => "\u{7f}",
        "up" | "arrowup" => "\u{1b}[A",
        "down" | "arrowdown" => "\u{1b}[B",
        "right" | "arrowright" => "\u{1b}[C",
        "left" | "arrowleft" => "\u{1b}[D",
        "home" => "\u{1b}[H",
        "end" => "\u{1b}[F",
        "insert" => "\u{1b}[2~",
        "delete" | "del" => "\u{1b}[3~",
        "pageup" | "page_up" => "\u{1b}[5~",
        "pagedown" | "page_down" => "\u{1b}[6~",
        "f1" => "\u{1b}OP",
        "f2" => "\u{1b}OQ",
        "f3" => "\u{1b}OR",
        "f4" => "\u{1b}OS",
        "f5" => "\u{1b}[15~",
        "f6" => "\u{1b}[17~",
        "f7" => "\u{1b}[18~",
        "f8" => "\u{1b}[19~",
        "f9" => "\u{1b}[20~",
        "f10" => "\u{1b}[21~",
        "f11" => "\u{1b}[23~",
        "f12" => "\u{1b}[24~",
        other => {
            return Err(MethodError::invalid_params(format!("unknown key {other}")));
        }
    };

    Ok(translated.to_string())
}

fn translate_ctrl_key(key: &str) -> Option<String> {
    let suffix = key
        .strip_prefix("ctrl-")
        .or_else(|| key.strip_prefix("control-"))?;

    let translated = match suffix {
        "space" | "@" | "2" => 0x00,
        "a" => 0x01,
        "b" => 0x02,
        "c" => 0x03,
        "d" => 0x04,
        "e" => 0x05,
        "f" => 0x06,
        "g" => 0x07,
        "h" => 0x08,
        "i" => 0x09,
        "j" => 0x0a,
        "k" => 0x0b,
        "l" => 0x0c,
        "m" => 0x0d,
        "n" => 0x0e,
        "o" => 0x0f,
        "p" => 0x10,
        "q" => 0x11,
        "r" => 0x12,
        "s" => 0x13,
        "t" => 0x14,
        "u" => 0x15,
        "v" => 0x16,
        "w" => 0x17,
        "x" => 0x18,
        "y" => 0x19,
        "z" => 0x1a,
        "[" | "3" => 0x1b,
        "\\" | "4" => 0x1c,
        "]" | "5" => 0x1d,
        "^" | "6" => 0x1e,
        "_" | "/" | "7" => 0x1f,
        "8" | "?" => 0x7f,
        _ => return None,
    };

    Some(char::from(translated).to_string())
}

fn translate_meta_key(key: &str) -> Option<String> {
    let suffix = key
        .strip_prefix("alt-")
        .or_else(|| key.strip_prefix("meta-"))?;
    if suffix.chars().count() != 1 {
        return None;
    }
    Some(format!("\u{1b}{suffix}"))
}

fn param_usize(params: &Map<String, Value>, key: &str) -> Result<Option<usize>, MethodError> {
    let Some(raw_value) = params.get(key) else {
        return Ok(None);
    };
    if let Some(value) = raw_value.as_u64() {
        return Ok(Some(value as usize));
    }
    if let Some(raw_value) = raw_value.as_str() {
        return raw_value
            .trim()
            .parse::<usize>()
            .map(Some)
            .map_err(|_| MethodError::invalid_params(format!("{key} must be an integer")));
    }
    Err(MethodError::invalid_params(format!(
        "{key} must be an integer"
    )))
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "id": id,
        "ok": true,
        "result": result,
    })
}

fn error_response(id: Value, code: &str, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message,
    });
    if let Some(data) = data {
        if let Some(error_object) = error.as_object_mut() {
            error_object.insert("data".to_string(), data);
        }
    }
    json!({
        "id": id,
        "ok": false,
        "error": error,
    })
}

fn surface_type_name(surface: &Surface) -> &'static str {
    match surface.kind {
        SurfaceKind::Terminal => "terminal",
    }
}

struct SurfaceRow<'a> {
    pane: &'a Pane,
    surface: &'a Surface,
    index_in_pane: usize,
}

fn workspace_surface_rows(workspace: &Workspace) -> Vec<SurfaceRow<'_>> {
    let mut rows = Vec::new();
    for pane in workspace.ordered_panes() {
        for (index_in_pane, surface) in pane.surfaces.iter().enumerate() {
            rows.push(SurfaceRow {
                pane,
                surface,
                index_in_pane,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Request {
        Request {
            id: json!(1),
            method: method.to_string(),
            params: params.as_object().cloned().unwrap_or_default(),
        }
    }

    fn result_body(response: Value) -> Value {
        assert!(response.get("ok").and_then(Value::as_bool).unwrap_or(false));
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    fn error_code(response: Value) -> String {
        assert!(!response.get("ok").and_then(Value::as_bool).unwrap_or(false));
        response
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn capabilities_advertise_linux_multi_window_profile() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let response = dispatch_request(&mut model, request("system.capabilities", json!({})));
        let result = result_body(response);

        assert_eq!(result["platform"]["id"], "linux");
        assert_eq!(result["platform"]["window_multi"], true);
        assert!(result["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .any(|method| method == "workspace.create"));
    }

    #[test]
    fn workspace_create_does_not_steal_focus() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let current = result_body(dispatch_request(
            &mut model,
            request("workspace.current", json!({})),
        ));

        let created = result_body(dispatch_request(
            &mut model,
            request("workspace.create", json!({})),
        ));
        let current_after = result_body(dispatch_request(
            &mut model,
            request("workspace.current", json!({})),
        ));

        assert_ne!(created["workspace_id"], current["workspace_id"]);
        assert_eq!(current_after["workspace_id"], current["workspace_id"]);
    }

    #[test]
    fn surface_send_text_round_trip_reads_back_transcript() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let current = result_body(dispatch_request(
            &mut model,
            request("surface.current", json!({})),
        ));
        let surface_id = current["surface_id"].clone();

        let _ = dispatch_request(
            &mut model,
            request(
                "surface.send_text",
                json!({ "surface_id": surface_id, "text": "echo hi\n" }),
            ),
        );
        let read = result_body(dispatch_request(
            &mut model,
            request("surface.read_text", json!({ "surface_id": surface_id })),
        ));

        assert_eq!(read["text"], "echo hi\n");
    }

    #[test]
    fn unsupported_browser_method_returns_not_supported() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let response = dispatch_request(&mut model, request("browser.navigate", json!({})));
        assert_eq!(error_code(response), "not_supported");
    }

    #[test]
    fn surface_split_keeps_current_focus_context() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let current_before = result_body(dispatch_request(
            &mut model,
            request("surface.current", json!({})),
        ));

        let created = result_body(dispatch_request(
            &mut model,
            request("surface.split", json!({ "direction": "right" })),
        ));
        let current_after = result_body(dispatch_request(
            &mut model,
            request("surface.current", json!({})),
        ));

        assert_ne!(created["surface_id"], current_before["surface_id"]);
        assert_eq!(current_after["surface_id"], current_before["surface_id"]);
    }

    #[test]
    fn workspace_rename_updates_workspace_title() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let created = result_body(dispatch_request(
            &mut model,
            request("workspace.create", json!({})),
        ));
        let workspace_id = created["workspace_id"].as_str().expect("workspace_id");

        let _ = dispatch_request(
            &mut model,
            request(
                "workspace.rename",
                json!({ "workspace_id": workspace_id, "title": "Renamed Workspace" }),
            ),
        );
        let listed = result_body(dispatch_request(
            &mut model,
            request("workspace.list", json!({})),
        ));

        let row = listed["workspaces"]
            .as_array()
            .expect("workspaces")
            .iter()
            .find(|row| row["id"] == workspace_id)
            .expect("workspace row");
        assert_eq!(row["title"], "Renamed Workspace");
    }

    #[test]
    fn workspace_reorder_moves_workspace_before_anchor() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let current = result_body(dispatch_request(
            &mut model,
            request("workspace.current", json!({})),
        ));
        let first_id = current["workspace_id"].as_str().expect("workspace_id");
        let created = result_body(dispatch_request(
            &mut model,
            request("workspace.create", json!({})),
        ));
        let second_id = created["workspace_id"].as_str().expect("workspace_id");

        let _ = dispatch_request(
            &mut model,
            request(
                "workspace.reorder",
                json!({
                    "workspace_id": second_id,
                    "before_workspace_id": first_id,
                }),
            ),
        );
        let listed = result_body(dispatch_request(
            &mut model,
            request("workspace.list", json!({})),
        ));
        let ids = listed["workspaces"]
            .as_array()
            .expect("workspaces")
            .iter()
            .map(|row| row["id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();

        assert_eq!(ids.first().map(String::as_str), Some(second_id));
    }

    #[test]
    fn surface_move_and_reorder_keep_stable_surface_ids() {
        let mut model = AppModel::new(DEFAULT_SOCKET_PATH.to_string());
        let split = result_body(dispatch_request(
            &mut model,
            request("surface.split", json!({ "direction": "right" })),
        ));
        let split_surface_id = split["surface_id"].as_str().expect("surface_id");
        let created = result_body(dispatch_request(
            &mut model,
            request("surface.create", json!({})),
        ));
        let created_surface_id = created["surface_id"].as_str().expect("surface_id");

        let panes = result_body(dispatch_request(
            &mut model,
            request("pane.list", json!({})),
        ));
        let pane_rows = panes["panes"].as_array().expect("panes");
        assert!(pane_rows.len() >= 2);
        let destination_pane_id = pane_rows[0]["id"].as_str().expect("pane_id");

        let _ = dispatch_request(
            &mut model,
            request(
                "surface.move",
                json!({
                    "surface_id": created_surface_id,
                    "pane_id": destination_pane_id,
                    "focus": false,
                }),
            ),
        );
        let moved = result_body(dispatch_request(
            &mut model,
            request("pane.surfaces", json!({ "pane_id": destination_pane_id })),
        ));
        let ids_after_move = moved["surfaces"]
            .as_array()
            .expect("surfaces")
            .iter()
            .map(|row| row["id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert!(ids_after_move.iter().any(|id| id == created_surface_id));

        let _ = dispatch_request(
            &mut model,
            request(
                "surface.reorder",
                json!({
                    "surface_id": created_surface_id,
                    "before_surface_id": split_surface_id,
                }),
            ),
        );
        let reordered = result_body(dispatch_request(
            &mut model,
            request("pane.surfaces", json!({ "pane_id": destination_pane_id })),
        ));
        let ids_after_reorder = reordered["surfaces"]
            .as_array()
            .expect("surfaces")
            .iter()
            .map(|row| row["id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            ids_after_reorder.first().map(String::as_str),
            Some(created_surface_id)
        );
        assert!(ids_after_reorder.iter().any(|id| id == split_surface_id));
    }

    #[test]
    fn translate_terminal_key_supports_common_terminal_sequences() {
        assert_eq!(translate_terminal_key("ctrl-c").unwrap(), "\u{3}");
        assert_eq!(translate_terminal_key("ctrl-d").unwrap(), "\u{4}");
        assert_eq!(translate_terminal_key("up").unwrap(), "\u{1b}[A");
        assert_eq!(translate_terminal_key("page_down").unwrap(), "\u{1b}[6~");
        assert_eq!(translate_terminal_key("f5").unwrap(), "\u{1b}[15~");
        assert_eq!(translate_terminal_key("alt-x").unwrap(), "\u{1b}x");
        assert_eq!(translate_terminal_key("shift-tab").unwrap(), "\u{1b}[Z");
    }

    #[test]
    fn translate_terminal_key_rejects_unknown_keys() {
        assert!(translate_terminal_key("ctrl-enter").is_err());
        assert!(translate_terminal_key("alt-left").is_err());
        assert!(translate_terminal_key("hyper-k").is_err());
    }
}
