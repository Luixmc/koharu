//! Runs khr and LM Studio steps one after another as child processes, so the
//! app never holds an LM Studio model and a crash in a step never takes the
//! window down. The steps left are saved after every change, so a queue cut
//! short (power cut, closed app) can be resumed.
//!
//! khr writes the project file itself, and the editor would overwrite that
//! with its next commit; a step on the project open in the editor closes it
//! first and reopens it once the project has no more steps queued.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager as _, ipc::Channel};
use tauri_runtime_cef::CefRuntime;
use tokio::io::AsyncReadExt as _;

use super::files;
use crate::commands::{
    ChannelExt as _,
    lifecycle::{close_current_project, replace_project},
    project::{CurrentProject, ProjectLibrary},
};

const QUEUE_FILE: &str = r"I:\Koharu\cola.json";
/// Seconds Windows waits before shutting down, so `shutdown /a` can cancel it.
const SHUTDOWN_DELAY: &str = "120";
const MAX_LINES: usize = 5000;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Program {
    Khr,
    LmStudio,
}

/// What to do once a step exits successfully.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum After {
    Nothing,
    /// The study or learning left term proposals to review.
    ShowProposals,
    /// The reviewer left corrections to review.
    ShowCorrections,
    /// Approve the work's proposals, so the translation that follows the
    /// study uses its terms.
    ApproveProposals,
    /// The step created the project.
    Created,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Step {
    pub(crate) label: String,
    /// The project file the step works on, if any.
    pub(crate) project: Option<PathBuf>,
    pub(crate) program: Program,
    pub(crate) args: Vec<String>,
    pub(crate) ignore_error: bool,
    pub(crate) after: After,
}

impl Step {
    pub(crate) fn khr(project: Option<&Path>, label: impl Into<String>, args: &[&str]) -> Self {
        Self {
            label: label.into(),
            project: project.map(Path::to_path_buf),
            program: Program::Khr,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ignore_error: false,
            after: After::Nothing,
        }
    }

    pub(crate) fn lm_studio(
        project: Option<&Path>,
        label: impl Into<String>,
        args: &[&str],
    ) -> Self {
        Self {
            program: Program::LmStudio,
            ..Self::khr(project, label, args)
        }
    }

    pub(crate) fn may_fail(self) -> Self {
        Self {
            ignore_error: true,
            ..self
        }
    }

    pub(crate) fn then(self, after: After) -> Self {
        Self { after, ..self }
    }

    fn project_name(&self) -> Option<String> {
        self.project.as_deref().map(project_name)
    }

    fn name(&self) -> String {
        match self.project_name() {
            Some(project) => format!("[{project}] {}", self.label),
            None => self.label.clone(),
        }
    }
}

pub(crate) fn project_name(project: &Path) -> String {
    project
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How far the running step has got, from khr's `@progreso` lines.
#[derive(Clone, Debug, Serialize, Type)]
pub struct StepProgress {
    pub what: String,
    #[specta(type = f64)]
    pub done: usize,
    #[specta(type = f64)]
    pub total: usize,
    /// When the count first went up (epoch ms) and to what: the pace since
    /// then estimates the time left, so loading the model before the first
    /// unit does not skew it.
    pub first: Option<ProgressStart>,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
pub struct ProgressStart {
    pub at: f64,
    #[specta(type = f64)]
    pub done: usize,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct RunningStep {
    pub name: String,
    pub project: Option<String>,
    /// Epoch milliseconds.
    pub started: f64,
    pub progress: Option<StepProgress>,
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct QueueState {
    pub running: Option<RunningStep>,
    /// Projects with steps queued that have not started, in order.
    pub waiting: Vec<String>,
    /// Projects with steps running or queued; the editor leaves them alone.
    pub busy: Vec<String>,
    /// Steps a previous run left unfinished, offered for resuming.
    #[specta(type = f64)]
    pub unfinished: usize,
    pub unfinished_projects: Vec<String>,
    pub failed: bool,
    pub shutdown_when_done: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Proposals,
    Corrections,
    Created,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueueEvent {
    State {
        state: QueueState,
    },
    Lines {
        lines: Vec<String>,
    },
    Cleared,
    /// The work files of a project changed.
    WorkChanged {
        project: String,
    },
    /// A step left something for the user to look at.
    Finished {
        project: String,
        outcome: Outcome,
    },
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct QueueSnapshot {
    pub state: QueueState,
    pub log: Vec<String>,
}

#[derive(Default)]
struct Inner {
    steps: VecDeque<Step>,
    current: Option<(Step, f64, Option<StepProgress>)>,
    unfinished: Vec<Step>,
    failed: bool,
    shutdown_when_done: bool,
    log: Vec<String>,
    /// The project the queue closed in the editor, to reopen when done.
    reopen: Option<PathBuf>,
    /// The app is closing: the saved queue is left as it is.
    closing: bool,
}

pub(crate) struct Queue {
    khr: PathBuf,
    lms: PathBuf,
    inner: Mutex<Inner>,
    pid: Mutex<Option<u32>>,
    channel: Mutex<Option<Channel<QueueEvent>>>,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as f64)
        .unwrap_or_default()
}

impl Queue {
    pub(crate) fn new() -> Self {
        // khr is built beside the app; a debug build of the app uses the
        // release khr next to it in target/.
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let khr = [exe_dir.join("khr.exe"), exe_dir.join(r"..\release\khr.exe")]
            .into_iter()
            .find(|path| path.exists())
            .unwrap_or_else(|| PathBuf::from("khr.exe"));
        let lms = dirs::home_dir()
            .unwrap_or_default()
            .join(r".lmstudio\bin\lms.exe");
        let unfinished = std::fs::read_to_string(QUEUE_FILE)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            khr,
            lms,
            inner: Mutex::new(Inner {
                unfinished,
                ..Inner::default()
            }),
            pid: Mutex::new(None),
            channel: Mutex::new(None),
        }
    }

    pub(crate) fn lms(&self) -> &Path {
        &self.lms
    }

    pub(crate) fn khr(&self) -> &Path {
        &self.khr
    }

    pub(crate) fn subscribe(&self, channel: Channel<QueueEvent>) -> QueueSnapshot {
        *self.channel.lock() = Some(channel);
        let inner = self.inner.lock();
        QueueSnapshot {
            state: state(&inner),
            log: inner.log.clone(),
        }
    }

    fn publish(&self, event: QueueEvent) {
        self.channel.publish(event);
    }

    fn publish_state(&self) {
        let state = state(&self.inner.lock());
        self.publish(QueueEvent::State { state });
    }

    fn log(&self, line: impl Into<String>) {
        let line = line.into();
        {
            let mut inner = self.inner.lock();
            inner.log.push(line.clone());
            let excess = inner.log.len().saturating_sub(MAX_LINES);
            inner.log.drain(..excess);
        }
        self.publish(QueueEvent::Lines { lines: vec![line] });
    }

    /// Whether a project has steps running or queued.
    pub(crate) fn holds(&self, project: &Path) -> bool {
        let inner = self.inner.lock();
        inner
            .current
            .iter()
            .map(|(step, _, _)| step)
            .chain(inner.steps.iter())
            .any(|step| step.project.as_deref() == Some(project))
    }

    /// Adds steps after whatever is queued; starts at once when idle, with a
    /// clean log.
    pub(crate) fn push(&self, handle: &AppHandle<CefRuntime>, steps: Vec<Step>) {
        let idle = {
            let mut inner = self.inner.lock();
            let idle = inner.current.is_none() && inner.steps.is_empty();
            if idle {
                inner.log.clear();
                inner.failed = false;
            }
            inner.steps.extend(steps);
            idle
        };
        if idle {
            self.publish(QueueEvent::Cleared);
            start_next(handle.clone());
        } else {
            self.save();
            self.publish_state();
        }
    }

    pub(crate) fn resume(&self, handle: &AppHandle<CefRuntime>) {
        let steps: Vec<Step> = std::mem::take(&mut self.inner.lock().unfinished)
            .into_iter()
            .map(|mut step| {
                // Pipeline runs skip the pages they had already finished; the
                // step that was cut is repeated from there.
                if step.program == Program::Khr
                    && step.args.first().is_some_and(|arg| arg == "run")
                    && !step.args.iter().any(|arg| arg == "--pendientes")
                {
                    step.args.push("--pendientes".to_owned());
                }
                step
            })
            .collect();
        self.push(handle, steps);
        self.log("--- Reanudando la cola anterior ---");
    }

    pub(crate) fn discard_unfinished(&self) {
        let idle = {
            let mut inner = self.inner.lock();
            inner.unfinished.clear();
            inner.current.is_none() && inner.steps.is_empty()
        };
        if idle {
            let _ = std::fs::remove_file(QUEUE_FILE);
        }
        self.publish_state();
    }

    /// Drops every pending step of a project that has not started.
    pub(crate) fn cancel(&self, project: &str) {
        self.inner
            .lock()
            .steps
            .retain(|step| step.project_name().as_deref() != Some(project));
        self.save();
        self.log(format!("--- {project}: quitado de la cola ---"));
        self.publish_state();
    }

    pub(crate) fn set_shutdown_when_done(&self, shutdown: bool) {
        self.inner.lock().shutdown_when_done = shutdown;
        self.publish_state();
    }

    /// Empties the queue and ends the running step with whatever it spawned.
    pub(crate) fn stop(&self) {
        {
            let mut inner = self.inner.lock();
            inner.steps.clear();
            // Stopped on purpose: nothing to resume, and no shutdown either.
            inner.shutdown_when_done = false;
        }
        let _ = std::fs::remove_file(QUEUE_FILE);
        if self.kill_running() {
            self.log("!!! Detenido por el usuario.");
        }
        self.publish_state();
    }

    /// The app is closing: ends the running step but keeps the saved queue,
    /// to be offered for resuming on the next start.
    pub(crate) fn abandon(&self) {
        self.inner.lock().closing = true;
        self.kill_running();
    }

    /// taskkill /T also ends whatever the step spawned itself.
    fn kill_running(&self) -> bool {
        let Some(pid) = *self.pid.lock() else {
            return false;
        };
        let mut kill = std::process::Command::new("taskkill");
        kill.args(["/T", "/F", "/PID", &pid.to_string()]);
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut kill, CREATE_NO_WINDOW);
        let _ = kill.status();
        true
    }

    /// Writes the running step and the ones after it; nothing left removes
    /// the file.
    fn save(&self) {
        let inner = self.inner.lock();
        let steps: Vec<&Step> = inner
            .current
            .iter()
            .map(|(step, _, _)| step)
            .chain(inner.steps.iter())
            .collect();
        if steps.is_empty() {
            let _ = std::fs::remove_file(QUEUE_FILE);
        } else if let Ok(text) = serde_json::to_string_pretty(&steps) {
            let _ = std::fs::write(QUEUE_FILE, text);
        }
    }

    /// Takes one output line: a `@progreso <done> <total> <what>` line
    /// updates the progress, any other goes to the log.
    fn output(&self, line: &str) {
        let Some(progress) = parse_progress(line) else {
            self.log(line);
            return;
        };
        {
            let mut inner = self.inner.lock();
            let Some((_, _, current)) = inner.current.as_mut() else {
                return;
            };
            match current {
                // A new label (the next stage of a run) or a count that went
                // back starts the estimate over.
                Some(known) if known.what == progress.what && progress.done >= known.done => {
                    if known.first.is_none() && progress.done > known.done {
                        known.first = Some(ProgressStart {
                            at: now(),
                            done: progress.done,
                        });
                    }
                    known.done = progress.done;
                    known.total = progress.total;
                }
                _ => *current = Some(progress),
            }
        }
        self.publish_state();
    }
}

fn parse_progress(line: &str) -> Option<StepProgress> {
    let rest = line.trim().strip_prefix("@progreso ")?;
    let mut parts = rest.splitn(3, ' ');
    let done = parts.next()?.parse().ok()?;
    let total = parts.next()?.parse().ok()?;
    Some(StepProgress {
        what: parts.next().unwrap_or_default().trim().to_owned(),
        done,
        total,
        first: None,
    })
}

fn state(inner: &Inner) -> QueueState {
    let running = inner
        .current
        .as_ref()
        .map(|(step, started, progress)| RunningStep {
            name: step.name(),
            project: step.project_name(),
            started: *started,
            progress: progress.clone(),
        });
    let running_project = inner
        .current
        .as_ref()
        .and_then(|(step, _, _)| step.project_name());
    let mut waiting: Vec<String> = Vec::new();
    for name in inner.steps.iter().filter_map(Step::project_name) {
        if Some(&name) != running_project.as_ref() && !waiting.contains(&name) {
            waiting.push(name);
        }
    }
    let mut busy: Vec<String> = running_project.into_iter().collect();
    busy.extend(waiting.iter().cloned());
    let mut unfinished_projects: Vec<String> = Vec::new();
    for name in inner.unfinished.iter().filter_map(Step::project_name) {
        if !unfinished_projects.contains(&name) {
            unfinished_projects.push(name);
        }
    }
    QueueState {
        running,
        waiting,
        busy,
        unfinished: inner.unfinished.len(),
        unfinished_projects,
        failed: inner.failed,
        shutdown_when_done: inner.shutdown_when_done,
    }
}

/// Starts the next step, or wraps the queue up when none is left. Not async:
/// the step task calls it back when it ends.
fn start_next(handle: AppHandle<CefRuntime>) {
    let queue = handle.state::<Queue>();
    let next = {
        let mut inner = queue.inner.lock();
        let step = inner.steps.pop_front();
        if let Some(step) = &step {
            inner.current = Some((step.clone(), now(), None));
        }
        step
    };
    queue.save();
    queue.publish_state();
    match next {
        Some(step) => drop(tauri::async_runtime::spawn(run(handle.clone(), step))),
        None => {
            let (failed, shutdown) = {
                let mut inner = queue.inner.lock();
                (inner.failed, std::mem::take(&mut inner.shutdown_when_done))
            };
            queue.log(if failed {
                "=== Terminado con errores ==="
            } else {
                "=== Terminado ==="
            });
            if shutdown {
                shut_down(&queue);
            }
            queue.publish_state();
        }
    }
}

async fn run(handle: AppHandle<CefRuntime>, step: Step) {
    let queue = handle.state::<Queue>();
    queue.log("");
    queue.log(format!("=== {} ===", step.name()));
    if let Some(project) = &step.project {
        release_from_editor(&handle, project).await;
    }
    if step.program == Program::Khr {
        // khr loads its own copy of whatever the editor holds.
        if let Some(pipeline) = handle.try_state::<koharu_pipeline::Pipeline>() {
            pipeline.unload();
        }
    }
    let code = match execute(&handle, &step).await {
        Ok(code) => code,
        Err(error) => {
            queue.log(format!("!!! No se pudo lanzar {}: {error:#}", step.label));
            None
        }
    };
    *queue.pid.lock() = None;
    if queue.inner.lock().closing {
        return;
    }
    queue.inner.lock().current = None;

    if code != Some(0) && !step.ignore_error {
        let shown = code.map_or("detenido".to_owned(), |code| format!("código {code}"));
        queue.log(format!(
            "!!! '{}' falló ({shown}). Se cancelan los pasos restantes.",
            step.name()
        ));
        let mut inner = queue.inner.lock();
        inner.failed = true;
        inner.steps.clear();
        // Leave the card free even when a step fails.
        inner
            .steps
            .push_back(Step::khr(None, "Liberar VRAM", &["models", "unload"]).may_fail());
    } else if let Some(project) = &step.project {
        finish(&queue, project, step.after);
    }
    if let Some(project) = &step.project {
        queue.publish(QueueEvent::WorkChanged {
            project: project_name(project),
        });
        if !queue.holds(project) {
            return_to_editor(&handle, project).await;
        }
    }
    start_next(handle.clone());
}

async fn execute(handle: &AppHandle<CefRuntime>, step: &Step) -> Result<Option<i32>> {
    let queue = handle.state::<Queue>();
    let program = match step.program {
        Program::Khr => queue.khr.clone(),
        Program::LmStudio => queue.lms.clone(),
    };
    let mut command = tokio::process::Command::new(&program);
    command
        .args(&step.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .with_context(|| program.display().to_string())?;
    *queue.pid.lock() = child.id();
    let mut pumps = Vec::new();
    if let Some(out) = child.stdout.take() {
        pumps.push(tauri::async_runtime::spawn(pump(handle.clone(), out)));
    }
    if let Some(err) = child.stderr.take() {
        pumps.push(tauri::async_runtime::spawn(pump(handle.clone(), err)));
    }
    let status = child.wait().await?;
    for pump in pumps {
        let _ = pump.await;
    }
    Ok(status.code())
}

/// Forwards a pipe line by line. Progress bars rewrite their line with \r;
/// each state counts as a line.
async fn pump(handle: AppHandle<CefRuntime>, mut pipe: impl tokio::io::AsyncRead + Unpin) {
    let queue = handle.state::<Queue>();
    let mut buffer = [0_u8; 4096];
    let mut partial: Vec<u8> = Vec::new();
    while let Ok(read) = pipe.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        partial.extend_from_slice(&buffer[..read]);
        while let Some(end) = partial
            .iter()
            .position(|byte| matches!(byte, b'\n' | b'\r'))
        {
            let line: Vec<u8> = partial.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..end]);
            if !line.trim().is_empty() {
                queue.output(&line);
            }
        }
    }
    let rest = String::from_utf8_lossy(&partial);
    if !rest.trim().is_empty() {
        queue.output(&rest);
    }
}

fn finish(queue: &Queue, project: &Path, after: After) {
    let name = project_name(project);
    let dir = files::dir(project);
    match after {
        After::Nothing => {}
        After::ShowProposals => {
            let pending = files::pending_terms(&dir);
            if pending > 0 {
                queue.log(format!("[{name}] {pending} término(s) por aprobar."));
                queue.publish(QueueEvent::Finished {
                    project: name,
                    outcome: Outcome::Proposals,
                });
            }
        }
        After::ShowCorrections => {
            let pending = files::pending_corrections(&dir);
            if pending > 0 {
                queue.log(format!("[{name}] {pending} corrección(es) por revisar."));
                queue.publish(QueueEvent::Finished {
                    project: name,
                    outcome: Outcome::Corrections,
                });
            }
        }
        After::ApproveProposals => match files::approve_all_terms(&dir) {
            Ok(0) => {}
            Ok(count) => queue.log(format!(
                "{count} término(s) propuestos aprobados solos antes de traducir."
            )),
            Err(error) => queue.log(format!(
                "No se pudieron aprobar los términos propuestos: {error:#}"
            )),
        },
        After::Created => queue.publish(QueueEvent::Finished {
            project: name,
            outcome: Outcome::Created,
        }),
    }
}

/// Closes the project in the editor if it is open there, to reopen it once
/// its steps are done.
async fn release_from_editor(handle: &AppHandle<CefRuntime>, project: &Path) {
    let open = handle
        .state::<CurrentProject>()
        .project
        .lock()
        .await
        .as_ref()
        .is_some_and(|current| current.path == project);
    if !open {
        return;
    }
    let queue = handle.state::<Queue>();
    match close_current_project(handle).await {
        Ok(()) => {
            queue.inner.lock().reopen = Some(project.to_path_buf());
            queue.log("El proyecto se cierra en el editor mientras khr trabaja en él.");
        }
        Err(error) => queue.log(format!("!!! No se pudo cerrar el proyecto: {error:#}")),
    }
}

/// Reopens the project the queue closed, unless the user opened another.
async fn return_to_editor(handle: &AppHandle<CefRuntime>, project: &Path) {
    let queue = handle.state::<Queue>();
    let reopen = {
        let mut inner = queue.inner.lock();
        if inner.reopen.as_deref() != Some(project) {
            return;
        }
        inner.reopen.take()
    };
    let Some(project) = reopen else { return };
    let editor_free = handle
        .state::<CurrentProject>()
        .project
        .lock()
        .await
        .is_none();
    if !editor_free {
        return;
    }
    let library = handle.state::<ProjectLibrary>().inner().clone();
    let opened = match library.open(&project_name(&project)).await {
        Ok(opened) => opened,
        Err(error) => {
            queue.log(format!("!!! No se pudo reabrir el proyecto: {error:#}"));
            return;
        }
    };
    if let Err(error) = replace_project(handle, opened).await {
        queue.log(format!("!!! No se pudo reabrir el proyecto: {error:#}"));
    }
}

fn shut_down(queue: &Queue) {
    let mut command = std::process::Command::new("shutdown");
    command.args(["/s", "/t", SHUTDOWN_DELAY, "/c", "Koharu terminó la cola."]);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, CREATE_NO_WINDOW);
    match command.status() {
        Ok(_) => queue.log(format!(
            "El PC se apaga en {SHUTDOWN_DELAY} s; para cancelarlo: shutdown /a"
        )),
        Err(error) => queue.log(format!("!!! No se pudo apagar el PC: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_read() {
        let progress = parse_progress("@progreso 3 12 ocr").unwrap();
        assert_eq!((progress.done, progress.total), (3, 12));
        assert_eq!(progress.what, "ocr");
        assert!(parse_progress("cargando el modelo").is_none());
        assert!(parse_progress("@progreso tres 12 ocr").is_none());
    }

    #[test]
    fn steps_are_named_after_their_project() {
        let step = Step::khr(
            Some(Path::new(r"C:\p\Sakurami EN.khrproj")),
            "5. Traducir",
            &[],
        );
        assert_eq!(step.name(), "[Sakurami EN] 5. Traducir");
        assert_eq!(Step::khr(None, "Liberar VRAM", &[]).name(), "Liberar VRAM");
    }
}
