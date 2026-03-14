# Linux / Wayland Port Notes

This branch starts the dual-platform split for `cmux`:

- macOS stays on Swift/AppKit.
- Linux gets a separate GTK4 + libadwaita frontend in `linux/cmux-linux/`.
- The shared contract is the existing CLI + v2 socket API, not a shared runtime library.

## Linux v1 scope

Included target surface:

- terminal workspaces
- pane splits
- per-pane surface tabs
- session restore
- socket / CLI control
- unread / flash state
- Linux desktop notifications

Explicitly deferred from Linux v1:

- embedded browser
- Sparkle / updater UI
- AppleScript
- macOS-only menu bar and titlebar integrations
- packaging beyond source-build instructions

## Manjaro source-build prerequisites

The GTK frontend scaffold in this branch assumes these Arch/Manjaro packages exist:

```bash
sudo pacman -S gtk4 libadwaita vte4 pkgconf rustup clang
rustup default stable
```

Why these:

- `gtk4` and `libadwaita` provide the Wayland-native UI stack.
- `vte4` provides the GTK4 terminal widget used for the supported Linux terminal backend.
- `pkgconf` provides `pkg-config`, which the Rust GTK bindings use at build time.
- `rustup` provides the Rust toolchain.

## Current bootstrap status

`./scripts/setup.sh` still follows the macOS-specific setup path. Linux source builds do not need those extra steps; for Linux the required path is just the system packages above plus the Rust toolchain.

That means:

- repo submodules are now populated
- the Linux GTK workspace can be developed independently
- the Linux branch now treats VTE as the single supported terminal backend

## Running the Linux frontend

Check readiness first:

```bash
python3 scripts/linux_wayland_doctor.py
./scripts/run-linux.sh --doctor
```

From the repo root:

```bash
./scripts/run-linux.sh
```

The Linux app uses VTE as its terminal backend:

- interactive workspace sidebar
- `New Workspace` / `Close Workspace`
- `Next Workspace` / `Previous Workspace` / `Last Workspace`
- `workspace.create` honors `cwd` for new terminal workspaces
- `Split Right` / `Split Down`
- `New Surface` tabs inside the selected pane
- `Next Surface` / `Previous Surface`
- directional pane focus plus `Last Pane`
- GTK-rendered split layout driven by the Linux state model
- a live v2 Unix socket server at `/tmp/cmux-linux.sock` by default
- socket-driven workspace/pane/surface mutations reflected in the running UI
- real VTE-backed shell sessions per terminal surface
- `system.capabilities` and `system.identify` both report the active Linux terminal backend as `vte`
- socket `surface.send_text` / `surface.send_key` routed into the live terminal widget
- `surface.send_key` now covers control keys, arrows, home/end, page keys, insert/delete, shift-tab, function keys, and simple `alt-`/`meta-` plus `ctrl-<key>` terminal sequences
- socket `surface.read_text` backed by terminal readback snapshots
- selected workspace cwd now follows shell `cd` changes on Linux terminals
- surface and workspace focus now mark related notifications read and increment flash counts
- `notification.clear` clears notification-driven unread state without clobbering transcript-driven unread state
- Linux now supports the existing client focus-control hooks for notification suppression and simulated app activation
- automatic session restore for workspaces, panes, surfaces, and transcript snapshots

## Socket control on Linux

The Linux app now starts the same style of line-oriented v2 JSON socket the macOS app uses.

Default socket path:

```bash
/tmp/cmux-linux.sock
```

Override it with either environment variable:

```bash
CMUX_SOCKET=/tmp/my-cmux.sock ./scripts/run-linux.sh
CMUX_SOCKET_PATH=/tmp/my-cmux.sock ./scripts/run-linux.sh
```

Default session file path:

```bash
$XDG_STATE_HOME/cmux/cmux-linux-session.json
```

Override it when needed:

```bash
CMUX_LINUX_SESSION_FILE=/tmp/cmux-linux-session.json ./scripts/run-linux.sh
```

Quick smoke commands against a running Linux app:

```bash
CMUX_SOCKET=/tmp/cmux-linux.sock python3 tests_v2/cmux.py --method system.ping
```

```bash
CMUX_SOCKET=/tmp/cmux-linux.sock python3 tests_v2/cmux.py --method system.capabilities
```

```bash
CMUX_SOCKET=/tmp/cmux-linux.sock python3 tests_v2/cmux.py --method workspace.list
```

Create a workspace rooted in a specific directory:

```bash
CMUX_SOCKET=/tmp/cmux-linux.sock python3 tests_v2/cmux.py \
  --method workspace.create \
  --params '{"cwd":"/tmp"}'
```

Verify that Linux terminal input and cwd tracking are live:

```bash
python3 - <<'PY'
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.getcwd(), "tests_v2"))
from cmux import cmux

sock = os.environ.get("CMUX_SOCKET", "/tmp/cmux-linux.sock")

with cmux(sock) as c:
    created = c._call("workspace.create", {"cwd": "/tmp"})
    c._call("workspace.select", {"workspace_id": created["workspace_id"]})
    surface = c._call("surface.current", {})
    c._call("surface.send_text", {"surface_id": surface["surface_id"], "text": "pwd\\n"})
    time.sleep(0.5)
    c._call("surface.send_text", {"surface_id": surface["surface_id"], "text": "cd /\\n"})
    time.sleep(1.0)
    print(json.dumps(c._call("workspace.current", {}), indent=2, sort_keys=True))
    print(c._call("surface.read_text", {"surface_id": surface["surface_id"]})["text"][-200:])
PY
```

Manual JSON example:

```bash
python3 - <<'PY'
import json
import socket

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("/tmp/cmux-linux.sock")
sock.sendall(b'{"id":1,"method":"surface.send_text","params":{"text":"echo hi\\\\n"}}\n')
print(sock.recv(8192).decode().strip())
sock.sendall(b'{"id":2,"method":"surface.read_text","params":{}}\n')
print(sock.recv(8192).decode().strip())
sock.close()
PY
```

Currently implemented Linux socket methods:

- `system.ping`
- `system.capabilities`
- `system.identify`
- `window.list`
- `window.current`
- `workspace.list`
- `workspace.create`
- `workspace.rename`
- `workspace.reorder`
- `workspace.select`
- `workspace.current`
- `workspace.next`
- `workspace.previous`
- `workspace.last`
- `workspace.close`
- `pane.list`
- `pane.surfaces`
- `pane.focus`
- `pane.create`
- `pane.break`
- `pane.join`
- `pane.last`
- `pane.swap`
- `surface.list`
- `surface.current`
- `surface.focus`
- `surface.split`
- `surface.create`
- `surface.close`
- `surface.clear_history`
- `surface.drag_to_split`
- `surface.send_text`
- `surface.send_key`
- `surface.read_text`
- `surface.health`
- `surface.move`
- `surface.refresh`
- `surface.reorder`
- `surface.trigger_flash`
- `notification.create`
- `notification.create_for_surface`
- `notification.list`
- `notification.clear`

Useful Linux payload note:

- `surface.list` now includes `unread` and `flash_count` for each surface so notification/focus transitions are directly observable over the socket.

Linux-specific test/control methods now implemented:

- `app.focus_override.set`
- `app.simulate_active`
- `debug.app.activate`
- `debug.bonsplit_underflow.count`
- `debug.bonsplit_underflow.reset`
- `debug.command_palette.rename_input.select_all`
- `debug.command_palette.rename_tab.open`
- `debug.empty_panel.count`
- `debug.empty_panel.reset`
- `debug.layout`
- `debug.notification.focus`
- `debug.panel_snapshot`
- `debug.panel_snapshot.reset`
- `debug.portal.stats`
- `debug.shortcut.set`
- `debug.shortcut.simulate`
- `debug.sidebar.visible`
- `debug.terminal.is_focused`
- `debug.terminal.read_text`
- `debug.terminal.render_stats`
- `debug.type`
- `debug.window.screenshot`
- `debug.flash.count`
- `debug.flash.reset`

Unsupported families still return structured `not_supported` errors:

- `browser.*`

Linux-specific behavior implemented in this slice:

- UUID handles plus stable `window:*`, `workspace:*`, `pane:*`, and `surface:*` refs
- non-focus socket creation commands preserve the current selected workspace/surface
- socket text send/read is wired to the live VTE terminal backend
- terminal focus and tab switches update Linux surface focus state
- desktop notifications are attempted through `notify-send` when available
- session state is saved on revision changes and restored on relaunch
- GTK accelerators mirror the existing macOS workspace/surface/split command set where Linux supports the same action
- workspace metadata and terminal spawn now respect the `cwd` passed to `workspace.create`
- Linux VTE terminals now execute `surface.send_text` input correctly and update workspace cwd after shell directory changes
- focusing a notified surface or selecting a notified workspace now marks those notifications read and increments the target surface flash counter
- when `app.focus_override.set` is `active`, Linux suppresses notifications for the currently focused surface/workspace instead of storing unread notifications
- Linux now supports real multi-window socket control with `window.list`, `window.current`, `window.create`, `window.focus`, `window.close`, and `workspace.move_to_window`
- Linux now supports workspace rename/reorder plus pane/surface move and reorder APIs through the shared v2 socket contract
- Linux now exposes the remaining shared client-facing tmux-compat helpers as either real operations (`pane.swap`, `pane.break`, `pane.join`, `surface.clear_history`) or lightweight debug shims (`debug.layout`, `debug.terminal.read_text`, `debug.window.screenshot`)
- Linux now exposes the remaining shared window-scoped shortcut/debug shims used by the acceptance suite, including `debug.sidebar.visible`, `debug.portal.stats`, `cmd+b` sidebar toggling, and `cmd+t` surface creation through `debug.shortcut.simulate`

## Keyboard shortcuts on Linux

Linux currently wires these app actions:

- `Primary+Alt+N`: new window
- `Primary+Alt+W`: close window
- `Primary+N`: new workspace
- `Primary+Shift+W`: close workspace
- `Primary+Control+]`: next workspace
- `Primary+Control+[` : previous workspace
- `Primary+Control+\``: last workspace
- `Primary+D`: split right
- `Primary+Shift+D`: split down
- `Primary+T`: new surface
- `Primary+W`: close surface
- `Primary+Shift+]`: next surface
- `Primary+Shift+[` : previous surface
- `Primary+Alt+Left/Right/Up/Down`: focus adjacent pane
- `Primary+Alt+\``: focus last pane

On Linux, GTK maps `Primary` to the platform's main modifier, which is typically `Ctrl`.

## Current gaps

This is still not macOS parity yet:

- browser surfaces remain unsupported
- macOS-specific window chrome, menus, and browser integrations are not ported
- Linux uses the supported VTE backend rather than the macOS terminal host path

## Next implementation checkpoints

1. Replace the polling UI refresh path with more targeted updates around unread/focus changes.
2. Harden the VTE path around focus, close churn, and session restore.
3. Add browser surfaces only after the terminal/workspace path is fully stable.
