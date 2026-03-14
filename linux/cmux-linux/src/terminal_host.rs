use crate::model::SharedModel;
use crate::state::{AppState, SurfaceId, TerminalHealth};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use vte4::prelude::*;

#[derive(Debug, Clone)]
pub struct TerminalBridge {
    sender: Sender<TerminalCommand>,
}

impl TerminalBridge {
    pub(crate) fn new() -> (Self, Receiver<TerminalCommand>) {
        let (sender, receiver) = mpsc::channel();
        (Self { sender }, receiver)
    }

    pub(crate) fn send_text(&self, surface_id: SurfaceId, text: String) -> Result<(), String> {
        self.sender
            .send(TerminalCommand::SendText { surface_id, text })
            .map_err(|error| format!("terminal runtime unavailable: {error}"))
    }
}

#[derive(Debug)]
pub(crate) enum TerminalCommand {
    SendText { surface_id: SurfaceId, text: String },
}

#[derive(Debug, Clone)]
pub struct TerminalBackendStatus {
    pub active: &'static str,
    pub requested: Option<String>,
    pub supported: &'static [&'static str],
    pub note: Option<String>,
}

pub fn terminal_backend_status() -> TerminalBackendStatus {
    TerminalBackendStatus {
        active: "vte",
        requested: None,
        supported: &["vte"],
        note: None,
    }
}

pub struct TerminalRuntime {
    backend_status: TerminalBackendStatus,
    model: SharedModel,
    receiver: Receiver<TerminalCommand>,
    sessions: HashMap<SurfaceId, TerminalSession>,
}

impl TerminalRuntime {
    pub(crate) fn new(model: SharedModel, receiver: Receiver<TerminalCommand>) -> Self {
        Self {
            backend_status: terminal_backend_status(),
            model,
            receiver,
            sessions: HashMap::new(),
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend_status.active
    }

    pub fn backend_status(&self) -> &TerminalBackendStatus {
        &self.backend_status
    }

    pub fn reconcile(&mut self, snapshot: &AppState) {
        let live_surfaces = snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.panes.iter())
            .flat_map(|pane| pane.surfaces.iter())
            .map(|surface| surface.id)
            .collect::<HashSet<_>>();

        self.sessions.retain(|surface_id, session| {
            if live_surfaces.contains(surface_id) {
                true
            } else {
                session.request_shutdown();
                false
            }
        });
    }

    pub fn pump_commands(&mut self) {
        while let Ok(command) = self.receiver.try_recv() {
            match command {
                TerminalCommand::SendText { surface_id, text } => self.send_text(surface_id, &text),
            }
        }
    }

    pub fn widget_for_surface(&mut self, surface_id: SurfaceId) -> gtk::Widget {
        self.ensure_session(surface_id)
            .map(|session| session.widget.clone())
            .unwrap_or_else(|| {
                placeholder_widget(
                    "Missing terminal session",
                    "The requested Linux terminal surface could not be created.",
                )
            })
    }

    pub fn focus_surface(&mut self, surface_id: SurfaceId) {
        let Some(session) = self.ensure_session(surface_id) else {
            return;
        };

        if session.session.terminal.parent().is_some()
            && session.session.terminal.root().is_some()
            && session.session.terminal.is_visible()
        {
            session.session.terminal.grab_focus();
        }
    }

    fn send_text(&mut self, surface_id: SurfaceId, text: &str) {
        let Some(session) = self.ensure_session(surface_id) else {
            return;
        };
        session.session.write_input(text);
    }

    fn ensure_session(&mut self, surface_id: SurfaceId) -> Option<&TerminalSession> {
        if self.sessions.contains_key(&surface_id) {
            return self.sessions.get(&surface_id);
        }
        if !surface_exists(&self.model, surface_id) {
            return None;
        }

        let session = TerminalSession::vte(&self.model, surface_id);
        self.sessions.insert(surface_id, session);
        self.sessions.get(&surface_id)
    }
}

struct TerminalSession {
    widget: gtk::Widget,
    session: VteSession,
}

impl TerminalSession {
    fn vte(model: &SharedModel, surface_id: SurfaceId) -> Self {
        let session = vte_session(model, surface_id);
        Self {
            widget: session.terminal.clone().upcast::<gtk::Widget>(),
            session,
        }
    }

    fn request_shutdown(&self) {
        self.session.request_shutdown();
    }
}

#[derive(Clone)]
struct VteSession {
    terminal: vte4::Terminal,
    ready: Rc<Cell<bool>>,
    pending_input: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl VteSession {
    fn write_input(&self, text: &str) {
        let bytes = normalize_terminal_input(text);
        if self.ready.get() {
            let _ = write_terminal_input(&self.terminal, &bytes);
            return;
        }
        self.pending_input.borrow_mut().push(bytes);
    }

    fn request_shutdown(&self) {
        self.write_input("exit\n");
    }
}

fn vte_session(model: &SharedModel, surface_id: SurfaceId) -> VteSession {
    let terminal = vte4::Terminal::new();
    terminal.set_hexpand(true);
    terminal.set_vexpand(true);
    terminal.set_scrollback_lines(20_000);
    terminal.set_mouse_autohide(true);
    terminal.set_allow_hyperlink(true);
    seed_restored_transcript(model, surface_id, &terminal);

    let _ = model.lock().state.update_surface_terminal_health(
        surface_id,
        TerminalHealth {
            realized: true,
            ..TerminalHealth::default()
        },
    );

    let ready = Rc::new(Cell::new(false));
    let child_pid = Rc::new(Cell::new(None));
    let pending_input = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    let session = VteSession {
        terminal: terminal.clone(),
        ready: ready.clone(),
        pending_input: pending_input.clone(),
    };

    {
        let model = model.clone();
        let child_pid = child_pid.clone();
        terminal.connect_contents_changed(move |terminal| {
            if let Some(text) = capture_terminal_text(terminal) {
                let _ = model.lock().state.replace_terminal_text(surface_id, text);
            }
            sync_surface_current_directory(
                &model,
                surface_id,
                child_pid.get().and_then(resolve_child_current_directory),
            );
        });
    }

    {
        let model = model.clone();
        terminal.connect_current_directory_uri_changed(move |terminal| {
            let current_directory = terminal.current_directory_uri().and_then(|uri| {
                gtk::gio::File::for_uri(uri.as_str())
                    .path()
                    .and_then(|path| path.to_str().map(ToOwned::to_owned))
            });
            sync_surface_current_directory(&model, surface_id, current_directory);
        });
    }

    {
        let model = model.clone();
        let child_pid = child_pid.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
            if !surface_exists(&model, surface_id) {
                return gtk::glib::ControlFlow::Break;
            }
            sync_surface_current_directory(
                &model,
                surface_id,
                child_pid.get().and_then(resolve_child_current_directory),
            );
            gtk::glib::ControlFlow::Continue
        });
    }

    {
        let model = model.clone();
        let focus_controller = gtk::EventControllerFocus::new();
        focus_controller.connect_enter(move |_| {
            let _ = model.lock().state.focus_surface(surface_id);
        });
        terminal.add_controller(focus_controller);
    }

    {
        let terminal_ref = terminal.clone();
        let key_controller =
            gtk::EventControllerKey::new();
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        key_controller.connect_key_pressed(move |_, keyval, _, modifiers| {
            let ctrl_shift = gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::SHIFT_MASK;
            if modifiers & ctrl_shift == ctrl_shift {
                if keyval == gtk::gdk::Key::C {
                    terminal_ref.copy_clipboard_format(vte4::Format::Text);
                    return gtk::glib::Propagation::Stop;
                }
                if keyval == gtk::gdk::Key::V {
                    terminal_ref.paste_clipboard();
                    return gtk::glib::Propagation::Stop;
                }
            }
            gtk::glib::Propagation::Proceed
        });
        terminal.add_controller(key_controller);
    }

    {
        let model = model.clone();
        let child_pid = child_pid.clone();
        terminal.connect_child_exited(move |_, status| {
            let _ = model.lock().state.update_surface_terminal_health(
                surface_id,
                TerminalHealth {
                    realized: true,
                    subprocess_start_attempted: true,
                    child_pid: child_pid.get().map(|pid| pid.0),
                    child_exited: true,
                    child_exit_code: (status >= 0).then_some(status as u32),
                    ..TerminalHealth::default()
                },
            );
        });
    }

    spawn_shell(
        &terminal,
        &ready,
        &child_pid,
        &pending_input,
        model.clone(),
        surface_id,
    );

    session
}

fn spawn_shell(
    terminal: &vte4::Terminal,
    ready: &Rc<Cell<bool>>,
    child_pid: &Rc<Cell<Option<gtk::glib::Pid>>>,
    pending_input: &Rc<RefCell<Vec<Vec<u8>>>>,
    model: SharedModel,
    surface_id: SurfaceId,
) {
    let shell_argv = login_shell_argv();
    let argv = shell_argv.iter().map(String::as_str).collect::<Vec<_>>();
    let cwd = resolve_surface_working_directory(&model, surface_id).or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(ToOwned::to_owned))
    });
    let ready_flag = ready.clone();
    let child_pid_state = child_pid.clone();
    let queue = pending_input.clone();
    let callback_terminal = terminal.clone();

    let _ = model.lock().state.update_surface_terminal_health(
        surface_id,
        TerminalHealth {
            realized: true,
            subprocess_start_attempted: true,
            ..TerminalHealth::default()
        },
    );

    terminal.spawn_async(
        vte4::PtyFlags::DEFAULT,
        cwd.as_deref(),
        &argv,
        &[],
        gtk::glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(pid) => {
                ready_flag.set(true);
                child_pid_state.set(Some(pid));
                for chunk in queue.borrow_mut().drain(..) {
                    let _ = write_terminal_input(&callback_terminal, &chunk);
                }
                let mut guard = model.lock();
                let _ = guard.state.update_surface_terminal_health(
                    surface_id,
                    TerminalHealth {
                        realized: true,
                        subprocess_start_attempted: true,
                        child_pid: Some(pid.0),
                        ..TerminalHealth::default()
                    },
                );
                if let Some(text) = capture_terminal_text(&callback_terminal) {
                    let _ = guard.state.replace_terminal_text(surface_id, text);
                }
            }
            Err(error) => {
                let mut guard = model.lock();
                let message =
                    format!("[cmux linux] failed to spawn terminal process: {error}\n");
                let _ = guard.state.append_terminal_text(surface_id, &message);
                let _ = guard.state.update_surface_terminal_health(
                    surface_id,
                    TerminalHealth {
                        realized: true,
                        subprocess_start_attempted: true,
                        startup_error: Some(format!(
                            "failed to spawn terminal process: {error}"
                        )),
                        ..TerminalHealth::default()
                    },
                );
            }
        },
    );
}

fn capture_terminal_text(terminal: &vte4::Terminal) -> Option<String> {
    use gtk::gio::prelude::*;

    let stream = gtk::gio::MemoryOutputStream::new_resizable();
    terminal
        .write_contents_sync(
            &stream,
            vte4::WriteFlags::Default,
            None::<&gtk::gio::Cancellable>,
        )
        .ok()?;
    stream.close(None::<&gtk::gio::Cancellable>).ok()?;
    let bytes = stream.steal_as_bytes();
    Some(String::from_utf8_lossy(bytes.as_ref()).into_owned())
}

fn surface_exists(model: &SharedModel, surface_id: SurfaceId) -> bool {
    model.lock().state.locate_surface(surface_id).is_some()
}

fn resolve_surface_working_directory(model: &SharedModel, surface_id: SurfaceId) -> Option<String> {
    let guard = model.lock();
    let (workspace_id, _) = guard.state.locate_surface(surface_id)?;
    let workspace = guard.state.workspace(workspace_id)?;
    workspace.current_directory.clone()
}

fn resolve_child_current_directory(child_pid: gtk::glib::Pid) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{}/cwd", child_pid.0)).ok()?;
    path.to_str().map(ToOwned::to_owned)
}

fn sync_surface_current_directory(
    model: &SharedModel,
    surface_id: SurfaceId,
    current_directory: Option<String>,
) {
    let _ = model.lock().state.update_surface_current_directory(surface_id, current_directory);
}

fn write_terminal_input(terminal: &vte4::Terminal, bytes: &[u8]) -> Result<(), String> {
    let pty = terminal
        .pty()
        .ok_or_else(|| "terminal pty unavailable".to_string())?;
    let fd = pty.fd().as_raw_fd();
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        let written = unsafe {
            libc::write(
                fd,
                remaining.as_ptr().cast::<libc::c_void>(),
                remaining.len(),
            )
        };
        if written < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("pty write failed: {error}"));
        }
        offset += written as usize;
    }
    Ok(())
}

fn normalize_terminal_input(text: &str) -> Vec<u8> {
    text.bytes()
        .map(|byte| if byte == b'\n' { b'\r' } else { byte })
        .collect()
}

fn seed_restored_transcript(model: &SharedModel, surface_id: SurfaceId, terminal: &vte4::Terminal) {
    let transcript = {
        let guard = model.lock();
        guard.state.locate_surface(surface_id)
            .and_then(|(workspace_id, _)| {
                let workspace = guard.state.workspace(workspace_id)?;
                let surface = workspace.surface(surface_id)?;
                Some(surface.transcript.clone())
            })
            .unwrap_or_default()
    };

    if !transcript.is_empty() {
        terminal.feed(transcript.as_bytes());
    }
}

fn login_shell_argv() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut argv = vec![shell.clone()];
    let shell_name = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    if matches!(shell_name, "bash" | "fish" | "ksh" | "sh" | "tcsh" | "zsh") {
        argv.push("-l".to_string());
    }

    argv
}

fn placeholder_widget(title: &str, body: &str) -> gtk::Widget {
    let title_label = gtk::Label::new(Some(title));
    title_label.add_css_class("title-2");
    title_label.set_xalign(0.0);

    let body_label = gtk::Label::new(Some(body));
    body_label.set_wrap(true);
    body_label.set_xalign(0.0);
    body_label.add_css_class("dim-label");

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.append(&title_label);
    content.append(&body_label);

    let frame = gtk::Frame::new(None);
    frame.set_hexpand(true);
    frame.set_vexpand(true);
    frame.set_child(Some(&content));
    frame.upcast::<gtk::Widget>()
}
