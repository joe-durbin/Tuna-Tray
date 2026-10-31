//! tuna-tray: StatusNotifier tray internet radio. Audio is played by mpv,
//! driven over its JSON IPC socket.
//!
//!   tuna-tray          run the tray app
//!   tuna-tray --toggle  for a keyboard shortcut: stop if playing, otherwise play
//!                      the last station (launching the app if it isn't running)
//!
//! Stations live in $XDG_CONFIG_HOME/tuna-tray/stations.yaml (default
//! ~/.config/tuna-tray/stations.yaml). Each top-level key is a category, shown
//! as a submenu in the tray, in file order:
//!
//!   Rock:
//!     - name: Radio X
//!       url: https://example.com/stream

mod icon;
mod theme;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use ksni::{Icon, ToolTip, Tray};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use indexmap::IndexMap;
use zbus::blocking::Connection;

const APP: &str = "tuna-tray";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Station {
    name: String,
    url: String,
}

/// stations.yaml: category name -> stations, in file order.
type Config = IndexMap<String, Vec<Station>>;

/// Stations grouped by category, in the order each category first appears.
struct Group {
    name: String,
    stations: Vec<Station>,
}

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).expect("HOME is not set")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn control_sock() -> PathBuf {
    runtime_dir().join(format!("{APP}.sock"))
}

fn mpv_sock() -> PathBuf {
    runtime_dir().join(format!("{APP}-mpv.sock"))
}

fn stations_path() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join(APP).join("stations.yaml")
}

const DEFAULT_STATIONS: &str = include_str!("default-stations.yaml");

/// Writes the default station list if there is no station file yet. An existing file is never touched.
fn ensure_stations_file() -> Result<(), String> {
    let path = stations_path();
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => file
            .write_all(DEFAULT_STATIONS.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Shows a desktop notification through the freedesktop Notifications service.
fn notify(summary: &str, body: &str) {
    let Ok(conn) = Connection::session() else { return };
    let hints: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    let _ = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(APP, 0u32, "audio-x-generic", summary, body, Vec::<&str>::new(), hints, -1i32),
    );
}

/// Opens the station file in the user's default text editor.
fn open_stations_file() {
    if let Err(e) = ensure_stations_file() {
        eprintln!("{APP}: {e}");
        return;
    }
    let spawned = Command::new("xdg-open")
        .arg(stations_path())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = spawned {
        eprintln!("{APP}: cannot open editor: {e}");
    }
}

fn last_path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join(APP).join("last.toml")
}

fn load_groups() -> Result<Vec<Group>, String> {
    let path = stations_path();
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let config: Config =
        serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let groups: Vec<Group> = config
        .into_iter()
        .filter(|(_, stations)| !stations.is_empty())
        .map(|(name, stations)| Group { name, stations })
        .collect();
    if groups.is_empty() {
        return Err(format!("{}: no stations", path.display()));
    }
    Ok(groups)
}

fn load_last() -> Option<Station> {
    toml::from_str(&fs::read_to_string(last_path()).ok()?).ok()
}

fn save_last(station: &Station) {
    let path = last_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(text) = toml::to_string(station) {
        let _ = fs::write(path, text);
    }
}

/// Send one command over mpv's IPC socket, retrying while mpv is still starting.
fn mpv_send(args: &[Value]) -> io::Result<()> {
    let line = format!("{}\n", json!({ "command": args }));
    let mut last_err = None;
    for _ in 0..30 {
        match UnixStream::connect(mpv_sock()) {
            Ok(mut stream) => return stream.write_all(line.as_bytes()),
            Err(e) => {
                last_err = Some(e);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("mpv socket unavailable")))
}

fn spawn_mpv() -> Result<Child, String> {
    let mut cmd = Command::new("mpv");
    cmd.args(["--idle=yes", "--no-video", "--no-terminal"])
        .arg(format!("--input-ipc-server={}", mpv_sock().display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Kill mpv if tuna-tray dies without running its shutdown path (e.g. SIGKILL).
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| format!("cannot start mpv: {e}"))
}

/// Whether mpv has no file loaded (`idle-active`). None if mpv can't be reached.
fn mpv_idle() -> Option<bool> {
    let mut stream = UnixStream::connect(mpv_sock()).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .write_all(b"{\"command\":[\"get_property\",\"idle-active\"]}\n")
        .ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str::<Value>(&line).ok()?.get("data")?.as_bool()
}

struct TunaTray {
    groups: Vec<Group>,
    current: Option<Station>,
    playing: bool,
    started: Option<Instant>,
    dark_panel: bool,
    mpv: Child,
}

impl TunaTray {
    fn play(&mut self, station: &Station) {
        let result = mpv_send(&[json!("loadfile"), json!(station.url), json!("replace")])
            .and_then(|_| mpv_send(&[json!("set_property"), json!("pause"), json!(false)]));
        if let Err(e) = result {
            eprintln!("{APP}: mpv: {e}");
        }
        self.current = Some(station.clone());
        self.playing = true;
        self.started = Some(Instant::now());
        save_last(station);
    }

    fn stop(&mut self) {
        if let Err(e) = mpv_send(&[json!("stop")]) {
            eprintln!("{APP}: mpv: {e}");
        }
        self.playing = false;
    }

    /// Clears the playing flag once mpv has finished a stream on its own (network or decode failure).
    /// Ignores the first few seconds after a play request, while mpv is still loading.
    fn playback_ended(&mut self) {
        if self.playing && self.started.is_some_and(|s| s.elapsed() > Duration::from_secs(5)) {
            self.playing = false;
        }
    }

    fn toggle(&mut self) {
        if self.playing {
            self.stop();
            return;
        }
        match self.current.clone() {
            Some(station) => self.play(&station),
            None => {
                thread::spawn(|| {
                    notify("No station chosen yet", "Right-click the tray icon and pick a station.");
                });
            }
        }
    }

    fn quit(&mut self) {
        let _ = self.mpv.kill();
        let _ = self.mpv.wait();
        let _ = fs::remove_file(control_sock());
        let _ = fs::remove_file(mpv_sock());
        std::process::exit(0);
    }

    fn status_label(&self) -> String {
        match (&self.current, self.playing) {
            (Some(c), true) => format!("Playing: {}", c.name),
            (Some(c), false) => format!("Stopped ({})", c.name),
            (None, _) => "Stopped".into(),
        }
    }
}

impl Tray for TunaTray {
    fn id(&self) -> String {
        APP.into()
    }

    fn title(&self) -> String {
        APP.into()
    }

    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        icon::render(self.playing, self.dark_panel)
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: self.status_label(), ..Default::default() }
    }

    /// Left-click on the tray icon.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.toggle();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem { label: self.status_label(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            StandardItem {
                label: if self.playing { "Stop" } else { "Play" }.into(),
                activate: Box::new(|t: &mut Self| t.toggle()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];

        for group in &self.groups {
            let submenu = group
                .stations
                .iter()
                .map(|s| {
                    let station = s.clone();
                    CheckmarkItem {
                        label: s.name.clone(),
                        checked: self.current.as_ref().is_some_and(|c| c.url == s.url),
                        activate: Box::new(move |t: &mut Self| t.play(&station)),
                        ..Default::default()
                    }
                    .into()
                })
                .collect();
            items.push(SubMenu { label: group.name.clone(), submenu, ..Default::default() }.into());
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Edit Stations".into(),
                activate: Box::new(|_: &mut Self| open_stations_file()),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.quit()),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Run the tray. Blocks for the lifetime of the process.
fn run() -> Result<(), String> {
    ensure_stations_file()?;
    let groups = load_groups()?;
    let _ = fs::remove_file(mpv_sock());
    let _ = fs::remove_file(control_sock());

    let mpv = spawn_mpv()?;
    let listener =
        UnixListener::bind(control_sock()).map_err(|e| format!("control socket: {e}"))?;

    let dark_panel = Connection::session().map(|conn| theme::prefers_dark(&conn)).unwrap_or(true);
    let tray = TunaTray { groups, current: load_last(), playing: false, started: None, dark_panel, mpv };
    let handle = tray.spawn().map_err(|e| format!("tray: {e}"))?;
    let watcher = handle.clone();
    theme::watch(move |dark| {
        watcher.update(|t| t.dark_panel = dark);
    });

    // mpv ends a stream by itself when the network or decoder fails. Follow its real state
    // so the icon doesn't stay "playing" while nothing is heard.
    let sync = handle.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(2));
        if mpv_idle() == Some(true) {
            sync.update(|t| t.playback_ended());
        }
    });

    // Resume the last-played station on start, so the tray is live straight away.
    if let Some(station) = load_last() {
        handle.update(move |t| t.play(&station));
    }

    for stream in listener.incoming().flatten() {
        let mut line = String::new();
        if BufReader::new(stream).read_line(&mut line).is_ok() && line.trim() == "toggle" {
            handle.update(|t| t.toggle());
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let toggle = args.iter().any(|a| a == "--toggle");

    // Already running? Forward the toggle, or report and exit.
    if let Ok(mut stream) = UnixStream::connect(control_sock()) {
        if toggle {
            let _ = stream.write_all(b"toggle\n");
        } else {
            eprintln!("{APP} is already running");
        }
        return;
    }

    // Not running: a toggle launches a detached instance that starts playing.
    if toggle {
        let exe = env::current_exe().expect("cannot resolve own path");
        let _ = Command::new(exe)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        return;
    }

    if let Err(e) = run() {
        eprintln!("{APP}: {e}");
        std::process::exit(1);
    }
}
