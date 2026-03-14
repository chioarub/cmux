use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

pub type WindowId = Uuid;
pub type WorkspaceId = Uuid;
pub type PaneId = Uuid;
pub type SurfaceId = Uuid;
pub type NotificationId = Uuid;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SplitOrientation {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum SurfaceKind {
    Terminal,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalHealth {
    #[serde(default)]
    pub realized: bool,
    #[serde(default)]
    pub startup_error: Option<String>,
    #[serde(default)]
    pub io_thread_main_started: bool,
    #[serde(default)]
    pub io_thread_entered: bool,
    #[serde(default)]
    pub subprocess_start_attempted: bool,
    #[serde(default)]
    pub child_pid: Option<i32>,
    #[serde(default)]
    pub child_exited: bool,
    #[serde(default)]
    pub child_exit_code: Option<u32>,
    #[serde(default)]
    pub child_runtime_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surface {
    pub id: SurfaceId,
    pub title: String,
    pub kind: SurfaceKind,
    #[serde(default)]
    pub current_directory: Option<String>,
    pub unread: bool,
    #[serde(default)]
    pub unread_activity: bool,
    #[serde(default)]
    pub unread_notification: bool,
    pub flash_count: u64,
    pub transcript: String,
    #[serde(default)]
    pub terminal_health: TerminalHealth,
}

impl Surface {
    fn new_terminal(id: SurfaceId, title: String) -> Self {
        Self {
            id,
            title,
            kind: SurfaceKind::Terminal,
            current_directory: None,
            unread: false,
            unread_activity: false,
            unread_notification: false,
            flash_count: 0,
            transcript: String::new(),
            terminal_health: TerminalHealth::default(),
        }
    }

    fn sync_unread(&mut self) {
        self.unread = self.unread_activity || self.unread_notification;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pane {
    pub id: PaneId,
    pub surfaces: Vec<Surface>,
    pub selected_surface_id: SurfaceId,
}

impl Pane {
    pub fn selected_surface(&self) -> Option<&Surface> {
        self.surface(self.selected_surface_id)
    }

    fn selected_surface_index(&self) -> Option<usize> {
        self.surface_index(self.selected_surface_id)
    }

    pub fn surface(&self, surface_id: SurfaceId) -> Option<&Surface> {
        self.surfaces
            .iter()
            .find(|surface| surface.id == surface_id)
    }

    pub fn surface_mut(&mut self, surface_id: SurfaceId) -> Option<&mut Surface> {
        self.surfaces
            .iter_mut()
            .find(|surface| surface.id == surface_id)
    }

    fn surface_index(&self, surface_id: SurfaceId) -> Option<usize> {
        self.surfaces
            .iter()
            .position(|surface| surface.id == surface_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkspaceLayout {
    Pane(PaneId),
    Split {
        orientation: SplitOrientation,
        first: Box<WorkspaceLayout>,
        second: Box<WorkspaceLayout>,
    },
}

impl WorkspaceLayout {
    fn split_leaf(
        &mut self,
        target_pane_id: PaneId,
        new_pane_id: PaneId,
        orientation: SplitOrientation,
        insert_first: bool,
    ) -> bool {
        match self {
            WorkspaceLayout::Pane(existing_pane_id) => {
                if *existing_pane_id != target_pane_id {
                    return false;
                }

                let current = WorkspaceLayout::Pane(*existing_pane_id);
                let new_leaf = WorkspaceLayout::Pane(new_pane_id);
                let (first, second) = if insert_first {
                    (new_leaf, current)
                } else {
                    (current, new_leaf)
                };

                *self = WorkspaceLayout::Split {
                    orientation,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            WorkspaceLayout::Split { first, second, .. } => {
                first.split_leaf(target_pane_id, new_pane_id, orientation, insert_first)
                    || second.split_leaf(target_pane_id, new_pane_id, orientation, insert_first)
            }
        }
    }

    fn without_pane(self, target_pane_id: PaneId) -> Option<Self> {
        match self {
            WorkspaceLayout::Pane(pane_id) => {
                (pane_id != target_pane_id).then_some(WorkspaceLayout::Pane(pane_id))
            }
            WorkspaceLayout::Split {
                orientation,
                first,
                second,
            } => match (
                first.without_pane(target_pane_id),
                second.without_pane(target_pane_id),
            ) {
                (Some(first), Some(second)) => Some(WorkspaceLayout::Split {
                    orientation,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(layout), None) | (None, Some(layout)) => Some(layout),
                (None, None) => None,
            },
        }
    }

    fn remove_pane(&mut self, target_pane_id: PaneId) -> bool {
        let current = std::mem::replace(self, WorkspaceLayout::Pane(target_pane_id));
        match current.without_pane(target_pane_id) {
            Some(next) => {
                *self = next;
                true
            }
            None => false,
        }
    }

    fn collect_pane_ids(&self, output: &mut Vec<PaneId>) {
        match self {
            WorkspaceLayout::Pane(pane_id) => output.push(*pane_id),
            WorkspaceLayout::Split { first, second, .. } => {
                first.collect_pane_ids(output);
                second.collect_pane_ids(output);
            }
        }
    }

    fn collect_pane_regions(
        &self,
        output: &mut Vec<PaneRegion>,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    ) {
        match self {
            WorkspaceLayout::Pane(pane_id) => output.push(PaneRegion {
                pane_id: *pane_id,
                x,
                y,
                width,
                height,
            }),
            WorkspaceLayout::Split {
                orientation,
                first,
                second,
            } => match orientation {
                SplitOrientation::Horizontal => {
                    let split_width = width / 2.0;
                    first.collect_pane_regions(output, x, y, split_width, height);
                    second.collect_pane_regions(
                        output,
                        x + split_width,
                        y,
                        width - split_width,
                        height,
                    );
                }
                SplitOrientation::Vertical => {
                    let split_height = height / 2.0;
                    first.collect_pane_regions(output, x, y, width, split_height);
                    second.collect_pane_regions(
                        output,
                        x,
                        y + split_height,
                        width,
                        height - split_height,
                    );
                }
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PaneRegion {
    pane_id: PaneId,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl PaneRegion {
    fn left(self) -> f64 {
        self.x
    }

    fn right(self) -> f64 {
        self.x + self.width
    }

    fn top(self) -> f64 {
        self.y
    }

    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn center_x(self) -> f64 {
        self.x + (self.width / 2.0)
    }

    fn center_y(self) -> f64 {
        self.y + (self.height / 2.0)
    }

    fn overlap_y(self, other: Self) -> f64 {
        (self.bottom().min(other.bottom()) - self.top().max(other.top())).max(0.0)
    }

    fn overlap_x(self, other: Self) -> f64 {
        (self.right().min(other.right()) - self.left().max(other.left())).max(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    #[serde(default = "Uuid::new_v4")]
    pub window_id: WindowId,
    pub title: String,
    #[serde(default)]
    pub current_directory: Option<String>,
    pub layout: WorkspaceLayout,
    pub panes: Vec<Pane>,
    pub selected_pane_id: PaneId,
    pub last_selected_pane_id: Option<PaneId>,
}

impl Workspace {
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    pub fn surface_count(&self) -> usize {
        self.panes.iter().map(|pane| pane.surfaces.len()).sum()
    }

    pub fn selected_pane(&self) -> Option<&Pane> {
        self.pane(self.selected_pane_id)
    }

    pub fn pane(&self, pane_id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == pane_id)
    }

    pub fn pane_mut(&mut self, pane_id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == pane_id)
    }

    pub fn ordered_pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        self.layout.collect_pane_ids(&mut ids);
        for pane in &self.panes {
            if !ids.contains(&pane.id) {
                ids.push(pane.id);
            }
        }
        ids
    }

    pub fn ordered_panes(&self) -> Vec<&Pane> {
        self.ordered_pane_ids()
            .into_iter()
            .filter_map(|pane_id| self.pane(pane_id))
            .collect()
    }

    pub fn selected_surface_id(&self) -> Option<SurfaceId> {
        self.selected_pane().map(|pane| pane.selected_surface_id)
    }

    pub fn selected_surface_neighbor(&self, delta: isize) -> Result<SurfaceId, String> {
        let pane = self
            .selected_pane()
            .ok_or_else(|| format!("pane {} not found", self.selected_pane_id))?;
        if pane.surfaces.is_empty() {
            return Err(format!("pane {} has no surfaces", pane.id));
        }

        let current_index = pane.selected_surface_index().unwrap_or(0);
        let len = pane.surfaces.len() as isize;
        let next_index = (current_index as isize + delta).rem_euclid(len) as usize;
        Ok(pane.surfaces[next_index].id)
    }

    pub fn pane_id_for_surface(&self, surface_id: SurfaceId) -> Option<PaneId> {
        self.panes
            .iter()
            .find(|pane| pane.surfaces.iter().any(|surface| surface.id == surface_id))
            .map(|pane| pane.id)
    }

    pub fn surface(&self, surface_id: SurfaceId) -> Option<&Surface> {
        self.panes.iter().find_map(|pane| pane.surface(surface_id))
    }

    pub fn surface_mut(&mut self, surface_id: SurfaceId) -> Option<&mut Surface> {
        self.panes
            .iter_mut()
            .find_map(|pane| pane.surface_mut(surface_id))
    }

    pub fn focus_pane(&mut self, pane_id: PaneId) -> Result<(), String> {
        let selected_surface_id = self
            .pane(pane_id)
            .map(|pane| pane.selected_surface_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        if self.selected_pane_id != pane_id {
            self.last_selected_pane_id = Some(self.selected_pane_id);
        }
        self.selected_pane_id = pane_id;
        if let Some(pane) = self.pane_mut(pane_id) {
            pane.selected_surface_id = selected_surface_id;
        }
        Ok(())
    }

    pub fn focus_surface(&mut self, surface_id: SurfaceId) -> Result<PaneId, String> {
        let pane_id = self
            .pane_id_for_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        if self.selected_pane_id != pane_id {
            self.last_selected_pane_id = Some(self.selected_pane_id);
        }
        let pane = self
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        pane.selected_surface_id = surface_id;
        self.selected_pane_id = pane_id;
        Ok(pane_id)
    }

    pub fn focus_last_pane(&mut self) -> Result<PaneId, String> {
        let pane_id = self
            .last_selected_pane_id
            .filter(|candidate| self.pane(*candidate).is_some())
            .unwrap_or(self.selected_pane_id);
        self.focus_pane(pane_id)?;
        Ok(pane_id)
    }

    pub fn focus_adjacent_pane(&mut self, direction: FocusDirection) -> Result<PaneId, String> {
        let current_pane_id = self.selected_pane_id;
        let target_pane_id = self.adjacent_pane_id(direction).unwrap_or(current_pane_id);
        self.focus_pane(target_pane_id)?;
        Ok(target_pane_id)
    }

    pub fn split_pane(
        &mut self,
        target_pane_id: PaneId,
        orientation: SplitOrientation,
        insert_first: bool,
        select_new: bool,
        new_pane: Pane,
    ) -> Result<PaneId, String> {
        if self.pane(target_pane_id).is_none() {
            return Err(format!("pane {target_pane_id} not found"));
        }

        let new_pane_id = new_pane.id;
        self.panes.push(new_pane);
        if self
            .layout
            .split_leaf(target_pane_id, new_pane_id, orientation, insert_first)
        {
            if select_new {
                self.last_selected_pane_id = Some(self.selected_pane_id);
                self.selected_pane_id = new_pane_id;
            }
            return Ok(new_pane_id);
        }

        let _ = self.panes.pop();
        Err(format!("pane {target_pane_id} could not be split"))
    }

    pub fn create_surface_in_pane(
        &mut self,
        pane_id: PaneId,
        surface: Surface,
        select_new: bool,
    ) -> Result<SurfaceId, String> {
        let pane = self
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let surface_id = surface.id;
        pane.surfaces.push(surface);
        if select_new {
            pane.selected_surface_id = surface_id;
            self.selected_pane_id = pane_id;
        }
        Ok(surface_id)
    }

    pub fn rename(&mut self, title: String) -> bool {
        if self.title == title {
            return false;
        }
        self.title = title;
        true
    }

    pub fn insert_surface_in_pane(
        &mut self,
        pane_id: PaneId,
        surface: Surface,
        target_index: usize,
        select_new: bool,
    ) -> Result<SurfaceId, String> {
        let pane = self
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let surface_id = surface.id;
        let insert_index = target_index.min(pane.surfaces.len());
        pane.surfaces.insert(insert_index, surface);
        if select_new {
            pane.selected_surface_id = surface_id;
            self.selected_pane_id = pane_id;
        }
        Ok(surface_id)
    }

    pub fn reorder_surface(
        &mut self,
        surface_id: SurfaceId,
        target_index: usize,
    ) -> Result<(), String> {
        let pane_id = self
            .pane_id_for_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let pane = self
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let current_index = pane
            .surface_index(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let surface = pane.surfaces.remove(current_index);
        let insert_index = target_index.min(pane.surfaces.len());
        pane.surfaces.insert(insert_index, surface);
        Ok(())
    }

    pub fn remove_surface_for_move(&mut self, surface_id: SurfaceId) -> Result<Surface, String> {
        let pane_id = self
            .pane_id_for_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let was_selected_pane = self.selected_pane_id == pane_id;
        let pane_index = self
            .panes
            .iter()
            .position(|pane| pane.id == pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let pane_surface_count = self.panes[pane_index].surfaces.len();

        if pane_surface_count <= 1 && self.panes.len() <= 1 {
            return Err("cannot move the last surface".to_string());
        }

        if pane_surface_count <= 1 {
            let removed = self.layout.remove_pane(pane_id);
            if !removed {
                return Err("cannot remove the last pane".to_string());
            }
            let mut pane = self.panes.remove(pane_index);
            if self.last_selected_pane_id == Some(pane_id) {
                self.last_selected_pane_id = None;
            }
            if was_selected_pane {
                self.selected_pane_id = self
                    .ordered_pane_ids()
                    .into_iter()
                    .find(|candidate| self.pane(*candidate).is_some())
                    .ok_or_else(|| "no pane remaining after move".to_string())?;
            }
            return pane
                .surfaces
                .pop()
                .ok_or_else(|| format!("surface {surface_id} not found"));
        }

        let pane = self
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let current_index = pane
            .surface_index(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let was_selected_surface = pane.selected_surface_id == surface_id;
        let surface = pane.surfaces.remove(current_index);
        if was_selected_surface {
            let next_index = current_index.saturating_sub(1).min(pane.surfaces.len() - 1);
            pane.selected_surface_id = pane.surfaces[next_index].id;
        }
        if was_selected_pane {
            self.selected_pane_id = pane_id;
        }
        Ok(surface)
    }

    pub fn close_surface(&mut self, surface_id: SurfaceId) -> Result<(PaneId, bool), String> {
        let pane_id = self
            .pane_id_for_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let was_selected_pane = self.selected_pane_id == pane_id;
        let pane_index = self
            .panes
            .iter()
            .position(|pane| pane.id == pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;

        let pane_surface_count = self.panes[pane_index].surfaces.len();
        if pane_surface_count <= 1 && self.panes.len() <= 1 {
            return Err("cannot close the last surface".to_string());
        }

        if pane_surface_count <= 1 {
            let removed = self.layout.remove_pane(pane_id);
            if !removed {
                return Err("cannot remove the last pane".to_string());
            }
            self.panes.remove(pane_index);
            if self.last_selected_pane_id == Some(pane_id) {
                self.last_selected_pane_id = None;
            }
            if was_selected_pane {
                self.selected_pane_id = self
                    .ordered_pane_ids()
                    .into_iter()
                    .find(|candidate| self.pane(*candidate).is_some())
                    .ok_or_else(|| "no pane remaining after close".to_string())?;
            }
            return Ok((pane_id, true));
        }

        let pane = self
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let was_selected_surface = pane.selected_surface_id == surface_id;
        let index = pane
            .surface_index(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        pane.surfaces.remove(index);
        if was_selected_surface {
            let next_index = index.saturating_sub(1).min(pane.surfaces.len() - 1);
            pane.selected_surface_id = pane.surfaces[next_index].id;
        }
        if was_selected_pane {
            self.selected_pane_id = pane_id;
        }
        Ok((pane_id, false))
    }

    fn pane_regions(&self) -> Vec<PaneRegion> {
        let mut regions = Vec::new();
        self.layout
            .collect_pane_regions(&mut regions, 0.0, 0.0, 1.0, 1.0);
        regions
    }

    pub fn debug_pane_regions(&self) -> Vec<(PaneId, f64, f64, f64, f64)> {
        self.pane_regions()
            .into_iter()
            .map(|region| {
                (
                    region.pane_id,
                    region.x,
                    region.y,
                    region.width,
                    region.height,
                )
            })
            .collect()
    }

    fn adjacent_pane_id(&self, direction: FocusDirection) -> Option<PaneId> {
        let regions = self.pane_regions();
        let current = regions
            .iter()
            .copied()
            .find(|region| region.pane_id == self.selected_pane_id)?;
        let mut best: Option<(PaneRegion, f64, f64, f64)> = None;

        for candidate in regions.iter().copied() {
            if candidate.pane_id == current.pane_id {
                continue;
            }

            let (primary_distance, secondary_distance, overlap) = match direction {
                FocusDirection::Left if candidate.center_x() < current.center_x() => (
                    (current.left() - candidate.right()).max(0.0),
                    (candidate.center_y() - current.center_y()).abs(),
                    current.overlap_y(candidate),
                ),
                FocusDirection::Right if candidate.center_x() > current.center_x() => (
                    (candidate.left() - current.right()).max(0.0),
                    (candidate.center_y() - current.center_y()).abs(),
                    current.overlap_y(candidate),
                ),
                FocusDirection::Up if candidate.center_y() < current.center_y() => (
                    (current.top() - candidate.bottom()).max(0.0),
                    (candidate.center_x() - current.center_x()).abs(),
                    current.overlap_x(candidate),
                ),
                FocusDirection::Down if candidate.center_y() > current.center_y() => (
                    (candidate.top() - current.bottom()).max(0.0),
                    (candidate.center_x() - current.center_x()).abs(),
                    current.overlap_x(candidate),
                ),
                _ => continue,
            };

            let replace = match best {
                None => true,
                Some((_, best_primary, best_secondary, best_overlap)) => {
                    overlap > best_overlap
                        || ((overlap - best_overlap).abs() < f64::EPSILON
                            && (primary_distance < best_primary
                                || ((primary_distance - best_primary).abs() < f64::EPSILON
                                    && secondary_distance < best_secondary)))
                }
            };

            if replace {
                best = Some((candidate, primary_distance, secondary_distance, overlap));
            }
        }

        best.map(|(candidate, _, _, _)| candidate.pane_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub id: WindowId,
    pub selected_workspace_id: WorkspaceId,
    pub last_selected_workspace_id: Option<WorkspaceId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: NotificationId,
    pub workspace_id: WorkspaceId,
    pub surface_id: Option<SurfaceId>,
    pub is_read: bool,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub delivered: bool,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub windows: Vec<WindowState>,
    pub window_id: WindowId,
    pub workspaces: Vec<Workspace>,
    pub selected_workspace_id: WorkspaceId,
    pub last_selected_workspace_id: Option<WorkspaceId>,
    pub notifications: Vec<Notification>,
    revision: u64,
    next_workspace_title_number: usize,
    next_surface_title_number: usize,
}

impl AppState {
    pub fn new() -> Self {
        let mut state = Self {
            windows: Vec::new(),
            window_id: Uuid::nil(),
            workspaces: Vec::new(),
            selected_workspace_id: Uuid::nil(),
            last_selected_workspace_id: None,
            notifications: Vec::new(),
            revision: 0,
            next_workspace_title_number: 1,
            next_surface_title_number: 1,
        };
        let (window_id, workspace_id) = state.create_window_with_focus(true);
        state.window_id = window_id;
        state.selected_workspace_id = workspace_id;
        state
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn touch(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn next_workspace_title(&mut self) -> String {
        let title = format!("Workspace {}", self.next_workspace_title_number);
        self.next_workspace_title_number += 1;
        title
    }

    fn next_terminal_title(&mut self) -> String {
        let title = format!("Terminal {}", self.next_surface_title_number);
        self.next_surface_title_number += 1;
        title
    }

    fn new_terminal_surface(&mut self) -> Surface {
        Surface::new_terminal(Uuid::new_v4(), self.next_terminal_title())
    }

    fn new_terminal_pane(&mut self) -> Pane {
        let surface = self.new_terminal_surface();
        Pane {
            id: Uuid::new_v4(),
            selected_surface_id: surface.id,
            surfaces: vec![surface],
        }
    }

    pub fn selected_workspace(&self) -> Option<&Workspace> {
        self.workspace(self.selected_workspace_id)
    }

    pub fn window(&self, window_id: WindowId) -> Option<&WindowState> {
        self.windows.iter().find(|window| window.id == window_id)
    }

    pub fn window_mut(&mut self, window_id: WindowId) -> Option<&mut WindowState> {
        self.windows
            .iter_mut()
            .find(|window| window.id == window_id)
    }

    pub fn workspace(&self, workspace_id: WorkspaceId) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
    }

    pub fn workspace_mut(&mut self, workspace_id: WorkspaceId) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
    }

    pub fn workspaces_in_window(&self, window_id: WindowId) -> Vec<&Workspace> {
        self.workspaces
            .iter()
            .filter(|workspace| workspace.window_id == window_id)
            .collect()
    }

    pub fn workspace_window_id(&self, workspace_id: WorkspaceId) -> Option<WindowId> {
        self.workspace(workspace_id)
            .map(|workspace| workspace.window_id)
    }

    fn sync_global_selection_from_window(&mut self, window_id: WindowId) {
        let Some((selected_workspace_id, last_selected_workspace_id)) =
            self.window(window_id).map(|window| {
                (
                    window.selected_workspace_id,
                    window.last_selected_workspace_id,
                )
            })
        else {
            self.window_id = Uuid::nil();
            self.selected_workspace_id = Uuid::nil();
            self.last_selected_workspace_id = None;
            return;
        };

        self.window_id = window_id;
        self.selected_workspace_id = selected_workspace_id;
        self.last_selected_workspace_id = last_selected_workspace_id;
    }

    fn sync_window_selection_from_global(&mut self, window_id: WindowId) {
        let selected_workspace_id = self.selected_workspace_id;
        let last_selected_workspace_id = self.last_selected_workspace_id;
        if let Some(window) = self.window_mut(window_id) {
            window.selected_workspace_id = selected_workspace_id;
            window.last_selected_workspace_id = last_selected_workspace_id;
        }
    }

    fn repair_window_selection(&mut self, window_id: WindowId) {
        let workspace_ids = self
            .workspaces_in_window(window_id)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let selected_workspace_id = workspace_ids.first().copied().unwrap_or(Uuid::nil());
        let Some(window) = self.window_mut(window_id) else {
            return;
        };
        if !workspace_ids.contains(&window.selected_workspace_id) {
            window.selected_workspace_id = selected_workspace_id;
        }
        window.last_selected_workspace_id = window
            .last_selected_workspace_id
            .filter(|candidate| workspace_ids.contains(candidate));
    }

    pub fn focus_window(&mut self, window_id: WindowId) -> Result<WorkspaceId, String> {
        if self.window(window_id).is_none() {
            return Err(format!("window {window_id} not found"));
        }
        self.repair_window_selection(window_id);
        let previous_window_id = self.window_id;
        let previous_workspace_id = self.selected_workspace_id;
        self.sync_global_selection_from_window(window_id);
        if self.selected_workspace_id.is_nil() {
            return Err(format!("window {window_id} has no selected workspace"));
        }

        let selected_surface_id = self
            .workspace(self.selected_workspace_id)
            .and_then(Workspace::selected_surface_id);
        let notification_reads =
            self.mark_notifications_read(self.selected_workspace_id, selected_surface_id);
        let unread_cleared = selected_surface_id
            .map(|surface_id| self.clear_surface_unread(surface_id))
            .unwrap_or(false);
        if let Some(surface_id) = selected_surface_id {
            if notification_reads > 0 {
                let _ = self.increment_surface_flash(surface_id, notification_reads as u64);
            }
        }
        if previous_window_id != window_id
            || previous_workspace_id != self.selected_workspace_id
            || notification_reads > 0
            || unread_cleared
        {
            self.touch();
        }
        Ok(self.selected_workspace_id)
    }

    pub fn create_window_with_focus(&mut self, focus_new: bool) -> (WindowId, WorkspaceId) {
        let new_window_id = Uuid::new_v4();
        self.windows.push(WindowState {
            id: new_window_id,
            selected_workspace_id: Uuid::nil(),
            last_selected_workspace_id: None,
        });
        let workspace_id =
            self.create_workspace_in_window_with_focus_and_cwd(new_window_id, focus_new, None);
        if focus_new || self.window_id.is_nil() {
            self.sync_global_selection_from_window(new_window_id);
        }
        self.touch();
        (new_window_id, workspace_id)
    }

    pub fn close_window(&mut self, window_id: WindowId) -> Result<(), String> {
        let index = self
            .windows
            .iter()
            .position(|window| window.id == window_id)
            .ok_or_else(|| format!("window {window_id} not found"))?;

        let workspace_ids = self
            .workspaces_in_window(window_id)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let valid_surface_ids = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.window_id != window_id)
            .flat_map(|workspace| workspace.panes.iter())
            .flat_map(|pane| pane.surfaces.iter())
            .map(|surface| surface.id)
            .collect::<HashSet<_>>();
        self.workspaces
            .retain(|workspace| workspace.window_id != window_id);
        self.notifications.retain(|notification| {
            !workspace_ids.contains(&notification.workspace_id)
                && notification
                    .surface_id
                    .map(|surface_id| valid_surface_ids.contains(&surface_id))
                    .unwrap_or(true)
        });
        self.windows.remove(index);

        if self.windows.is_empty() {
            let (new_window_id, new_workspace_id) = self.create_window_with_focus(true);
            self.window_id = new_window_id;
            self.selected_workspace_id = new_workspace_id;
            self.last_selected_workspace_id = None;
            self.touch();
            return Ok(());
        }

        let fallback_window_id = self
            .windows
            .get(index.saturating_sub(1).min(self.windows.len() - 1))
            .map(|window| window.id)
            .unwrap_or_else(|| self.windows[0].id);
        self.sync_global_selection_from_window(fallback_window_id);
        self.touch();
        Ok(())
    }

    pub fn create_workspace(&mut self) -> WorkspaceId {
        self.create_workspace_with_focus(true)
    }

    pub fn create_workspace_with_focus(&mut self, focus_new: bool) -> WorkspaceId {
        self.create_workspace_with_focus_and_cwd(focus_new, None)
    }

    pub fn create_workspace_with_focus_and_cwd(
        &mut self,
        focus_new: bool,
        current_directory: Option<String>,
    ) -> WorkspaceId {
        self.create_workspace_in_window_with_focus_and_cwd(
            self.window_id,
            focus_new,
            current_directory,
        )
    }

    pub fn create_workspace_in_window_with_focus_and_cwd(
        &mut self,
        window_id: WindowId,
        focus_new: bool,
        current_directory: Option<String>,
    ) -> WorkspaceId {
        let pane = self.new_terminal_pane();
        let pane_id = pane.id;
        let workspace_id = Uuid::new_v4();
        let workspace = Workspace {
            id: workspace_id,
            window_id,
            title: self.next_workspace_title(),
            current_directory,
            layout: WorkspaceLayout::Pane(pane_id),
            panes: vec![pane],
            selected_pane_id: pane_id,
            last_selected_pane_id: None,
        };

        self.workspaces.push(workspace);
        if let Some(window) = self.window_mut(window_id) {
            if focus_new || window.selected_workspace_id == Uuid::nil() {
                window.last_selected_workspace_id =
                    Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                window.selected_workspace_id = workspace_id;
            }
        }
        if window_id == self.window_id && (focus_new || self.selected_workspace_id == Uuid::nil()) {
            self.last_selected_workspace_id =
                Some(self.selected_workspace_id).filter(|id| *id != Uuid::nil());
            self.selected_workspace_id = workspace_id;
        }
        self.touch();
        workspace_id
    }

    pub fn select_workspace(&mut self, workspace_id: WorkspaceId) -> Result<(), String> {
        let window_id = self
            .workspace_window_id(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if self
            .workspaces
            .iter()
            .any(|workspace| workspace.id == workspace_id)
        {
            let previous_selected_workspace_id = self.selected_workspace_id;
            let previous_window_id = self.window_id;
            if let Some(window) = self.window_mut(window_id) {
                if window.selected_workspace_id != workspace_id {
                    window.last_selected_workspace_id =
                        Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                    window.selected_workspace_id = workspace_id;
                }
            }
            self.sync_global_selection_from_window(window_id);
            let selected_surface_id = self
                .workspace(workspace_id)
                .and_then(Workspace::selected_surface_id);
            let notification_reads =
                self.mark_notifications_read(workspace_id, selected_surface_id);
            let unread_cleared = selected_surface_id
                .map(|surface_id| self.clear_surface_unread(surface_id))
                .unwrap_or(false);
            if let Some(surface_id) = selected_surface_id {
                if notification_reads > 0 {
                    let _ = self.increment_surface_flash(surface_id, notification_reads as u64);
                }
            }
            if previous_window_id != window_id
                || previous_selected_workspace_id != workspace_id
                || notification_reads > 0
                || unread_cleared
            {
                self.touch();
            }
            return Ok(());
        }
        Err(format!("workspace {workspace_id} not found"))
    }

    pub fn select_next_workspace(&mut self) -> Result<WorkspaceId, String> {
        self.select_relative_workspace(1)
    }

    pub fn select_previous_workspace(&mut self) -> Result<WorkspaceId, String> {
        self.select_relative_workspace(-1)
    }

    pub fn select_last_workspace(&mut self) -> Result<WorkspaceId, String> {
        let workspace_id = self
            .last_selected_workspace_id
            .filter(|candidate| self.workspace(*candidate).is_some())
            .unwrap_or(self.selected_workspace_id);
        self.select_workspace(workspace_id)?;
        Ok(workspace_id)
    }

    fn select_relative_workspace(&mut self, delta: isize) -> Result<WorkspaceId, String> {
        let window_id = self.window_id;
        let window_workspaces = self.workspaces_in_window(window_id);
        if window_workspaces.is_empty() {
            return Err("no workspaces".to_string());
        }

        let current_index = window_workspaces
            .iter()
            .position(|workspace| workspace.id == self.selected_workspace_id)
            .unwrap_or(0);
        let len = window_workspaces.len() as isize;
        let next_index = (current_index as isize + delta).rem_euclid(len) as usize;
        let workspace_id = window_workspaces[next_index].id;
        self.select_workspace(workspace_id)?;
        Ok(workspace_id)
    }

    pub fn close_workspace(&mut self, workspace_id: WorkspaceId) -> Result<(), String> {
        let window_id = self
            .workspace_window_id(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;

        let window_workspace_ids = self
            .workspaces_in_window(window_id)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        if window_workspace_ids.len() == 1 {
            self.workspaces.remove(index);
            let new_workspace_id =
                self.create_workspace_in_window_with_focus_and_cwd(window_id, true, None);
            if window_id == self.window_id {
                self.selected_workspace_id = new_workspace_id;
                self.last_selected_workspace_id = None;
                self.sync_window_selection_from_global(window_id);
            }
            self.touch();
            return Ok(());
        }

        self.workspaces.remove(index);
        let remaining = self
            .workspaces_in_window(window_id)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let fallback_workspace_id = self
            .window(window_id)
            .and_then(|window| {
                window
                    .last_selected_workspace_id
                    .filter(|candidate| remaining.contains(candidate))
            })
            .unwrap_or_else(|| remaining[0]);
        if let Some(window) = self.window_mut(window_id) {
            window.selected_workspace_id = fallback_workspace_id;
            window.last_selected_workspace_id = None;
        }
        if window_id == self.window_id {
            self.selected_workspace_id = fallback_workspace_id;
            self.last_selected_workspace_id = None;
        }
        self.touch();
        Ok(())
    }

    pub fn rename_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        title: String,
    ) -> Result<(), String> {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err("workspace title cannot be empty".to_string());
        }
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if workspace.rename(title) {
            self.touch();
        }
        Ok(())
    }

    pub fn reorder_workspace_in_window(
        &mut self,
        workspace_id: WorkspaceId,
        window_id: WindowId,
        target_index: usize,
    ) -> Result<(), String> {
        let source_window_id = self
            .workspace_window_id(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if source_window_id != window_id {
            return Err(format!(
                "workspace {workspace_id} is not in window {window_id}"
            ));
        }

        let source_index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let workspace = self.workspaces.remove(source_index);
        let remaining_window_indices = self
            .workspaces
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.window_id == window_id)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let target_index = target_index.min(remaining_window_indices.len());
        let insert_index = if remaining_window_indices.is_empty() {
            self.workspaces.len()
        } else if target_index >= remaining_window_indices.len() {
            remaining_window_indices
                .last()
                .copied()
                .map(|index| index + 1)
                .unwrap_or(self.workspaces.len())
        } else {
            remaining_window_indices[target_index]
        };
        self.workspaces.insert(insert_index, workspace);
        self.touch();
        Ok(())
    }

    pub fn rename_surface(
        &mut self,
        surface_id: SurfaceId,
        title: String,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err("surface title cannot be empty".to_string());
        }

        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let pane = workspace
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let surface = pane
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        if surface.title == trimmed {
            return Ok((workspace_id, pane_id));
        }
        surface.title = trimmed.to_string();
        self.touch();
        Ok((workspace_id, pane_id))
    }

    pub fn clear_surface_title(
        &mut self,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId, String), String> {
        let default_title = self.next_terminal_title();
        let (workspace_id, pane_id) = self.rename_surface(surface_id, default_title.clone())?;
        Ok((workspace_id, pane_id, default_title))
    }

    pub fn mark_surface_unread(
        &mut self,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let pane = workspace
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let surface = pane
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        surface.unread_activity = true;
        surface.sync_unread();
        self.touch();
        Ok((workspace_id, pane_id))
    }

    pub fn mark_surface_read(
        &mut self,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let pane = workspace
            .pane_mut(pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let surface = pane
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        surface.unread_activity = false;
        surface.unread_notification = false;
        surface.sync_unread();
        self.touch();
        Ok((workspace_id, pane_id))
    }

    pub fn move_workspace_to_window(
        &mut self,
        workspace_id: WorkspaceId,
        target_window_id: WindowId,
        focus: bool,
    ) -> Result<(), String> {
        if self.window(target_window_id).is_none() {
            return Err(format!("window {target_window_id} not found"));
        }
        let source_window_id = self
            .workspace_window_id(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if source_window_id == target_window_id {
            if focus {
                let _ = self.focus_window(target_window_id)?;
                self.select_workspace(workspace_id)?;
            }
            return Ok(());
        }

        let source_workspace_ids = self
            .workspaces_in_window(source_window_id)
            .into_iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let source_selected_workspace_id = self
            .window(source_window_id)
            .map(|window| window.selected_workspace_id)
            .unwrap_or(Uuid::nil());

        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        workspace.window_id = target_window_id;

        if let Some(target_window) = self.window_mut(target_window_id) {
            if focus || target_window.selected_workspace_id.is_nil() {
                target_window.last_selected_workspace_id =
                    Some(target_window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                target_window.selected_workspace_id = workspace_id;
            }
        }

        let source_remaining = source_workspace_ids
            .into_iter()
            .filter(|candidate| *candidate != workspace_id)
            .collect::<Vec<_>>();
        if source_remaining.is_empty() {
            let replacement_workspace_id =
                self.create_workspace_in_window_with_focus_and_cwd(source_window_id, false, None);
            if let Some(source_window) = self.window_mut(source_window_id) {
                source_window.selected_workspace_id = replacement_workspace_id;
                source_window.last_selected_workspace_id = None;
            }
        } else if let Some(source_window) = self.window_mut(source_window_id) {
            if source_selected_workspace_id == workspace_id
                || source_window.selected_workspace_id == workspace_id
            {
                let fallback_workspace_id = source_window
                    .last_selected_workspace_id
                    .filter(|candidate| source_remaining.contains(candidate))
                    .unwrap_or(source_remaining[0]);
                source_window.selected_workspace_id = fallback_workspace_id;
                source_window.last_selected_workspace_id = None;
            }
        }

        self.repair_window_selection(source_window_id);
        self.repair_window_selection(target_window_id);
        if focus {
            self.sync_global_selection_from_window(target_window_id);
        } else if self.window_id == source_window_id || self.window_id == target_window_id {
            self.sync_global_selection_from_window(self.window_id);
        }
        self.touch();
        Ok(())
    }

    pub fn move_surface_to_pane(
        &mut self,
        surface_id: SurfaceId,
        destination_workspace_id: WorkspaceId,
        destination_pane_id: PaneId,
        target_index: usize,
        focus: bool,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let (source_workspace_id, source_pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let source_window_id = self
            .workspace_window_id(source_workspace_id)
            .ok_or_else(|| format!("workspace {source_workspace_id} not found"))?;
        let source_workspace_is_single_surface = self
            .workspace(source_workspace_id)
            .map(|workspace| workspace.pane_count() == 1 && workspace.surface_count() == 1)
            .unwrap_or(false);

        if source_workspace_id == destination_workspace_id && source_pane_id == destination_pane_id
        {
            let workspace = self
                .workspace_mut(destination_workspace_id)
                .ok_or_else(|| format!("workspace {destination_workspace_id} not found"))?;
            workspace.reorder_surface(surface_id, target_index)?;
            if focus {
                let _ = self.focus_surface(surface_id)?;
            } else {
                self.touch();
            }
            return Ok((destination_workspace_id, destination_pane_id, surface_id));
        }

        let moved_surface = if source_workspace_is_single_surface {
            let source_index = self
                .workspaces
                .iter()
                .position(|workspace| workspace.id == source_workspace_id)
                .ok_or_else(|| format!("workspace {source_workspace_id} not found"))?;
            let mut workspace = self.workspaces.remove(source_index);
            let moved_surface = workspace
                .panes
                .pop()
                .and_then(|mut pane| pane.surfaces.pop())
                .ok_or_else(|| format!("surface {surface_id} not found"))?;
            let remaining_workspace_ids = self
                .workspaces_in_window(source_window_id)
                .into_iter()
                .map(|candidate| candidate.id)
                .collect::<Vec<_>>();
            if remaining_workspace_ids.is_empty() {
                let replacement_workspace_id = self.create_workspace_in_window_with_focus_and_cwd(
                    source_window_id,
                    false,
                    None,
                );
                if let Some(window) = self.window_mut(source_window_id) {
                    window.selected_workspace_id = replacement_workspace_id;
                    window.last_selected_workspace_id = None;
                }
            } else if let Some(window) = self.window_mut(source_window_id) {
                if window.selected_workspace_id == source_workspace_id {
                    let fallback_workspace_id = window
                        .last_selected_workspace_id
                        .filter(|candidate| remaining_workspace_ids.contains(candidate))
                        .unwrap_or(remaining_workspace_ids[0]);
                    window.selected_workspace_id = fallback_workspace_id;
                    window.last_selected_workspace_id = None;
                }
            }
            if self.window_id == source_window_id {
                self.sync_global_selection_from_window(source_window_id);
            }
            moved_surface
        } else {
            let workspace = self
                .workspace_mut(source_workspace_id)
                .ok_or_else(|| format!("workspace {source_workspace_id} not found"))?;
            workspace.remove_surface_for_move(surface_id)?
        };

        {
            let workspace = self
                .workspace_mut(destination_workspace_id)
                .ok_or_else(|| format!("workspace {destination_workspace_id} not found"))?;
            workspace.insert_surface_in_pane(
                destination_pane_id,
                moved_surface,
                target_index,
                focus,
            )?;
        }

        if focus {
            let _ = self.focus_surface(surface_id)?;
        } else {
            self.touch();
        }
        Ok((destination_workspace_id, destination_pane_id, surface_id))
    }

    pub fn swap_panes(
        &mut self,
        pane_id: PaneId,
        target_pane_id: PaneId,
        focus: bool,
    ) -> Result<WorkspaceId, String> {
        let source_workspace_id = self
            .workspaces
            .iter()
            .find(|workspace| workspace.pane(pane_id).is_some())
            .map(|workspace| workspace.id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let target_workspace_id = self
            .workspaces
            .iter()
            .find(|workspace| workspace.pane(target_pane_id).is_some())
            .map(|workspace| workspace.id)
            .ok_or_else(|| format!("pane {target_pane_id} not found"))?;
        if source_workspace_id != target_workspace_id {
            return Err("pane.swap requires panes in the same workspace".to_string());
        }

        let workspace = self
            .workspace_mut(source_workspace_id)
            .ok_or_else(|| format!("workspace {source_workspace_id} not found"))?;
        let source_index = workspace
            .panes
            .iter()
            .position(|pane| pane.id == pane_id)
            .ok_or_else(|| format!("pane {pane_id} not found"))?;
        let target_index = workspace
            .panes
            .iter()
            .position(|pane| pane.id == target_pane_id)
            .ok_or_else(|| format!("pane {target_pane_id} not found"))?;
        let source_surfaces = workspace.panes[source_index].surfaces.clone();
        let source_selected_surface_id = workspace.panes[source_index].selected_surface_id;
        workspace.panes[source_index].surfaces = workspace.panes[target_index].surfaces.clone();
        workspace.panes[source_index].selected_surface_id =
            workspace.panes[target_index].selected_surface_id;
        workspace.panes[target_index].surfaces = source_surfaces;
        workspace.panes[target_index].selected_surface_id = source_selected_surface_id;
        if focus {
            let _ = workspace.focus_pane(target_pane_id);
            let _ = self.select_workspace(source_workspace_id);
        } else {
            self.touch();
        }
        Ok(source_workspace_id)
    }

    pub fn break_surface_to_workspace(
        &mut self,
        surface_id: SurfaceId,
        focus: bool,
    ) -> Result<WorkspaceId, String> {
        let (source_workspace_id, _) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let source_workspace = self
            .workspace(source_workspace_id)
            .ok_or_else(|| format!("workspace {source_workspace_id} not found"))?;
        let window_id = source_workspace.window_id;
        let current_directory = source_workspace
            .surface(surface_id)
            .and_then(|surface| surface.current_directory.clone())
            .or_else(|| source_workspace.current_directory.clone());
        let moved_surface = {
            let workspace = self
                .workspace_mut(source_workspace_id)
                .ok_or_else(|| format!("workspace {source_workspace_id} not found"))?;
            workspace.remove_surface_for_move(surface_id)?
        };

        let pane_id = Uuid::new_v4();
        let workspace_id = Uuid::new_v4();
        let workspace = Workspace {
            id: workspace_id,
            window_id,
            title: self.next_workspace_title(),
            current_directory,
            layout: WorkspaceLayout::Pane(pane_id),
            panes: vec![Pane {
                id: pane_id,
                selected_surface_id: surface_id,
                surfaces: vec![moved_surface],
            }],
            selected_pane_id: pane_id,
            last_selected_pane_id: None,
        };
        self.workspaces.push(workspace);
        if let Some(window) = self.window_mut(window_id) {
            window.last_selected_workspace_id =
                Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
            if focus {
                window.selected_workspace_id = workspace_id;
            }
        }
        if focus {
            self.sync_global_selection_from_window(window_id);
            let _ = self.focus_surface(surface_id)?;
        } else {
            self.touch();
        }
        Ok(workspace_id)
    }

    pub fn clear_surface_history(&mut self, surface_id: SurfaceId) -> Result<(), String> {
        let (workspace_id, _) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        if surface.transcript.is_empty() {
            return Ok(());
        }
        surface.transcript.clear();
        surface.unread_activity = false;
        surface.sync_unread();
        self.touch();
        Ok(())
    }

    #[cfg(test)]
    pub fn current_pane_id(&self) -> Option<PaneId> {
        self.selected_workspace()
            .map(|workspace| workspace.selected_pane_id)
    }

    pub fn current_surface_id(&self) -> Option<SurfaceId> {
        self.selected_workspace()
            .and_then(Workspace::selected_surface_id)
    }

    pub fn focus_pane(&mut self, pane_id: PaneId) -> Result<WorkspaceId, String> {
        let previous_workspace_id = self.selected_workspace_id;
        let previous_window_id = self.window_id;
        let previous_pane_id = self
            .selected_workspace()
            .map(|workspace| workspace.selected_pane_id);
        for workspace in &mut self.workspaces {
            if workspace.pane(pane_id).is_some() {
                let workspace_id = workspace.id;
                let window_id = workspace.window_id;
                workspace.focus_pane(pane_id)?;
                if let Some(window) = self.window_mut(window_id) {
                    if window.selected_workspace_id != workspace_id {
                        window.last_selected_workspace_id =
                            Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                        window.selected_workspace_id = workspace_id;
                    }
                }
                self.sync_global_selection_from_window(window_id);
                if previous_window_id != window_id
                    || previous_workspace_id != workspace_id
                    || previous_pane_id != Some(pane_id)
                {
                    self.touch();
                }
                return Ok(workspace_id);
            }
        }
        Err(format!("pane {pane_id} not found"))
    }

    pub fn focus_last_pane(&mut self) -> Result<(WorkspaceId, PaneId), String> {
        let workspace_id = self.selected_workspace_id;
        let previous_pane_id = self
            .workspace(workspace_id)
            .map(|workspace| workspace.selected_pane_id);
        let pane_id = {
            let workspace = self
                .workspace_mut(workspace_id)
                .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
            workspace.focus_last_pane()?
        };
        if previous_pane_id != Some(pane_id) {
            self.touch();
        }
        Ok((workspace_id, pane_id))
    }

    pub fn focus_adjacent_pane(
        &mut self,
        direction: FocusDirection,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let workspace_id = self.selected_workspace_id;
        let previous_pane_id = self
            .workspace(workspace_id)
            .map(|workspace| workspace.selected_pane_id);
        let pane_id = {
            let workspace = self
                .workspace_mut(workspace_id)
                .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
            workspace.focus_adjacent_pane(direction)?
        };
        if previous_pane_id != Some(pane_id) {
            self.touch();
        }
        Ok((workspace_id, pane_id))
    }

    pub fn focus_surface(
        &mut self,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let previous_workspace_id = self.selected_workspace_id;
        let previous_window_id = self.window_id;
        let previous_surface_id = self.current_surface_id();
        let workspace_index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.surface(surface_id).is_some())
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let (workspace_id, window_id, pane_id, current_directory) = {
            let workspace = self
                .workspaces
                .get_mut(workspace_index)
                .ok_or_else(|| format!("surface {surface_id} not found"))?;
            let workspace_id = workspace.id;
            let window_id = workspace.window_id;
            let pane_id = workspace.focus_surface(surface_id)?;
            let current_directory = workspace
                .surface(surface_id)
                .and_then(|surface| surface.current_directory.clone());
            if let Some(current_directory) = current_directory.clone() {
                workspace.current_directory = Some(current_directory);
            }
            (workspace_id, window_id, pane_id, current_directory)
        };
        if let Some(window) = self.window_mut(window_id) {
            if window.selected_workspace_id != workspace_id {
                window.last_selected_workspace_id =
                    Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                window.selected_workspace_id = workspace_id;
            }
        }
        self.sync_global_selection_from_window(window_id);
        if let Some(current_directory) = current_directory {
            if let Some(workspace) = self.workspace_mut(workspace_id) {
                workspace.current_directory = Some(current_directory);
            }
        }
        let notification_reads = self.mark_notifications_read(workspace_id, Some(surface_id));
        let unread_cleared = self.clear_surface_unread(surface_id);
        if notification_reads > 0 {
            let _ = self.increment_surface_flash(surface_id, notification_reads as u64);
        }
        if previous_window_id != window_id
            || previous_workspace_id != workspace_id
            || previous_surface_id != Some(surface_id)
            || notification_reads > 0
            || unread_cleared
        {
            self.touch();
        }
        Ok((workspace_id, pane_id))
    }

    pub fn split_selected_pane(
        &mut self,
        orientation: SplitOrientation,
        insert_first: bool,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        self.split_selected_pane_with_focus(orientation, insert_first, true)
    }

    pub fn split_selected_pane_with_focus(
        &mut self,
        orientation: SplitOrientation,
        insert_first: bool,
        focus_new: bool,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let workspace_id = self.selected_workspace_id;
        self.split_workspace_pane(workspace_id, None, orientation, insert_first, focus_new)
    }

    pub fn split_surface_with_focus(
        &mut self,
        surface_id: SurfaceId,
        orientation: SplitOrientation,
        insert_first: bool,
        focus_new: bool,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        self.split_workspace_pane(
            workspace_id,
            Some(pane_id),
            orientation,
            insert_first,
            focus_new,
        )
    }

    fn split_workspace_pane(
        &mut self,
        workspace_id: WorkspaceId,
        pane_id: Option<PaneId>,
        orientation: SplitOrientation,
        insert_first: bool,
        focus_new: bool,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let new_pane = self.new_terminal_pane();
        let new_pane_id = new_pane.id;
        let new_surface_id = new_pane.selected_surface_id;
        let previous_selected_workspace_id = self.selected_workspace_id;
        let previous_window_id = self.window_id;
        let target_pane_id = pane_id.unwrap_or_else(|| {
            self.workspace(workspace_id)
                .map(|workspace| workspace.selected_pane_id)
                .unwrap_or(Uuid::nil())
        });
        let pane_id = {
            let workspace = self
                .workspace_mut(workspace_id)
                .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
            workspace.split_pane(
                target_pane_id,
                orientation,
                insert_first,
                focus_new,
                new_pane,
            )?;
            if focus_new {
                workspace.selected_pane_id
            } else {
                new_pane_id
            }
        };
        let target_window_id = self
            .workspace_window_id(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if focus_new {
            if let Some(window) = self.window_mut(target_window_id) {
                if window.selected_workspace_id != workspace_id {
                    window.last_selected_workspace_id =
                        Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                    window.selected_workspace_id = workspace_id;
                }
            }
            if previous_window_id != target_window_id
                || previous_selected_workspace_id != workspace_id
            {
                self.sync_global_selection_from_window(target_window_id);
            }
        }
        self.touch();
        Ok((workspace_id, pane_id, new_surface_id))
    }

    pub fn create_surface_in_selected_pane(
        &mut self,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let workspace_id = self.selected_workspace_id;
        let pane_id = self
            .selected_workspace()
            .map(|workspace| workspace.selected_pane_id)
            .ok_or_else(|| "no selected workspace".to_string())?;
        self.create_surface_in_pane_with_focus(workspace_id, pane_id, true)
    }

    pub fn create_surface_in_pane_with_focus(
        &mut self,
        workspace_id: WorkspaceId,
        pane_id: PaneId,
        focus_new: bool,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let surface = self.new_terminal_surface();
        let surface_id = surface.id;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        workspace.create_surface_in_pane(pane_id, surface, focus_new)?;
        let window_id = self
            .workspace_window_id(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if focus_new {
            if let Some(window) = self.window_mut(window_id) {
                if window.selected_workspace_id != workspace_id {
                    window.last_selected_workspace_id =
                        Some(window.selected_workspace_id).filter(|id| *id != Uuid::nil());
                    window.selected_workspace_id = workspace_id;
                }
            }
            if self.window_id != window_id || self.selected_workspace_id != workspace_id {
                self.sync_global_selection_from_window(window_id);
            }
        }
        self.touch();
        Ok((workspace_id, pane_id, surface_id))
    }

    pub fn focus_relative_surface(
        &mut self,
        delta: isize,
    ) -> Result<(WorkspaceId, PaneId, SurfaceId), String> {
        let current_surface_id = self
            .current_surface_id()
            .ok_or_else(|| "no focused surface".to_string())?;
        let next_surface_id = self
            .selected_workspace()
            .ok_or_else(|| "no selected workspace".to_string())?
            .selected_surface_neighbor(delta)?;
        if next_surface_id == current_surface_id {
            let (workspace_id, pane_id) = self
                .locate_surface(next_surface_id)
                .ok_or_else(|| format!("surface {next_surface_id} not found"))?;
            return Ok((workspace_id, pane_id, next_surface_id));
        }

        let (workspace_id, pane_id) = self.focus_surface(next_surface_id)?;
        Ok((workspace_id, pane_id, next_surface_id))
    }

    pub fn close_selected_surface(&mut self) -> Result<(WorkspaceId, PaneId, bool), String> {
        let workspace_id = self.selected_workspace_id;
        let surface_id = self
            .current_surface_id()
            .ok_or_else(|| "no focused surface".to_string())?;
        self.close_surface(workspace_id, surface_id)
    }

    pub fn close_surface(
        &mut self,
        workspace_id: WorkspaceId,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId, bool), String> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let (closed_pane_id, removed_pane) = workspace.close_surface(surface_id)?;
        self.touch();
        Ok((workspace_id, closed_pane_id, removed_pane))
    }

    pub fn locate_surface(&self, surface_id: SurfaceId) -> Option<(WorkspaceId, PaneId)> {
        self.workspaces.iter().find_map(|workspace| {
            workspace
                .pane_id_for_surface(surface_id)
                .map(|pane_id| (workspace.id, pane_id))
        })
    }

    pub fn append_terminal_text(
        &mut self,
        surface_id: SurfaceId,
        text: &str,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let (workspace_id, pane_id, unread) = self.transcript_context(surface_id)?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let previous_unread = surface.unread;
        surface.transcript.push_str(text);
        surface.unread_activity = unread;
        surface.sync_unread();
        if surface.unread != previous_unread {
            self.touch();
        }
        Ok((workspace_id, pane_id))
    }

    pub fn update_surface_current_directory(
        &mut self,
        surface_id: SurfaceId,
        current_directory: Option<String>,
    ) -> Result<WorkspaceId, String> {
        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let is_selected_surface = workspace.selected_pane_id == pane_id
            && workspace
                .pane(pane_id)
                .map(|pane| pane.selected_surface_id == surface_id)
                .unwrap_or(false);
        let surface = workspace
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        if surface.current_directory == current_directory {
            return Ok(workspace_id);
        }

        surface.current_directory = current_directory.clone();
        if is_selected_surface {
            workspace.current_directory = current_directory;
        }
        self.touch();
        Ok(workspace_id)
    }

    pub fn update_surface_terminal_health(
        &mut self,
        surface_id: SurfaceId,
        terminal_health: TerminalHealth,
    ) -> Result<WorkspaceId, String> {
        let (workspace_id, _) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        if surface.terminal_health.realized == terminal_health.realized
            && surface.terminal_health.startup_error == terminal_health.startup_error
            && surface.terminal_health.io_thread_main_started
                == terminal_health.io_thread_main_started
            && surface.terminal_health.io_thread_entered == terminal_health.io_thread_entered
            && surface.terminal_health.subprocess_start_attempted
                == terminal_health.subprocess_start_attempted
            && surface.terminal_health.child_pid == terminal_health.child_pid
            && surface.terminal_health.child_exited == terminal_health.child_exited
            && surface.terminal_health.child_exit_code == terminal_health.child_exit_code
            && surface.terminal_health.child_runtime_ms == terminal_health.child_runtime_ms
        {
            return Ok(workspace_id);
        }

        surface.terminal_health = terminal_health;
        self.touch();
        Ok(workspace_id)
    }

    pub fn replace_terminal_text(
        &mut self,
        surface_id: SurfaceId,
        text: String,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let (workspace_id, pane_id, unread) = self.transcript_context(surface_id)?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let previous_unread = surface.unread;
        surface.transcript = text;
        surface.unread_activity = unread;
        surface.sync_unread();
        if surface.unread != previous_unread {
            self.touch();
        }
        Ok((workspace_id, pane_id))
    }

    fn transcript_context(
        &self,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId, bool), String> {
        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let unread = {
            let workspace = self
                .workspace(workspace_id)
                .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
            let pane_is_selected = workspace.selected_pane_id == pane_id;
            let surface_is_selected = workspace
                .pane(pane_id)
                .map(|pane| pane.selected_surface_id == surface_id)
                .unwrap_or(false);
            self.selected_workspace_id != workspace_id || !pane_is_selected || !surface_is_selected
        };
        Ok((workspace_id, pane_id, unread))
    }

    pub fn read_terminal_text(
        &self,
        surface_id: SurfaceId,
        line_limit: Option<usize>,
    ) -> Result<(WorkspaceId, String), String> {
        let (workspace_id, _) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;

        let text = if let Some(line_limit) = line_limit {
            let mut lines: Vec<&str> = surface.transcript.lines().collect();
            if lines.len() > line_limit {
                lines = lines.split_off(lines.len() - line_limit);
            }
            lines.join("\n")
        } else {
            surface.transcript.clone()
        };

        Ok((workspace_id, text))
    }

    pub fn trigger_flash(
        &mut self,
        surface_id: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId, u64), String> {
        let (workspace_id, pane_id) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let flash_count = {
            let workspace = self
                .workspace_mut(workspace_id)
                .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
            let surface = workspace
                .surface_mut(surface_id)
                .ok_or_else(|| format!("surface {surface_id} not found"))?;
            surface.flash_count = surface.flash_count.saturating_add(1);
            surface.unread_notification = true;
            surface.sync_unread();
            surface.flash_count
        };
        self.touch();
        Ok((workspace_id, pane_id, flash_count))
    }

    pub fn create_notification(
        &mut self,
        workspace_id: WorkspaceId,
        surface_id: Option<SurfaceId>,
        title: String,
        subtitle: String,
        body: String,
        delivered: bool,
    ) -> Result<NotificationId, String> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        if let Some(surface_id) = surface_id {
            let surface = workspace
                .surface_mut(surface_id)
                .ok_or_else(|| format!("surface {surface_id} not found"))?;
            surface.unread_notification = true;
            surface.sync_unread();
        }

        let notification_id = Uuid::new_v4();
        self.notifications.push(Notification {
            id: notification_id,
            workspace_id,
            surface_id,
            is_read: false,
            title,
            subtitle,
            body,
            delivered,
        });
        self.touch();
        Ok(notification_id)
    }

    pub fn clear_notifications(&mut self) {
        for workspace in &mut self.workspaces {
            for pane in &mut workspace.panes {
                for surface in &mut pane.surfaces {
                    surface.unread_notification = false;
                    surface.sync_unread();
                }
            }
        }
        self.notifications.clear();
        self.touch();
    }

    pub fn flash_count(&self, surface_id: SurfaceId) -> Result<u64, String> {
        let (workspace_id, _) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        Ok(surface.flash_count)
    }

    pub fn reset_flash_counts(&mut self) {
        let mut changed = false;
        for workspace in &mut self.workspaces {
            for pane in &mut workspace.panes {
                for surface in &mut pane.surfaces {
                    if surface.flash_count != 0 {
                        surface.flash_count = 0;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.touch();
        }
    }

    fn mark_notifications_read(
        &mut self,
        workspace_id: WorkspaceId,
        surface_id: Option<SurfaceId>,
    ) -> usize {
        let mut changed = 0;
        for notification in &mut self.notifications {
            if notification.workspace_id != workspace_id || notification.is_read {
                continue;
            }
            if notification.surface_id.is_none() || notification.surface_id == surface_id {
                notification.is_read = true;
                changed += 1;
            }
        }
        changed
    }

    fn clear_surface_unread(&mut self, surface_id: SurfaceId) -> bool {
        let Some((workspace_id, _)) = self.locate_surface(surface_id) else {
            return false;
        };
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return false;
        };
        let Some(surface) = workspace.surface_mut(surface_id) else {
            return false;
        };
        let previous_unread = surface.unread;
        surface.unread_activity = false;
        surface.unread_notification = false;
        surface.sync_unread();
        previous_unread != surface.unread
    }

    fn increment_surface_flash(
        &mut self,
        surface_id: SurfaceId,
        amount: u64,
    ) -> Result<u64, String> {
        let (workspace_id, _) = self
            .locate_surface(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or_else(|| format!("workspace {workspace_id} not found"))?;
        let surface = workspace
            .surface_mut(surface_id)
            .ok_or_else(|| format!("surface {surface_id} not found"))?;
        surface.flash_count = surface.flash_count.saturating_add(amount.max(1));
        Ok(surface.flash_count)
    }

    pub fn to_persistent_snapshot(&self) -> PersistentStateSnapshot {
        PersistentStateSnapshot {
            version: 2,
            windows: self.windows.clone(),
            window_id: self.window_id,
            workspaces: self.workspaces.clone(),
            selected_workspace_id: self.selected_workspace_id,
            last_selected_workspace_id: self.last_selected_workspace_id,
            notifications: self.notifications.clone(),
            next_workspace_title_number: self.next_workspace_title_number,
            next_surface_title_number: self.next_surface_title_number,
        }
    }

    pub fn from_persistent_snapshot(snapshot: PersistentStateSnapshot) -> Result<Self, String> {
        if snapshot.version != 1 && snapshot.version != 2 {
            return Err(format!(
                "unsupported session snapshot version {}",
                snapshot.version
            ));
        }
        if snapshot.workspaces.is_empty() {
            return Err("session snapshot has no workspaces".to_string());
        }

        let mut state = Self {
            windows: snapshot.windows,
            window_id: if snapshot.window_id.is_nil() {
                Uuid::new_v4()
            } else {
                snapshot.window_id
            },
            workspaces: snapshot.workspaces,
            selected_workspace_id: snapshot.selected_workspace_id,
            last_selected_workspace_id: snapshot.last_selected_workspace_id,
            notifications: snapshot.notifications,
            revision: 0,
            next_workspace_title_number: snapshot.next_workspace_title_number.max(1),
            next_surface_title_number: snapshot.next_surface_title_number.max(1),
        };

        if state.windows.is_empty() {
            let fallback_window_id = state
                .workspaces
                .first()
                .map(|workspace| {
                    if workspace.window_id.is_nil() {
                        Uuid::new_v4()
                    } else {
                        workspace.window_id
                    }
                })
                .unwrap_or_else(Uuid::new_v4);
            for workspace in &mut state.workspaces {
                if workspace.window_id.is_nil() {
                    workspace.window_id = fallback_window_id;
                }
            }
            state.windows.push(WindowState {
                id: fallback_window_id,
                selected_workspace_id: snapshot.selected_workspace_id,
                last_selected_workspace_id: snapshot.last_selected_workspace_id,
            });
        }

        for workspace in &mut state.workspaces {
            if workspace.window_id.is_nil() {
                workspace.window_id = state.window_id;
            }
            if workspace.panes.is_empty() {
                return Err(format!("workspace {} has no panes", workspace.id));
            }

            if !workspace
                .panes
                .iter()
                .any(|pane| pane.id == workspace.selected_pane_id)
            {
                workspace.selected_pane_id = workspace.panes[0].id;
            }
            workspace.last_selected_pane_id = workspace
                .last_selected_pane_id
                .filter(|candidate| workspace.pane(*candidate).is_some());

            for pane in &mut workspace.panes {
                if pane.surfaces.is_empty() {
                    return Err(format!("pane {} has no surfaces", pane.id));
                }
                if !pane
                    .surfaces
                    .iter()
                    .any(|surface| surface.id == pane.selected_surface_id)
                {
                    pane.selected_surface_id = pane.surfaces[0].id;
                }
                for surface in &mut pane.surfaces {
                    if surface.unread && !surface.unread_activity && !surface.unread_notification {
                        surface.unread_activity = true;
                    }
                    surface.sync_unread();
                }
            }
        }

        if !state
            .workspaces
            .iter()
            .any(|workspace| workspace.id == state.selected_workspace_id)
        {
            state.selected_workspace_id = state
                .windows
                .iter()
                .find_map(|window| {
                    state
                        .workspaces_in_window(window.id)
                        .into_iter()
                        .map(|workspace| workspace.id)
                        .next()
                })
                .unwrap_or(state.workspaces[0].id);
        }
        state.last_selected_workspace_id = state
            .last_selected_workspace_id
            .filter(|candidate| state.workspace(*candidate).is_some());
        let valid_window_ids = state
            .workspaces
            .iter()
            .map(|workspace| workspace.window_id)
            .collect::<HashSet<_>>();
        state
            .windows
            .retain(|window| valid_window_ids.contains(&window.id));
        for window_id in valid_window_ids {
            if state.window(window_id).is_none() {
                let selected_workspace_id = state
                    .workspaces_in_window(window_id)
                    .into_iter()
                    .map(|workspace| workspace.id)
                    .next()
                    .unwrap_or(Uuid::nil());
                state.windows.push(WindowState {
                    id: window_id,
                    selected_workspace_id,
                    last_selected_workspace_id: None,
                });
            }
            state.repair_window_selection(window_id);
        }
        if !state
            .windows
            .iter()
            .any(|window| window.id == state.window_id)
        {
            state.window_id = state.windows[0].id;
        }
        state.sync_global_selection_from_window(state.window_id);
        let valid_workspace_ids = state
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<HashSet<_>>();
        let valid_surface_ids = state
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panes.iter())
            .flat_map(|pane| pane.surfaces.iter())
            .map(|surface| surface.id)
            .collect::<HashSet<_>>();
        state.notifications.retain(|notification| {
            valid_workspace_ids.contains(&notification.workspace_id)
                && notification
                    .surface_id
                    .map(|surface_id| valid_surface_ids.contains(&surface_id))
                    .unwrap_or(true)
        });

        if snapshot.next_workspace_title_number == 0 {
            state.next_workspace_title_number = state.workspaces.len() + 1;
        }
        if snapshot.next_surface_title_number == 0 {
            state.next_surface_title_number = state
                .workspaces
                .iter()
                .map(Workspace::surface_count)
                .sum::<usize>()
                + 1;
        }

        Ok(state)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentStateSnapshot {
    pub version: u32,
    #[serde(default)]
    pub windows: Vec<WindowState>,
    pub window_id: WindowId,
    pub workspaces: Vec<Workspace>,
    pub selected_workspace_id: WorkspaceId,
    pub last_selected_workspace_id: Option<WorkspaceId>,
    pub notifications: Vec<Notification>,
    pub next_workspace_title_number: usize,
    pub next_surface_title_number: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_ok<T>(value: Result<T, String>) -> T {
        value.unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn initial_state_has_one_workspace_one_pane_and_one_surface() {
        let state = AppState::new();
        let workspace = state.selected_workspace().expect("selected workspace");

        assert_eq!(state.workspaces.len(), 1);
        assert_eq!(workspace.pane_count(), 1);
        assert_eq!(workspace.surface_count(), 1);
        assert_eq!(workspace.title, "Workspace 1");
    }

    #[test]
    fn creating_workspace_selects_it() {
        let mut state = AppState::new();
        let first_workspace_id = state.selected_workspace_id;
        let workspace_id = state.create_workspace();

        assert_eq!(state.selected_workspace_id, workspace_id);
        assert_eq!(state.workspaces.len(), 2);
        assert_eq!(state.last_selected_workspace_id, Some(first_workspace_id));
    }

    #[test]
    fn selecting_workspace_changes_focus() {
        let mut state = AppState::new();
        let first_workspace_id = state.selected_workspace_id;
        let second_workspace_id = state.create_workspace();

        expect_ok(state.select_workspace(first_workspace_id));

        assert_eq!(state.selected_workspace_id, first_workspace_id);
        assert_eq!(state.last_selected_workspace_id, Some(second_workspace_id));
    }

    #[test]
    fn splitting_selected_pane_creates_new_pane_and_focuses_it() {
        let mut state = AppState::new();
        let workspace_id = state.selected_workspace_id;

        let (_, new_pane_id, new_surface_id) =
            expect_ok(state.split_selected_pane(SplitOrientation::Horizontal, false));

        let workspace = state.workspace(workspace_id).expect("workspace");
        assert_eq!(workspace.pane_count(), 2);
        assert_eq!(workspace.selected_pane_id, new_pane_id);
        assert_eq!(
            workspace
                .selected_pane()
                .map(|pane| pane.selected_surface_id),
            Some(new_surface_id)
        );
    }

    #[test]
    fn creating_surface_adds_tab_to_selected_pane_and_selects_it() {
        let mut state = AppState::new();
        let workspace_id = state.selected_workspace_id;
        let pane_id = state.current_pane_id().expect("pane");

        let (_, _, surface_id) = expect_ok(state.create_surface_in_selected_pane());

        let workspace = state.workspace(workspace_id).expect("workspace");
        let pane = workspace.pane(pane_id).expect("pane");
        assert_eq!(pane.surfaces.len(), 2);
        assert_eq!(pane.selected_surface_id, surface_id);
    }

    #[test]
    fn closing_last_surface_in_extra_pane_collapses_layout() {
        let mut state = AppState::new();
        let workspace_id = state.selected_workspace_id;
        let (_, pane_id, surface_id) =
            expect_ok(state.split_selected_pane(SplitOrientation::Vertical, false));

        let (_, closed_pane_id, removed_pane) =
            expect_ok(state.close_surface(workspace_id, surface_id));

        let workspace = state.workspace(workspace_id).expect("workspace");
        assert!(removed_pane);
        assert_eq!(closed_pane_id, pane_id);
        assert_eq!(workspace.pane_count(), 1);
    }

    #[test]
    fn terminal_text_round_trip_reads_last_lines() {
        let mut state = AppState::new();
        let surface_id = state.current_surface_id().expect("surface");

        expect_ok(state.append_terminal_text(surface_id, "alpha\nbeta\ngamma\n"));
        let (_, text) = expect_ok(state.read_terminal_text(surface_id, Some(2)));

        assert_eq!(text, "beta\ngamma");
    }
}
