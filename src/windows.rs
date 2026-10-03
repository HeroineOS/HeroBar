//! Open windows and workspaces, for the taskbar and workspaces modules,
//! from the compositor:
//!
//! - HeroWM (and fht-compositor): its IPC event stream ($FHTC_SOCKET_PATH).
//! - sway: its IPC ($SWAYSOCK).
//! - Other wlroots-style compositors (Hyprland, labwc, river, Wayfire):
//!   the wlr-foreign-toplevel-management protocol, over our own small
//!   Wayland connection (windows only: it has no workspaces).
//!
//! Both run on one worker thread that sleeps until the compositor reports
//! a change, then sends the whole (small) list to the UI.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::sync::Arc;

use crate::apps;

#[derive(Debug, Clone, PartialEq)]
pub struct Win {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub focused: bool,
    /// From the app's .desktop file, if found.
    pub app: Option<apps::App>,
    /// The workspace it's on, if the compositor says.
    pub workspace: Option<u64>,
}

/// A workspace of the bar's monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct Ws {
    pub id: u64,
    /// "1", "2"... or the workspace's name.
    pub label: String,
    /// Shown on the monitor now.
    pub active: bool,
    /// Has windows.
    pub occupied: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Cmd {
    Focus(u64),
    Close(u64),
    FocusWorkspace(u64),
}

/// Sends window commands to the compositor.
#[derive(Clone)]
pub struct Control(Arc<dyn Fn(Cmd) + Send + Sync>);

impl Control {
    pub fn send(&self, cmd: Cmd) {
        (self.0)(cmd)
    }
}

#[derive(Clone)]
pub enum Update {
    /// The backend is running; commands go through this.
    Ready(Control),
    Windows(Vec<Win>),
    Workspaces(Vec<Ws>),
}

/// The worker: picks a backend and reports until the connection ends.
/// `output`: the monitor whose workspaces to report (None: the active one).
pub fn run(output: Option<String>, send: impl Fn(Update) -> bool + Send + 'static) {
    let mut finder = apps::Finder::default();
    let mut emit = move |u: Update| match u {
        Update::Windows(wins) => {
            let wins = wins
                .into_iter()
                .map(|mut w| {
                    w.app = finder.find(&w.app_id);
                    w
                })
                .collect();
            send(Update::Windows(wins))
        }
        ready => send(ready),
    };
    let result = if let Some(path) = std::env::var_os("FHTC_SOCKET_PATH") {
        fht::run(path.into(), output.as_deref(), &mut emit)
    } else if let Some(path) = std::env::var_os("SWAYSOCK") {
        sway::run(path.into(), output.as_deref(), &mut emit)
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        wlr::run(&mut emit)
    } else {
        Err("no supported compositor (HeroWM IPC or wlr-foreign-toplevel-management)".into())
    };
    if let Err(e) = result {
        eprintln!("herobar: taskbar: {e}");
    }
}

/// HeroWM / fht-compositor IPC: one JSON request per line; after
/// "subscribe" the socket streams events, starting with the full state.
mod fht {
    use super::*;
    use serde::Deserialize;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct Window {
        id: u64,
        title: Option<String>,
        app_id: Option<String>,
        workspace_id: Option<u64>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct Workspace {
        id: u64,
        #[serde(default)]
        windows: Vec<u64>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct Monitor {
        output: String,
        workspaces: Vec<u64>,
        active_workspace_idx: usize,
        #[serde(default)]
        active: bool,
    }

    #[derive(Deserialize)]
    struct Space {
        monitors: HashMap<String, Monitor>,
    }

    #[derive(Deserialize)]
    #[serde(tag = "event", content = "data", rename_all = "kebab-case")]
    enum Event {
        Windows(BTreeMap<String, Window>),
        FocusedWindowChanged { id: Option<u64> },
        WindowChanged(Window),
        WindowClosed { id: u64 },
        Workspaces(BTreeMap<String, Workspace>),
        WorkspaceChanged(Workspace),
        WorkspaceRemoved { id: u64 },
        ActiveWorkspaceChanged { id: u64 },
        Space(Space),
    }

    fn win(w: Window) -> Win {
        Win {
            id: w.id,
            app_id: w.app_id.unwrap_or_default(),
            title: w.title.unwrap_or_default(),
            focused: false,
            app: None,
            workspace: w.workspace_id,
        }
    }

    /// Runs one action on a fresh connection (the subscribed one can't
    /// take requests).
    fn action(path: &PathBuf, cmd: Cmd) -> std::io::Result<()> {
        let json = match cmd {
            Cmd::Focus(id) => format!(r#"{{"action":{{"focus-window":{{"window-id":{id}}}}}}}"#),
            Cmd::Close(id) => format!(r#"{{"action":{{"close-window":{{"window-id":{id},"kill":false}}}}}}"#),
            Cmd::FocusWorkspace(id) => format!(r#"{{"action":{{"focus-workspace":{{"workspace-id":{id}}}}}}}"#),
        };
        let mut s = UnixStream::connect(path)?;
        s.write_all(json.as_bytes())?;
        s.write_all(b"\n")?;
        // Wait for the reply so the compositor isn't left writing to a
        // closed socket.
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line)?;
        Ok(())
    }

    #[derive(Default)]
    struct State {
        wins: BTreeMap<u64, Win>,
        focused: Option<u64>,
        workspaces: BTreeMap<u64, Workspace>,
        space: Option<Space>,
    }

    impl State {
        fn apply(&mut self, ev: Event) {
            match ev {
                Event::Windows(all) => self.wins = all.into_values().map(|w| (w.id, win(w))).collect(),
                Event::FocusedWindowChanged { id } => self.focused = id,
                Event::WindowChanged(w) => {
                    self.wins.insert(w.id, win(w));
                }
                Event::WindowClosed { id } => {
                    self.wins.remove(&id);
                }
                Event::Workspaces(all) => self.workspaces = all.into_values().map(|w| (w.id, w)).collect(),
                Event::WorkspaceChanged(w) => {
                    self.workspaces.insert(w.id, w);
                }
                Event::WorkspaceRemoved { id } => {
                    self.workspaces.remove(&id);
                }
                Event::ActiveWorkspaceChanged { id } => {
                    if let Some(space) = &mut self.space {
                        for m in space.monitors.values_mut() {
                            if let Some(i) = m.workspaces.iter().position(|&w| w == id) {
                                m.active_workspace_idx = i;
                            }
                        }
                    }
                }
                Event::Space(s) => self.space = Some(s),
            }
        }

        fn windows(&self) -> Vec<Win> {
            self.wins
                .values()
                .cloned()
                .map(|mut w| {
                    w.focused = Some(w.id) == self.focused;
                    w
                })
                .collect()
        }

        /// The chosen monitor's workspaces (1..9), in order.
        fn workspaces(&self, output: Option<&str>) -> Vec<Ws> {
            let Some(space) = &self.space else { return vec![] };
            let mon = match output {
                Some(o) => space.monitors.values().find(|m| m.output == o),
                None => space.monitors.values().find(|m| m.active).or_else(|| space.monitors.values().next()),
            };
            let Some(mon) = mon else { return vec![] };
            mon.workspaces
                .iter()
                .enumerate()
                .map(|(i, &id)| Ws {
                    id,
                    label: (i + 1).to_string(),
                    active: i == mon.active_workspace_idx,
                    occupied: self.workspaces.get(&id).is_some_and(|w| !w.windows.is_empty())
                        || self.wins.values().any(|w| w.workspace == Some(id)),
                })
                .collect()
        }
    }

    #[cfg(test)]
    pub fn parse(line: &str) -> Option<(u64, String)> {
        match serde_json::from_str::<Event>(line).ok()? {
            Event::WindowChanged(w) => Some((w.id, w.app_id.unwrap_or_default())),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn workspaces_from(lines: &[&str]) -> Vec<Ws> {
        let mut st = State::default();
        for l in lines {
            st.apply(serde_json::from_str(l).unwrap());
        }
        st.workspaces(None)
    }

    pub fn run(path: PathBuf, output: Option<&str>, emit: &mut dyn FnMut(Update) -> bool) -> Result<(), String> {
        let mut s = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        s.write_all(b"\"subscribe\"\n").map_err(|e| e.to_string())?;
        let control = {
            let path = path.clone();
            Control(Arc::new(move |cmd| {
                let path = path.clone();
                std::thread::spawn(move || {
                    if let Err(e) = action(&path, cmd) {
                        eprintln!("herobar: taskbar: {e}");
                    }
                });
            }))
        };
        if !emit(Update::Ready(control)) {
            return Ok(());
        }
        let mut st = State::default();
        let (mut last_wins, mut last_ws) = (None, None);
        for line in BufReader::new(s).lines() {
            let line = line.map_err(|e| e.to_string())?;
            // Layer-shell events and anything newer don't concern us.
            let Ok(ev) = serde_json::from_str::<Event>(&line) else { continue };
            st.apply(ev);
            // Send only what changed.
            let wins = st.windows();
            if last_wins.as_ref() != Some(&wins) {
                last_wins = Some(wins.clone());
                if !emit(Update::Windows(wins)) {
                    break;
                }
            }
            let ws = st.workspaces(output);
            if last_ws.as_ref() != Some(&ws) {
                last_ws = Some(ws.clone());
                if !emit(Update::Workspaces(ws)) {
                    break;
                }
            }
        }
        Ok(())
    }
}

/// sway's IPC (the i3 protocol): subscribe to window and workspace events;
/// on each, read the tree and the workspace list again.
mod sway {
    use super::*;
    use serde::Deserialize;
    use std::path::PathBuf;

    const RUN_COMMAND: u32 = 0;
    const GET_WORKSPACES: u32 = 1;
    const SUBSCRIBE: u32 = 2;
    const GET_TREE: u32 = 4;

    fn send(s: &mut UnixStream, kind: u32, payload: &str) -> std::io::Result<()> {
        let mut msg = Vec::with_capacity(14 + payload.len());
        msg.extend_from_slice(b"i3-ipc");
        msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        msg.extend_from_slice(&kind.to_ne_bytes());
        msg.extend_from_slice(payload.as_bytes());
        s.write_all(&msg)
    }

    fn recv(s: &mut UnixStream) -> std::io::Result<(u32, Vec<u8>)> {
        let mut head = [0u8; 14];
        s.read_exact(&mut head)?;
        let len = u32::from_ne_bytes(head[6..10].try_into().expect("4 bytes")) as usize;
        let kind = u32::from_ne_bytes(head[10..14].try_into().expect("4 bytes"));
        let mut body = vec![0u8; len];
        s.read_exact(&mut body)?;
        Ok((kind, body))
    }

    fn request(path: &PathBuf, kind: u32, payload: &str) -> std::io::Result<Vec<u8>> {
        let mut s = UnixStream::connect(path)?;
        send(&mut s, kind, payload)?;
        Ok(recv(&mut s)?.1)
    }

    #[derive(Deserialize)]
    struct Node {
        id: u64,
        #[serde(rename = "type")]
        kind: String,
        name: Option<String>,
        app_id: Option<String>,
        #[serde(default)]
        focused: bool,
        window_properties: Option<WindowProps>,
        #[serde(default)]
        nodes: Vec<Node>,
        #[serde(default)]
        floating_nodes: Vec<Node>,
    }

    #[derive(Deserialize)]
    struct WindowProps {
        class: Option<String>,
    }

    #[derive(Deserialize)]
    struct Workspace {
        id: u64,
        name: String,
        #[serde(default)]
        focused: bool,
        #[serde(default)]
        visible: bool,
        output: String,
    }

    /// Windows in the tree, with their workspace.
    fn collect(n: &Node, ws: Option<u64>, out: &mut Vec<Win>) {
        let ws = if n.kind == "workspace" { Some(n.id) } else { ws };
        let app_id = n.app_id.clone().or_else(|| n.window_properties.as_ref().and_then(|p| p.class.clone()));
        if (n.kind == "con" || n.kind == "floating_con") && app_id.is_some() && n.nodes.is_empty() {
            out.push(Win {
                id: n.id,
                app_id: app_id.unwrap_or_default(),
                title: n.name.clone().unwrap_or_default(),
                focused: n.focused,
                app: None,
                workspace: ws,
            });
        }
        // The scratchpad isn't a real workspace.
        if n.name.as_deref() == Some("__i3") {
            return;
        }
        for c in n.nodes.iter().chain(&n.floating_nodes) {
            collect(c, ws, out);
        }
    }

    fn snapshot(path: &PathBuf, output: Option<&str>) -> Result<(Vec<Win>, Vec<Ws>), String> {
        let tree: Node = serde_json::from_slice(&request(path, GET_TREE, "").map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut wins = Vec::new();
        collect(&tree, None, &mut wins);
        wins.sort_by_key(|w| w.id);
        let all: Vec<Workspace> =
            serde_json::from_slice(&request(path, GET_WORKSPACES, "").map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        // The bar's monitor: the configured one, else the focused one.
        let out = output.map(str::to_owned).or_else(|| all.iter().find(|w| w.focused).map(|w| w.output.clone()));
        let ws = all
            .iter()
            .filter(|w| out.as_deref().is_none_or(|o| w.output == o))
            .map(|w| Ws {
                id: w.id,
                label: w.name.clone(),
                active: w.visible,
                occupied: wins.iter().any(|x| x.workspace == Some(w.id)),
            })
            .collect();
        Ok((wins, ws))
    }

    pub fn run(path: PathBuf, output: Option<&str>, emit: &mut dyn FnMut(Update) -> bool) -> Result<(), String> {
        let mut events = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        send(&mut events, SUBSCRIBE, r#"["window","workspace"]"#).map_err(|e| e.to_string())?;
        recv(&mut events).map_err(|e| e.to_string())?;
        let control = {
            let path = path.clone();
            Control(Arc::new(move |cmd| {
                let c = match cmd {
                    Cmd::Focus(id) => format!("[con_id={id}] focus"),
                    Cmd::Close(id) => format!("[con_id={id}] kill"),
                    // Workspace ids aren't commands' arguments; names are.
                    Cmd::FocusWorkspace(id) => format!("__ws {id}"),
                };
                let path = path.clone();
                std::thread::spawn(move || {
                    let c = match c.strip_prefix("__ws ") {
                        Some(id) => {
                            let Ok(body) = request(&path, GET_WORKSPACES, "") else { return };
                            let Ok(all) = serde_json::from_slice::<Vec<Workspace>>(&body) else { return };
                            let Some(w) = all.iter().find(|w| w.id.to_string() == id) else { return };
                            format!("workspace \"{}\"", w.name.replace('"', "\\\""))
                        }
                        None => c,
                    };
                    if let Err(e) = request(&path, RUN_COMMAND, &c) {
                        eprintln!("herobar: taskbar: {e}");
                    }
                });
            }))
        };
        if !emit(Update::Ready(control)) {
            return Ok(());
        }
        let (mut last_wins, mut last_ws) = (None, None);
        loop {
            let (wins, ws) = snapshot(&path, output)?;
            if last_wins.as_ref() != Some(&wins) {
                last_wins = Some(wins.clone());
                if !emit(Update::Windows(wins)) {
                    return Ok(());
                }
            }
            if last_ws.as_ref() != Some(&ws) {
                last_ws = Some(ws.clone());
                if !emit(Update::Workspaces(ws)) {
                    return Ok(());
                }
            }
            // Wait for the next event (its content doesn't matter).
            recv(&mut events).map_err(|e| e.to_string())?;
        }
    }
}

/// wlr-foreign-toplevel-management-unstable-v1 through raw libwayland
/// (linked anyway by FLTK), so no Wayland crates are needed.
mod wlr {
    use super::*;
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::os::fd::AsRawFd;
    use std::sync::OnceLock;

    #[repr(C)]
    pub struct Interface {
        name: *const c_char,
        version: c_int,
        method_count: c_int,
        methods: *const Message,
        event_count: c_int,
        events: *const Message,
    }

    #[repr(C)]
    struct Message {
        name: *const c_char,
        signature: *const c_char,
        types: *const *const Interface,
    }

    #[repr(C)]
    struct Array {
        size: usize,
        alloc: usize,
        data: *mut c_void,
    }

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: i16,
        revents: i16,
    }

    extern "C" {
        static wl_registry_interface: Interface;
        static wl_seat_interface: Interface;
        static wl_output_interface: Interface;
        static wl_surface_interface: Interface;
        fn wl_display_connect(name: *const c_char) -> *mut c_void;
        fn wl_display_disconnect(display: *mut c_void);
        fn wl_display_roundtrip(display: *mut c_void) -> c_int;
        fn wl_display_get_fd(display: *mut c_void) -> c_int;
        fn wl_display_prepare_read(display: *mut c_void) -> c_int;
        fn wl_display_read_events(display: *mut c_void) -> c_int;
        fn wl_display_cancel_read(display: *mut c_void);
        fn wl_display_dispatch_pending(display: *mut c_void) -> c_int;
        fn wl_display_flush(display: *mut c_void) -> c_int;
        fn wl_proxy_marshal_flags(
            proxy: *mut c_void,
            opcode: u32,
            interface: *const Interface,
            version: u32,
            flags: u32, ...
        ) -> *mut c_void;
        fn wl_proxy_get_version(proxy: *mut c_void) -> u32;
        fn wl_proxy_add_listener(proxy: *mut c_void, listener: *const c_void, data: *mut c_void) -> c_int;
        fn poll(fds: *mut PollFd, n: std::ffi::c_ulong, timeout: c_int) -> c_int;
    }

    const MARSHAL_DESTROY: u32 = 1;
    // zwlr_foreign_toplevel_handle_v1 requests
    const ACTIVATE: u32 = 4;
    const CLOSE: u32 = 5;
    const DESTROY: u32 = 7;
    // state values
    const STATE_ACTIVATED: u32 = 2;

    struct Ifaces {
        manager: Interface,
    }
    struct SyncIfaces(Ifaces);
    unsafe impl Sync for SyncIfaces {}
    unsafe impl Send for SyncIfaces {}

    fn cs(s: &str) -> *const c_char {
        CString::new(s).expect("no NUL").into_raw()
    }

    fn msg(name: &str, sig: &str, types: Vec<*const Interface>) -> Message {
        Message { name: cs(name), signature: cs(sig), types: Box::leak(types.into_boxed_slice()).as_ptr() }
    }

    /// The protocol's interface descriptions (what wayland-scanner would
    /// generate), built once and kept for the process's life.
    fn ifaces() -> &'static Ifaces {
        static I: OnceLock<SyncIfaces> = OnceLock::new();
        &I.get_or_init(|| unsafe {
            let null = std::ptr::null::<Interface>();
            // The handle interface is referenced by the manager's event and
            // its own `parent` event: allocate it first, fill it in after.
            let handle: &'static mut Interface = Box::leak(Box::new(Interface {
                name: cs("zwlr_foreign_toplevel_handle_v1"),
                version: 3,
                method_count: 0,
                methods: std::ptr::null(),
                event_count: 0,
                events: std::ptr::null(),
            }));
            let h = handle as *const Interface;
            let methods = vec![
                msg("set_maximized", "", vec![]),
                msg("unset_maximized", "", vec![]),
                msg("set_minimized", "", vec![]),
                msg("unset_minimized", "", vec![]),
                msg("activate", "o", vec![&wl_seat_interface]),
                msg("close", "", vec![]),
                msg("set_rectangle", "oiiii", vec![&wl_surface_interface, null, null, null, null]),
                msg("destroy", "", vec![]),
                msg("set_fullscreen", "2?o", vec![&wl_output_interface]),
                msg("unset_fullscreen", "2", vec![]),
            ];
            let events = vec![
                msg("title", "s", vec![null]),
                msg("app_id", "s", vec![null]),
                msg("output_enter", "o", vec![&wl_output_interface]),
                msg("output_leave", "o", vec![&wl_output_interface]),
                msg("state", "a", vec![null]),
                msg("done", "", vec![]),
                msg("closed", "", vec![]),
                msg("parent", "3?o", vec![h]),
            ];
            handle.method_count = methods.len() as c_int;
            handle.methods = Box::leak(methods.into_boxed_slice()).as_ptr();
            handle.event_count = events.len() as c_int;
            handle.events = Box::leak(events.into_boxed_slice()).as_ptr();
            let manager = Interface {
                name: cs("zwlr_foreign_toplevel_manager_v1"),
                version: 3,
                method_count: 1,
                methods: Box::leak(vec![msg("stop", "", vec![])].into_boxed_slice()).as_ptr(),
                event_count: 2,
                events: Box::leak(vec![msg("toplevel", "n", vec![h]), msg("finished", "", vec![])].into_boxed_slice()).as_ptr(),
            };
            SyncIfaces(Ifaces { manager })
        })
        .0
    }

    #[derive(Default)]
    struct Toplevel {
        id: u64,
        title: String,
        app_id: String,
        active: bool,
    }

    struct State {
        manager: *mut c_void,
        manager_name: u32,
        manager_version: u32,
        seat: *mut c_void,
        next_id: u64,
        /// By handle pointer.
        tops: BTreeMap<usize, Toplevel>,
        changed: bool,
        finished: bool,
    }

    #[repr(C)]
    struct RegistryListener {
        global: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *const c_char, u32),
        global_remove: unsafe extern "C" fn(*mut c_void, *mut c_void, u32),
    }
    #[repr(C)]
    struct ManagerListener {
        toplevel: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
        finished: unsafe extern "C" fn(*mut c_void, *mut c_void),
    }
    #[repr(C)]
    struct HandleListener {
        title: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char),
        app_id: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char),
        output_enter: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
        output_leave: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
        state: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut Array),
        done: unsafe extern "C" fn(*mut c_void, *mut c_void),
        closed: unsafe extern "C" fn(*mut c_void, *mut c_void),
        parent: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
    }

    unsafe fn st<'a>(data: *mut c_void) -> &'a mut State {
        &mut *(data as *mut State)
    }

    unsafe extern "C" fn global(data: *mut c_void, reg: *mut c_void, name: u32, iface: *const c_char, version: u32) {
        let s = st(data);
        match CStr::from_ptr(iface).to_bytes() {
            b"zwlr_foreign_toplevel_manager_v1" if s.manager.is_null() => {
                s.manager_name = name;
                s.manager_version = version.min(3);
            }
            b"wl_seat" if s.seat.is_null() => s.seat = bind(reg, name, &wl_seat_interface, 1),
            _ => {}
        }
    }
    unsafe extern "C" fn global_remove(_: *mut c_void, _: *mut c_void, _: u32) {}

    unsafe fn bind(reg: *mut c_void, name: u32, iface: *const Interface, version: u32) -> *mut c_void {
        // wl_registry.bind (opcode 0): name, then an untyped new_id
        // (interface name, version, id).
        wl_proxy_marshal_flags(reg, 0, iface, version, 0, name, (*iface).name, version, std::ptr::null_mut::<c_void>())
    }

    unsafe extern "C" fn toplevel(data: *mut c_void, _: *mut c_void, handle: *mut c_void) {
        let s = st(data);
        s.next_id += 1;
        s.tops.insert(handle as usize, Toplevel { id: s.next_id, ..Default::default() });
        wl_proxy_add_listener(handle, &HANDLE_LISTENER as *const _ as *const c_void, data);
    }
    unsafe extern "C" fn finished(data: *mut c_void, _: *mut c_void) {
        st(data).finished = true;
    }
    unsafe extern "C" fn title(data: *mut c_void, h: *mut c_void, t: *const c_char) {
        if let Some(top) = st(data).tops.get_mut(&(h as usize)) {
            top.title = CStr::from_ptr(t).to_string_lossy().into_owned();
        }
    }
    unsafe extern "C" fn app_id(data: *mut c_void, h: *mut c_void, t: *const c_char) {
        if let Some(top) = st(data).tops.get_mut(&(h as usize)) {
            top.app_id = CStr::from_ptr(t).to_string_lossy().into_owned();
        }
    }
    unsafe extern "C" fn output(_: *mut c_void, _: *mut c_void, _: *mut c_void) {}
    unsafe extern "C" fn state(data: *mut c_void, h: *mut c_void, a: *mut Array) {
        let a = &*a;
        let states = std::slice::from_raw_parts(a.data as *const u32, a.size / 4);
        if let Some(top) = st(data).tops.get_mut(&(h as usize)) {
            top.active = states.contains(&STATE_ACTIVATED);
        }
    }
    unsafe extern "C" fn done(data: *mut c_void, _: *mut c_void) {
        st(data).changed = true;
    }
    unsafe extern "C" fn closed(data: *mut c_void, h: *mut c_void) {
        let s = st(data);
        s.tops.remove(&(h as usize));
        s.changed = true;
        wl_proxy_marshal_flags(h, DESTROY, std::ptr::null(), wl_proxy_get_version(h), MARSHAL_DESTROY);
    }

    static REGISTRY_LISTENER: RegistryListener = RegistryListener { global, global_remove };
    static MANAGER_LISTENER: ManagerListener = ManagerListener { toplevel, finished };
    static HANDLE_LISTENER: HandleListener = HandleListener {
        title,
        app_id,
        output_enter: output,
        output_leave: output,
        state,
        done,
        closed,
        parent: output,
    };

    pub fn run(emit: &mut dyn FnMut(Update) -> bool) -> Result<(), String> {
        let ifaces = ifaces();
        unsafe {
            let display = wl_display_connect(std::ptr::null());
            if display.is_null() {
                return Err("can't connect to the Wayland display".into());
            }
            let mut s = State {
                manager: std::ptr::null_mut(),
                manager_name: 0,
                manager_version: 0,
                seat: std::ptr::null_mut(),
                next_id: 0,
                tops: BTreeMap::new(),
                changed: false,
                finished: false,
            };
            let data = &mut s as *mut State as *mut c_void;
            // wl_display.get_registry (opcode 1)
            let registry = wl_proxy_marshal_flags(display, 1, &wl_registry_interface, wl_proxy_get_version(display), 0, std::ptr::null_mut::<c_void>());
            wl_proxy_add_listener(registry, &REGISTRY_LISTENER as *const _ as *const c_void, data);
            wl_display_roundtrip(display);
            if s.manager_name == 0 {
                wl_display_disconnect(display);
                return Err("the compositor has no wlr-foreign-toplevel-management".into());
            }
            s.manager = bind(registry, s.manager_name, &ifaces.manager, s.manager_version);
            wl_proxy_add_listener(s.manager, &MANAGER_LISTENER as *const _ as *const c_void, data);

            // Commands from the UI thread come through a channel; a byte on
            // a socket pair wakes the poll below.
            let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
            let (mut wake_rx, wake_tx) = UnixStream::pair().map_err(|e| e.to_string())?;
            let cmd_tx = std::sync::Mutex::new(cmd_tx);
            let wake_tx = std::sync::Mutex::new(wake_tx);
            let control = Control(Arc::new(move |cmd| {
                if cmd_tx.lock().map(|t| t.send(cmd).is_ok()).unwrap_or(false) {
                    let _ = wake_tx.lock().map(|mut w| w.write(&[1]));
                }
            }));
            if !emit(Update::Ready(control)) {
                wl_display_disconnect(display);
                return Ok(());
            }
            let fd = wl_display_get_fd(display);
            loop {
                while wl_display_prepare_read(display) != 0 {
                    if wl_display_dispatch_pending(display) < 0 {
                        return Err("Wayland connection lost".into());
                    }
                }
                wl_display_flush(display);
                let mut fds = [
                    PollFd { fd, events: 1, revents: 0 },
                    PollFd { fd: wake_rx.as_raw_fd(), events: 1, revents: 0 },
                ];
                if poll(fds.as_mut_ptr(), 2, -1) < 0 {
                    wl_display_cancel_read(display);
                    continue;
                }
                if fds[0].revents != 0 {
                    if wl_display_read_events(display) < 0 {
                        return Err("Wayland connection lost".into());
                    }
                } else {
                    wl_display_cancel_read(display);
                }
                if wl_display_dispatch_pending(display) < 0 {
                    return Err("Wayland connection lost".into());
                }
                if fds[1].revents != 0 {
                    let mut buf = [0u8; 64];
                    let _ = wake_rx.read(&mut buf);
                    for cmd in cmd_rx.try_iter() {
                        let id = match cmd {
                            Cmd::Focus(id) | Cmd::Close(id) => id,
                            // No workspaces in this protocol.
                            Cmd::FocusWorkspace(_) => continue,
                        };
                        let Some((&h, _)) = s.tops.iter().find(|(_, t)| t.id == id) else { continue };
                        let h = h as *mut c_void;
                        match cmd {
                            Cmd::Focus(_) if !s.seat.is_null() => {
                                wl_proxy_marshal_flags(h, ACTIVATE, std::ptr::null(), wl_proxy_get_version(h), 0, s.seat);
                            }
                            Cmd::Close(_) => {
                                wl_proxy_marshal_flags(h, CLOSE, std::ptr::null(), wl_proxy_get_version(h), 0);
                            }
                            _ => {}
                        }
                    }
                }
                if s.finished {
                    return Err("the compositor stopped sending windows".into());
                }
                if std::mem::take(&mut s.changed) {
                    let list = s
                        .tops
                        .values()
                        .map(|t| Win { id: t.id, app_id: t.app_id.clone(), title: t.title.clone(), focused: t.active, app: None, workspace: None })
                        .collect::<Vec<_>>();
                    let mut list = list;
                    list.sort_by_key(|w| w.id);
                    if !emit(Update::Windows(list)) {
                        wl_display_disconnect(display);
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn herowm_events_parse() {
        // As fht-compositor-ipc serializes them (kebab-case fields).
        let line = r#"{"event":"window-changed","data":{"id":3,"title":"t","app-id":"foot","workspace-id":0,"size":[1,1],"location":[0,0],"fullscreened":false,"maximized":false,"tiled":true,"activated":true,"focused":true}}"#;
        assert_eq!(super::fht::parse(line), Some((3, "foot".into())));
    }

    #[test]
    fn herowm_workspaces() {
        let ws = super::fht::workspaces_from(&[
            r#"{"event":"workspaces","data":{"10":{"id":10,"output":"eDP-1","windows":[3],"active-window-idx":0,"fullscreen-window-idx":null,"mwfact":0.5,"nmaster":1},"11":{"id":11,"output":"eDP-1","windows":[],"active-window-idx":null,"fullscreen-window-idx":null,"mwfact":0.5,"nmaster":1}}}"#,
            r#"{"event":"space","data":{"monitors":{"eDP-1":{"output":"eDP-1","workspaces":[10,11,12,13,14,15,16,17,18],"active-workspace-idx":1,"active":true}},"primary-idx":0,"active-idx":0}}"#,
        ]);
        assert_eq!(ws.len(), 9);
        assert_eq!((ws[0].label.as_str(), ws[0].occupied, ws[0].active), ("1", true, false));
        assert_eq!((ws[1].occupied, ws[1].active), (false, true));
    }
}
