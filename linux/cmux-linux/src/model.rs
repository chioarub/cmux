use crate::state::{AppState, PaneId, SurfaceId, WindowId, WorkspaceId};
use crate::terminal_host::TerminalBridge;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use parking_lot::Mutex;
use uuid::Uuid;

pub type SharedModel = Arc<Mutex<AppModel>>;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AppFocusOverride {
    Active,
    Inactive,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CommandPaletteMode {
    Commands,
    Switcher,
    RenameInput,
}

impl CommandPaletteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::Switcher => "switcher",
            Self::RenameInput => "rename_input",
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RenameTarget {
    Workspace(WorkspaceId),
    Surface(SurfaceId),
}

#[derive(Debug, Clone)]
pub struct CommandPaletteState {
    pub visible: bool,
    pub mode: CommandPaletteMode,
    pub query: String,
    pub selected_index: usize,
    pub text: String,
    pub selection_location: usize,
    pub selection_length: usize,
    pub rename_target: Option<RenameTarget>,
}

impl Default for CommandPaletteState {
    fn default() -> Self {
        Self {
            visible: false,
            mode: CommandPaletteMode::Commands,
            query: ">".to_string(),
            selected_index: 0,
            text: ">".to_string(),
            selection_location: 1,
            selection_length: 0,
            rename_target: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum HandleKind {
    Window,
    Workspace,
    Pane,
    Surface,
}

impl HandleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HandleKind::Window => "window",
            HandleKind::Workspace => "workspace",
            HandleKind::Pane => "pane",
            HandleKind::Surface => "surface",
        }
    }
}

#[derive(Debug, Default)]
struct HandleRegistry {
    next_ordinal: BTreeMap<HandleKind, usize>,
    ref_by_id: HashMap<(HandleKind, Uuid), String>,
    id_by_ref: HashMap<String, (HandleKind, Uuid)>,
}

impl HandleRegistry {
    fn ensure_ref(&mut self, kind: HandleKind, id: Uuid) -> String {
        if let Some(existing) = self.ref_by_id.get(&(kind, id)) {
            return existing.clone();
        }

        let next = self.next_ordinal.entry(kind).or_insert(1);
        let reference = format!("{}:{}", kind.as_str(), *next);
        *next += 1;
        self.ref_by_id.insert((kind, id), reference.clone());
        self.id_by_ref.insert(reference.clone(), (kind, id));
        reference
    }

    fn resolve_ref(&self, raw: &str) -> Option<(HandleKind, Uuid)> {
        self.id_by_ref.get(raw).copied().or_else(|| {
            let trimmed = raw.trim().to_ascii_lowercase();
            if !trimmed.starts_with("tab:") {
                return None;
            }
            let alias = trimmed.replacen("tab:", "surface:", 1);
            self.id_by_ref.get(&alias).copied()
        })
    }
}

#[derive(Debug)]
pub struct AppModel {
    pub state: AppState,
    pub socket_path: String,
    pub terminal_bridge: TerminalBridge,
    pub app_focus_override: Option<AppFocusOverride>,
    pub command_palette_by_window: HashMap<WindowId, CommandPaletteState>,
    pub sidebar_visible_by_window: HashMap<WindowId, bool>,
    pub rename_select_all: bool,
    pub shortcut_overrides: HashMap<String, String>,
    handles: HandleRegistry,
}

impl AppModel {
    #[allow(dead_code)]
    pub fn new(socket_path: String) -> Self {
        let (terminal_bridge, _receiver) = TerminalBridge::new();
        Self::with_bridge(socket_path, terminal_bridge)
    }

    #[allow(dead_code)]
    pub fn with_bridge(socket_path: String, terminal_bridge: TerminalBridge) -> Self {
        Self::from_state(socket_path, terminal_bridge, AppState::new())
    }

    pub fn from_state(
        socket_path: String,
        terminal_bridge: TerminalBridge,
        state: AppState,
    ) -> Self {
        let mut model = Self {
            state,
            socket_path,
            terminal_bridge,
            app_focus_override: None,
            command_palette_by_window: HashMap::new(),
            sidebar_visible_by_window: HashMap::new(),
            rename_select_all: false,
            shortcut_overrides: HashMap::new(),
            handles: HandleRegistry::default(),
        };
        model.refresh_handles();
        model
    }

    #[allow(dead_code)]
    pub fn shared(socket_path: String) -> SharedModel {
        Arc::new(Mutex::new(Self::new(socket_path)))
    }

    #[allow(dead_code)]
    pub fn shared_with_bridge(socket_path: String, terminal_bridge: TerminalBridge) -> SharedModel {
        Arc::new(Mutex::new(Self::with_bridge(socket_path, terminal_bridge)))
    }

    pub fn shared_with_state(
        socket_path: String,
        terminal_bridge: TerminalBridge,
        state: AppState,
    ) -> SharedModel {
        Arc::new(Mutex::new(Self::from_state(
            socket_path,
            terminal_bridge,
            state,
        )))
    }

    pub fn revision(&self) -> u64 {
        self.state.revision()
    }

    pub fn snapshot_state(&self) -> AppState {
        self.state.clone()
    }

    pub fn refresh_handles(&mut self) {
        for window in &self.state.windows {
            self.handles.ensure_ref(HandleKind::Window, window.id);
        }
        for workspace in &self.state.workspaces {
            self.handles.ensure_ref(HandleKind::Workspace, workspace.id);
            for pane in &workspace.panes {
                self.handles.ensure_ref(HandleKind::Pane, pane.id);
                for surface in &pane.surfaces {
                    self.handles.ensure_ref(HandleKind::Surface, surface.id);
                }
            }
        }
    }

    fn resolve_id(&mut self, kind: HandleKind, raw: &str) -> Option<Uuid> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Ok(id) = Uuid::parse_str(trimmed) {
            return self.contains_id(kind, id).then_some(id);
        }

        self.refresh_handles();
        let (resolved_kind, resolved_id) = self.handles.resolve_ref(trimmed)?;
        if resolved_kind == kind {
            return Some(resolved_id);
        }
        None
    }

    fn contains_id(&self, kind: HandleKind, id: Uuid) -> bool {
        match kind {
            HandleKind::Window => self.state.window(id).is_some(),
            HandleKind::Workspace => self.state.workspace(id).is_some(),
            HandleKind::Pane => self
                .state
                .workspaces
                .iter()
                .any(|workspace| workspace.pane(id).is_some()),
            HandleKind::Surface => self.state.locate_surface(id).is_some(),
        }
    }

    pub fn resolve_window_id(&mut self, raw: &str) -> Option<WindowId> {
        self.resolve_id(HandleKind::Window, raw)
    }

    pub fn resolve_workspace_id(&mut self, raw: &str) -> Option<WorkspaceId> {
        self.resolve_id(HandleKind::Workspace, raw)
    }

    pub fn resolve_pane_id(&mut self, raw: &str) -> Option<PaneId> {
        self.resolve_id(HandleKind::Pane, raw)
    }

    pub fn resolve_surface_id(&mut self, raw: &str) -> Option<SurfaceId> {
        self.resolve_id(HandleKind::Surface, raw)
    }

    pub fn window_ref(&mut self, window_id: WindowId) -> String {
        self.refresh_handles();
        self.handles.ensure_ref(HandleKind::Window, window_id)
    }

    pub fn workspace_ref(&mut self, workspace_id: WorkspaceId) -> String {
        self.refresh_handles();
        self.handles.ensure_ref(HandleKind::Workspace, workspace_id)
    }

    pub fn pane_ref(&mut self, pane_id: PaneId) -> String {
        self.refresh_handles();
        self.handles.ensure_ref(HandleKind::Pane, pane_id)
    }

    pub fn surface_ref(&mut self, surface_id: SurfaceId) -> String {
        self.refresh_handles();
        self.handles.ensure_ref(HandleKind::Surface, surface_id)
    }

    pub fn tab_ref(&mut self, surface_id: SurfaceId) -> String {
        self.surface_ref(surface_id).replacen("surface:", "tab:", 1)
    }

    pub fn app_focus_active(&self) -> bool {
        matches!(self.app_focus_override, Some(AppFocusOverride::Active))
    }

    pub fn palette_state(&self, window_id: WindowId) -> CommandPaletteState {
        self.command_palette_by_window
            .get(&window_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn palette_state_mut(&mut self, window_id: WindowId) -> &mut CommandPaletteState {
        self.command_palette_by_window.entry(window_id).or_default()
    }

    pub fn sidebar_visible(&self, window_id: WindowId) -> bool {
        self.sidebar_visible_by_window
            .get(&window_id)
            .copied()
            .unwrap_or(true)
    }

    pub fn toggle_sidebar(&mut self, window_id: WindowId) -> bool {
        let next = !self.sidebar_visible(window_id);
        self.sidebar_visible_by_window.insert(window_id, next);
        next
    }

    pub fn close_palette(&mut self, window_id: WindowId) {
        let state = self.palette_state_mut(window_id);
        state.visible = false;
        state.rename_target = None;
    }

    pub fn open_palette_mode(&mut self, window_id: WindowId, mode: CommandPaletteMode) {
        let state = self.palette_state_mut(window_id);
        state.visible = true;
        state.mode = mode;
        state.selected_index = 0;
        state.rename_target = None;
        match mode {
            CommandPaletteMode::Commands => {
                state.query = ">".to_string();
                state.text = state.query.clone();
                state.selection_location = state.text.len();
                state.selection_length = 0;
            }
            CommandPaletteMode::Switcher => {
                state.query.clear();
                state.text.clear();
                state.selection_location = 0;
                state.selection_length = 0;
            }
            CommandPaletteMode::RenameInput => {}
        }
    }

    pub fn open_palette_rename_input(
        &mut self,
        window_id: WindowId,
        target: RenameTarget,
        text: String,
    ) {
        let select_all = self.rename_select_all && !text.is_empty();
        let state = self.palette_state_mut(window_id);
        state.visible = true;
        state.mode = CommandPaletteMode::RenameInput;
        state.selected_index = 0;
        state.query = text.clone();
        state.text = text;
        state.rename_target = Some(target);
        state.selection_location = if select_all { 0 } else { state.text.len() };
        state.selection_length = if select_all { state.text.len() } else { 0 };
    }

    pub fn select_all_palette_text(&mut self, window_id: WindowId) {
        let state = self.palette_state_mut(window_id);
        state.selection_location = 0;
        state.selection_length = state.text.len();
    }

    pub fn replace_palette_text(&mut self, window_id: WindowId, replacement: &str) {
        let state = self.palette_state_mut(window_id);
        if state.selection_length > 0 {
            let start = state.selection_location.min(state.text.len());
            let end = (start + state.selection_length).min(state.text.len());
            state.text.replace_range(start..end, replacement);
        } else {
            state.text.push_str(replacement);
        }
        state.selection_location = state.text.len();
        state.selection_length = 0;
        state.query = state.text.clone();
        state.selected_index = 0;
    }

    pub fn delete_backward_palette_text(&mut self, window_id: WindowId) {
        let state = self.palette_state_mut(window_id);
        if state.selection_length > 0 {
            let start = state.selection_location.min(state.text.len());
            let end = (start + state.selection_length).min(state.text.len());
            state.text.replace_range(start..end, "");
        } else if !state.text.is_empty() {
            state.text.pop();
        }
        state.selection_location = state.text.len();
        state.selection_length = 0;
        state.query = state.text.clone();
        state.selected_index = 0;
    }

    pub fn set_shortcut_override(&mut self, name: &str, combo: &str) {
        if combo.eq_ignore_ascii_case("clear") {
            self.shortcut_overrides.remove(name);
        } else {
            self.shortcut_overrides
                .insert(name.to_string(), combo.to_string());
        }
    }

    pub fn shortcut_hint(&self, name: &str) -> String {
        let combo = self
            .shortcut_overrides
            .get(name)
            .map(String::as_str)
            .unwrap_or(match name {
                "new_window" => "cmd+shift+n",
                "close_window" => "cmd+ctrl+w",
                "rename_tab" => "cmd+r",
                _ => "",
            });
        format_shortcut_hint(combo)
    }
}

fn format_shortcut_hint(combo: &str) -> String {
    let combo = combo.trim();
    if combo.is_empty() {
        return String::new();
    }

    let mut has_shift = false;
    let mut has_option = false;
    let mut has_control = false;
    let mut has_command = false;
    let mut key = String::new();
    for part in combo.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" => has_command = true,
            "ctrl" | "control" => has_control = true,
            "shift" => has_shift = true,
            "opt" | "alt" | "option" => has_option = true,
            other => {
                key = match other {
                    "enter" | "return" => "↩".to_string(),
                    "space" => "Space".to_string(),
                    _ => other.to_ascii_uppercase(),
                };
            }
        }
    }
    let mut modifiers = String::new();
    if has_shift {
        modifiers.push('⇧');
    }
    if has_option {
        modifiers.push('⌥');
    }
    if has_control {
        modifiers.push('⌃');
    }
    if has_command {
        modifiers.push('⌘');
    }
    format!("{modifiers}{key}")
}
