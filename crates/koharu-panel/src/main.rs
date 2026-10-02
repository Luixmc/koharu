//! Small desktop panel over `khr`: pick a project, tick the stages, run them
//! in order and watch the output. Projects can be queued one after another.
//! Every step is a child process, so the panel holds no models and a crash in
//! a stage never takes the window down.

#![windows_subsystem = "windows"]

use std::collections::VecDeque;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;

const PROJECTS_DIR: &str = r"I:\Usuario\Documentos\Koharu";
const WORKS_DIR: &str = r"I:\Koharu\obras";
/// Finished pages go to a folder named after the project in here.
const EXPORT_DIR: &str = r"I:\Koharu\output";
const GLOBAL_GLOSSARY: &str = r"I:\Koharu\glosario.tsv";
const DEFAULT_CORRECTOR: &str = "gemma-4-12b-it-qat";
/// Cydonia corrects Spanish better than Gemma (adverbs, word order), and
/// only step 6 needs it, so it takes the memory once Gemma is gone.
const DEFAULT_REVIEWER: &str = "thedrummer_cydonia-24b-v4.3";
/// The steps still to run, so a queue cut short (power cut, closed panel)
/// can be resumed; removed once the queue empties or the user stops it.
const QUEUE_FILE: &str = r"I:\Koharu\panel-cola.json";
/// Seconds Windows waits before shutting down, so `shutdown /a` can cancel it.
const SHUTDOWN_DELAY: &str = "120";
/// The model each LM Studio step last used, so the choice survives a restart.
const MODEL_CHOICES: &str = r"I:\Koharu\panel-modelos.json";
/// Source languages; khr picks the OCR that reads each one best.
const LANGUAGES: [(&str, &str); 4] = [
    ("ja", "Japonés"),
    ("ko", "Coreano"),
    ("zh", "Chino"),
    ("en", "Inglés"),
];
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_LINES: usize = 5000;

/// Koharu's own stages, run first while the card is free of the LLM.
const STAGES: [(&str, &str); 3] = [
    ("detection", "1. Detectar globos"),
    ("ocr", "2. Reconocer texto (OCR)"),
    ("inpainting", "3. Borrar texto original"),
];

/// What to do once a step exits successfully.
#[derive(serde::Serialize, serde::Deserialize)]
enum After {
    Nothing,
    /// Open the proposals of this project (if it is still the selected one).
    ShowProposals(PathBuf),
    /// Open the corrections of this project (if it is still the selected one).
    ShowCorrections(PathBuf),
    /// Approve the proposals of this work folder (study → translate).
    ApproveProposals(PathBuf),
    /// A project was just created: list it and select it.
    SelectProject(PathBuf),
}

/// One proposed correction from `khr revisar`.
#[derive(Clone)]
struct Correction {
    id: String,
    page: String,
    original: String,
    current: String,
    proposal: String,
    reason: String,
    /// Who the reviewer took to be speaking, and to whom.
    speaker: String,
}

/// One balloon of a reviewed page, from `revision-paginas.tsv`, to show the
/// proposals with the rest of their page around them.
struct Balloon {
    page: String,
    id: String,
    original: String,
    translation: String,
    speaker: String,
}

/// One glossary line: original, rendering and an optional note.
#[derive(Clone)]
struct Term {
    source: String,
    target: String,
    note: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Step {
    name: String,
    program: PathBuf,
    args: Vec<String>,
    ignore_error: bool,
    after: After,
}

impl Step {
    /// The project a planned step belongs to, from its "[name] step" label.
    fn project(&self) -> Option<&str> {
        self.name
            .strip_prefix('[')?
            .split_once("] ")
            .map(|(name, _)| name)
    }
}

enum Event {
    Output(String),
    Exited(Option<i32>),
    /// `khr etiquetas` finished: the gallery as JSON, or why it failed.
    Gallery(Result<String, String>),
}

/// How far the running step has got, from khr's `@progreso` lines.
struct StepProgress {
    what: String,
    done: usize,
    total: usize,
    /// When the count first went up, and to what; the pace since then
    /// estimates the time left, so loading the model before the first unit
    /// does not skew it.
    first: Option<(Instant, usize)>,
}

impl StepProgress {
    fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).min(1.0)
        }
    }

    fn remaining(&self) -> Option<Duration> {
        let (since, start) = self.first?;
        let measured = self.done.checked_sub(start).filter(|&units| units > 0)?;
        let left = self.total.saturating_sub(self.done);
        Some(since.elapsed().mul_f64(left as f64 / measured as f64))
    }
}

fn short_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min {:02} s", seconds / 60, seconds % 60),
        _ => format!("{} h {:02} min", seconds / 3600, seconds % 3600 / 60),
    }
}

struct Panel {
    khr: PathBuf,
    lms: PathBuf,
    koharu: PathBuf,
    projects: Vec<PathBuf>,
    selected: Option<usize>,
    stages: [bool; 3],
    study: bool,
    /// The user's tags (comma separated) and description of the work, given
    /// to `khr estudiar` as a starting point it compares with the text.
    tags: String,
    description: String,
    /// e-hentai gallery number or link to take tags from, and whether a
    /// fetch is under way.
    gallery: String,
    fetching_gallery: bool,
    translate: bool,
    review_step: bool,
    llm_translation: bool,
    corrector: String,
    reviewer: String,
    /// Model for the study (step 4); the reviewer's by default.
    study_model: String,
    /// LLMs installed in LM Studio, for the model menus.
    models: Vec<String>,
    pages: u32,
    left_to_right: bool,
    idioma: usize,
    queue: VecDeque<Step>,
    queued_projects: Vec<String>,
    /// A queue left unfinished by a previous run, offered for resuming.
    unfinished: Vec<Step>,
    /// Shut Windows down once the queue empties.
    shutdown_when_done: bool,
    current: Option<Step>,
    /// When the running step started, and its progress if khr reports any.
    step_started: Instant,
    progress: Option<StepProgress>,
    failed: bool,
    child: Arc<Mutex<Option<Child>>>,
    events: (Sender<Event>, Receiver<Event>),
    lines: Vec<String>,
    partial: String,
    status: String,
    glossary_open: bool,
    proposals: Vec<Term>,
    approved: usize,
    /// This work's approved terms, to copy into the global glossary.
    work_terms: Vec<Term>,
    corrections_open: bool,
    corrections: Vec<Correction>,
    approved_corrections: usize,
    page_balloons: Vec<Balloon>,
    /// Correction being turned into a glossary term: (original, rendering).
    new_term: Option<(String, String)>,
}

impl Panel {
    fn new() -> Self {
        let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
        let local = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default());
        // khr lives next to the panel in the installed copy; fall back to the
        // fixed location so a build run from target/ still finds it.
        let khr = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("khr.exe")))
            .filter(|path| path.exists())
            .unwrap_or_else(|| PathBuf::from(r"I:\Koharu\bin\khr.exe"));
        let mut panel = Self {
            khr,
            lms: home.join(r".lmstudio\bin\lms.exe"),
            koharu: local.join(r"koharu\koharu.exe"),
            projects: Vec::new(),
            selected: None,
            stages: [true; 3],
            study: true,
            tags: String::new(),
            description: String::new(),
            gallery: String::new(),
            fetching_gallery: false,
            translate: true,
            review_step: true,
            llm_translation: true,
            corrector: DEFAULT_CORRECTOR.to_owned(),
            reviewer: DEFAULT_REVIEWER.to_owned(),
            study_model: DEFAULT_REVIEWER.to_owned(),
            models: Vec::new(),
            pages: 0,
            left_to_right: false,
            idioma: 0,
            queue: VecDeque::new(),
            queued_projects: Vec::new(),
            unfinished: load_queue(),
            shutdown_when_done: false,
            current: None,
            step_started: Instant::now(),
            progress: None,
            failed: false,
            child: Arc::new(Mutex::new(None)),
            events: channel(),
            lines: Vec::new(),
            partial: String::new(),
            status: "Listo. El proyecto se crea en Koharu (importar imágenes); aquí se procesa."
                .to_owned(),
            glossary_open: false,
            proposals: Vec::new(),
            approved: 0,
            work_terms: Vec::new(),
            corrections_open: false,
            corrections: Vec::new(),
            approved_corrections: 0,
            page_balloons: Vec::new(),
            new_term: None,
        };
        panel.models = installed_models(&panel.lms);
        panel.load_model_choices();
        panel.reload_projects();
        panel
    }

    fn load_model_choices(&mut self) {
        let Some(saved) = std::fs::read_to_string(MODEL_CHOICES)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        else {
            return;
        };
        let text = |key: &str| {
            saved
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        if let Some(model) = text("estudiar") {
            self.study_model = model;
        }
        if let Some(model) = text("traducir") {
            self.corrector = model;
        }
        if let Some(model) = text("revisar") {
            self.reviewer = model;
        }
        if let Some(local) = saved
            .get("traducir_local")
            .and_then(|value| value.as_bool())
        {
            self.llm_translation = local;
        }
    }

    fn save_model_choices(&self) {
        let choices = serde_json::json!({
            "estudiar": self.study_model,
            "traducir": self.corrector,
            "traducir_local": self.llm_translation,
            "revisar": self.reviewer,
        });
        let _ = std::fs::write(
            MODEL_CHOICES,
            serde_json::to_string_pretty(&choices).unwrap_or_default(),
        );
    }

    fn reload_projects(&mut self) {
        let mut found: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(PROJECTS_DIR)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "khrproj"))
            .map(|path| (modified(&path), path))
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        let previous = self.project().map(Path::to_path_buf);
        self.projects = found.into_iter().map(|(_, path)| path).collect();
        self.selected = previous
            .and_then(|prev| self.projects.iter().position(|path| *path == prev))
            .or(if self.projects.is_empty() {
                None
            } else {
                Some(0)
            });
        self.project_changed();
    }

    /// Everything shown about a project (terms, corrections, the language)
    /// belongs to it; a switch reloads it all instead of leaving the previous
    /// project's lists on screen.
    fn project_changed(&mut self) {
        self.guess_language();
        self.new_term = None;
        self.load_glossary();
        self.load_corrections();
        self.load_user_notes();
        // While a step runs the status line belongs to it, not to the selection.
        if self.current.is_none() {
            self.status = self
                .project()
                .map(|project| format!("Proyecto: {}", project_name(project)))
                .unwrap_or_default();
        }
    }

    /// Projects named "... JA", "... KO", "... ZH" or "... EN" set the source
    /// language themselves.
    fn guess_language(&mut self) {
        let Some(stem) = self.project().and_then(Path::file_stem) else {
            return;
        };
        let stem = stem.to_string_lossy().to_lowercase();
        if let Some(index) = LANGUAGES.iter().position(|(code, _)| {
            stem.ends_with(&format!(" {code}")) || stem.ends_with(&format!("-{code}"))
        }) {
            self.idioma = index;
        }
    }

    fn project(&self) -> Option<&Path> {
        self.selected
            .and_then(|index| self.projects.get(index))
            .map(PathBuf::as_path)
    }

    fn log(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
        if self.lines.len() > MAX_LINES {
            self.lines.drain(..self.lines.len() - MAX_LINES);
        }
    }

    fn push(
        &mut self,
        name: &str,
        program: &Path,
        args: &[&str],
        ignore_error: bool,
        after: After,
    ) {
        self.queue.push_back(Step {
            name: name.to_owned(),
            program: program.to_path_buf(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            ignore_error,
            after,
        });
    }

    /// Clears the output and starts a fresh sequence of queued steps.
    fn begin(&mut self, ctx: &egui::Context) {
        self.failed = false;
        self.start_next(ctx);
    }

    fn start_next(&mut self, ctx: &egui::Context) {
        let Some(step) = self.queue.pop_front() else {
            if self.failed {
                self.log("=== Terminado con errores ===");
            } else {
                self.status = "Terminado.".to_owned();
                self.log("=== Terminado ===");
            }
            self.save_queue();
            if std::mem::take(&mut self.shutdown_when_done) {
                self.shut_down();
            }
            return;
        };
        self.log("");
        self.log(format!("=== {} ===", step.name));
        if !self.failed {
            self.status = format!("{}...", step.name);
        }

        let spawned = Command::new(&step.program)
            .args(&step.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.log(format!(
                    "!!! No se pudo lanzar {}: {error}",
                    step.program.display()
                ));
                self.status = format!("Error en: {}", step.name);
                self.failed = true;
                self.queue.clear();
                return;
            }
        };

        let readers: Vec<Box<dyn Read + Send>> = vec![
            Box::new(child.stdout.take().expect("piped stdout")),
            Box::new(child.stderr.take().expect("piped stderr")),
        ];
        let pipes: Vec<_> = readers
            .into_iter()
            .map(|mut reader| {
                let tx = self.events.0.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = reader.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let _ = tx.send(Event::Output(chunk));
                        ctx.request_repaint();
                    }
                })
            })
            .collect();

        *self.child.lock().unwrap() = Some(child);
        let slot = self.child.clone();
        let tx = self.events.0.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for pipe in pipes {
                let _ = pipe.join();
            }
            let code = slot
                .lock()
                .unwrap()
                .take()
                .and_then(|mut child| child.wait().ok())
                .and_then(|status| status.code());
            let _ = tx.send(Event::Exited(code));
            ctx.request_repaint();
        });
        self.current = Some(step);
        self.step_started = Instant::now();
        self.progress = None;
        self.save_queue();
    }

    /// Writes the running step and the ones after it; nothing left removes
    /// the file.
    fn save_queue(&self) {
        let steps: Vec<&Step> = self.current.iter().chain(self.queue.iter()).collect();
        if steps.is_empty() {
            let _ = std::fs::remove_file(QUEUE_FILE);
            return;
        }
        if let Ok(text) = serde_json::to_string_pretty(&steps) {
            let _ = std::fs::write(QUEUE_FILE, text);
        }
    }

    /// Queues the steps a previous run left unfinished. Pipeline runs skip
    /// the pages they had already finished; the step that was cut is
    /// repeated from there.
    fn resume(&mut self, ctx: &egui::Context) {
        if !confirm_koharu_closed() {
            return;
        }
        let idle = self.current.is_none() && self.queue.is_empty();
        if idle {
            self.lines.clear();
            self.queued_projects.clear();
        }
        for mut step in std::mem::take(&mut self.unfinished) {
            if step.args.first().is_some_and(|arg| arg == "run")
                && !step.args.iter().any(|arg| arg == "--pendientes")
            {
                step.args.push("--pendientes".to_owned());
            }
            if let Some(name) = step.project().map(str::to_owned)
                && !self.queued_projects.contains(&name)
            {
                self.queued_projects.push(name);
            }
            self.queue.push_back(step);
        }
        self.log("--- Reanudando la cola anterior ---");
        if idle {
            self.begin(ctx);
        } else {
            self.save_queue();
        }
    }

    /// A new project named after `folder`, with its images as pages.
    fn create_project(&mut self, ctx: &egui::Context, folder: &Path) {
        let name = folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let project = Path::new(PROJECTS_DIR).join(format!("{name}.khrproj"));
        if project.exists() {
            self.status = format!("Ya existe un proyecto llamado {name}.");
            return;
        }
        let khr = self.khr.clone();
        let (project_arg, folder_arg) = (
            project.to_string_lossy().into_owned(),
            folder.to_string_lossy().into_owned(),
        );
        self.lines.clear();
        self.push(
            &format!("[{name}] Crear el proyecto"),
            &khr,
            &[
                "crear",
                "--project",
                &project_arg,
                "--imagenes",
                &folder_arg,
            ],
            false,
            After::SelectProject(project),
        );
        self.begin(ctx);
    }

    fn shut_down(&mut self) {
        let started = Command::new("shutdown")
            .args(["/s", "/t", SHUTDOWN_DELAY, "/c", "Koharu terminó la cola."])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        match started {
            Ok(_) => {
                self.status =
                    format!("El PC se apaga en {SHUTDOWN_DELAY} s; para cancelarlo: shutdown /a");
                self.log(self.status.clone());
            }
            Err(error) => self.log(format!("!!! No se pudo apagar el PC: {error}")),
        }
    }

    /// Projects in the queue, in order, that have not started yet.
    fn waiting_projects(&self) -> Vec<String> {
        let running = self.current.as_ref().and_then(Step::project);
        let mut waiting: Vec<String> = Vec::new();
        for name in self.queue.iter().filter_map(Step::project) {
            if Some(name) != running && !waiting.iter().any(|seen| seen == name) {
                waiting.push(name.to_owned());
            }
        }
        waiting
    }

    /// Drops every pending step of a project that has not started.
    fn cancel_queued(&mut self, name: &str) {
        self.queue.retain(|step| step.project() != Some(name));
        self.queued_projects.retain(|queued| queued != name);
        self.save_queue();
        self.log(format!("--- {name}: quitado de la cola ---"));
        self.status = format!("Quitado de la cola: {name}");
    }

    /// Takes a `@progreso <done> <total> <what>` line; returns false for any
    /// other line, which belongs in the log.
    fn read_progress(&mut self, line: &str) -> bool {
        let Some(rest) = line.trim().strip_prefix("@progreso ") else {
            return false;
        };
        let mut parts = rest.splitn(3, ' ');
        let (Some(Ok(done)), Some(Ok(total))) = (
            parts.next().map(str::parse::<usize>),
            parts.next().map(str::parse::<usize>),
        ) else {
            return false;
        };
        let what = parts.next().unwrap_or_default().trim().to_owned();
        let now = Instant::now();
        match &mut self.progress {
            // A new label (the next stage of a run) or a count that went back
            // starts the estimate over.
            Some(progress) if progress.what == what && done >= progress.done => {
                if progress.first.is_none() && done > progress.done {
                    progress.first = Some((now, done));
                }
                progress.done = done;
                progress.total = total;
            }
            _ => {
                self.progress = Some(StepProgress {
                    what,
                    done,
                    total,
                    first: None,
                });
            }
        }
        true
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.1.try_recv() {
            match event {
                Event::Output(chunk) => {
                    self.partial.push_str(&chunk);
                    // Progress bars rewrite their line with \r; show each state.
                    while let Some(end) = self.partial.find(['\n', '\r']) {
                        let line: String = self.partial.drain(..=end).collect();
                        let line = line.trim_end_matches(['\n', '\r']);
                        if !line.is_empty() && !self.read_progress(line) {
                            self.log(line.to_owned());
                        }
                    }
                }
                Event::Gallery(result) => self.apply_gallery(result),
                Event::Exited(code) => {
                    if !self.partial.is_empty() {
                        let rest = std::mem::take(&mut self.partial);
                        self.log(rest);
                    }
                    self.progress = None;
                    let Some(step) = self.current.take() else {
                        continue;
                    };
                    if code != Some(0) && !step.ignore_error {
                        let shown = code.map_or("detenido".to_owned(), |c| format!("código {c}"));
                        self.log(format!(
                            "!!! '{}' falló ({shown}). Se cancelan los pasos restantes.",
                            step.name
                        ));
                        self.status = format!("Error en: {}", step.name);
                        self.failed = true;
                        self.queue.clear();
                        // Leave the card free even when correction fails.
                        let khr = self.khr.clone();
                        self.push(
                            "Liberar VRAM",
                            &khr,
                            &["models", "unload"],
                            true,
                            After::Nothing,
                        );
                    } else {
                        self.finish(step.after);
                    }
                    self.start_next(ctx);
                }
            }
        }
    }

    fn finish(&mut self, after: After) {
        match after {
            After::Nothing => {}
            After::ShowProposals(project) => {
                if self.project() == Some(project.as_path()) {
                    self.load_glossary();
                    self.glossary_open = !self.proposals.is_empty();
                } else {
                    let dir = Path::new(WORKS_DIR).join(log_base(&project));
                    let pending = read_terms(&dir.join("propuestas.tsv")).len();
                    if pending > 0 {
                        self.log(format!(
                            "[{}] {pending} término(s) por aprobar: elige ese proyecto para verlos.",
                            project_name(&project)
                        ));
                    }
                }
            }
            After::ShowCorrections(project) => {
                if self.project() == Some(project.as_path()) {
                    self.load_corrections();
                    self.corrections_open = !self.corrections.is_empty();
                } else {
                    let dir = Path::new(WORKS_DIR).join(log_base(&project));
                    let pending = read_corrections(&dir.join("correcciones.tsv")).len();
                    if pending > 0 {
                        self.log(format!(
                            "[{}] {pending} corrección(es) por revisar: elige ese proyecto para verlas.",
                            project_name(&project)
                        ));
                    }
                }
            }
            After::ApproveProposals(dir) => self.approve_all_in(&dir),
            After::SelectProject(project) => {
                self.reload_projects();
                if let Some(index) = self.projects.iter().position(|path| *path == project) {
                    self.selected = Some(index);
                    self.project_changed();
                }
            }
        }
    }

    /// Runs the ticked steps on the selected project now.
    fn run(&mut self, ctx: &egui::Context) {
        if !confirm_koharu_closed() {
            return;
        }
        self.lines.clear();
        self.queued_projects.clear();
        if self.plan() {
            self.begin(ctx);
        }
    }

    /// Adds the selected project, with the steps and options as they are now,
    /// after whatever is queued; starts at once when nothing is running.
    fn enqueue(&mut self, ctx: &egui::Context) {
        let idle = self.current.is_none() && self.queue.is_empty();
        if idle && !confirm_koharu_closed() {
            return;
        }
        if idle {
            self.lines.clear();
            self.queued_projects.clear();
        }
        if self.plan() {
            if idle {
                self.begin(ctx);
            } else {
                self.save_queue();
            }
        }
    }

    /// Queues the ticked steps for the selected project. Koharu's own models
    /// run first, then one LM Studio load covers studying, translating and
    /// reviewing.
    fn plan(&mut self) -> bool {
        let Some(project) = self.project().map(Path::to_path_buf) else {
            return false;
        };
        let stages: Vec<(&str, &str)> = STAGES
            .iter()
            .zip(self.stages)
            .filter(|(_, on)| *on)
            .map(|(stage, _)| *stage)
            .collect();
        if stages.is_empty() && !self.study && !self.translate && !self.review_step {
            self.status = "Marca al menos un paso.".to_owned();
            return false;
        }
        let model = self.corrector.trim().to_owned();
        let reviewer = self.reviewer.trim().to_owned();
        let study_model = self.study_model.trim().to_owned();
        let llm_translation = self.translate && self.llm_translation;
        if llm_translation && model.is_empty() {
            self.status = "Elige el modelo de traducción.".to_owned();
            return false;
        }
        if self.study && study_model.is_empty() {
            self.status = "Elige el modelo de la ficha.".to_owned();
            return false;
        }
        if self.review_step && reviewer.is_empty() {
            self.status = "Elige el modelo de revisión.".to_owned();
            return false;
        }
        let name = project
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.queued_projects.push(name.clone());
        let khr = self.khr.clone();
        let project_arg = project.display().to_string();
        let pages = self.pages.to_string();
        let page_args: Vec<&str> = if self.pages > 0 {
            vec!["--pages", &pages]
        } else {
            Vec::new()
        };
        let idioma = LANGUAGES[self.idioma].0;
        let label = |step: &str| format!("[{name}] {step}");

        if !stages.is_empty() {
            self.push(
                &label("Liberar VRAM"),
                &khr,
                &["models", "unload"],
                true,
                After::Nothing,
            );
            let joined = stages
                .iter()
                .map(|(key, _)| *key)
                .collect::<Vec<_>>()
                .join(",");
            let mut args = vec![
                "run",
                "--project",
                &project_arg,
                "--stages",
                &joined,
                "--idioma",
                idioma,
            ];
            args.extend(&page_args);
            let shown = stages
                .iter()
                .map(|(_, text)| *text)
                .collect::<Vec<_>>()
                .join(" · ");
            self.push(&label(&shown), &khr, &args, false, After::Nothing);
        }
        if self.translate && !self.llm_translation {
            let mut args = vec!["run", "--project", &project_arg, "--stages", "translation"];
            args.extend(&page_args);
            self.push(
                &label("5. Traducir con DeepL"),
                &khr,
                &args,
                false,
                After::Nothing,
            );
        }
        let uses_llm = self.study || llm_translation || self.review_step;
        if uses_llm {
            let lms = self.lms.clone();
            self.push(
                &label("Iniciar servidor de LM Studio"),
                &lms,
                &["server", "start"],
                true,
                After::Nothing,
            );
        }
        // Two models do not fit in 16 GB of RAM: each change frees the other.
        let mut loaded: Option<String> = None;
        if self.study {
            // A gallery link left in the field is enough: its tags are
            // fetched into usuario.json right before the study reads it.
            let gallery = self.gallery.trim().to_owned();
            if !gallery.is_empty() {
                let args = [
                    "etiquetas",
                    "--galeria",
                    &gallery,
                    "--project",
                    &project_arg,
                ];
                self.push(
                    &label("Traer etiquetas de la galería"),
                    &khr,
                    &args,
                    true,
                    After::Nothing,
                );
            }
            self.switch_model(&mut loaded, &study_model, &label);
            if let Err(error) = self.save_user_notes() {
                self.log(format!(
                    "No se guardaron tus etiquetas y descripción: {error}"
                ));
            }
            let mut args = vec![
                "estudiar",
                "--project",
                &project_arg,
                "--model",
                &study_model,
            ];
            if self.left_to_right {
                args.push("--left-to-right");
            }
            // Translating right after studying: the new terms go in approved.
            let after = if self.translate {
                After::ApproveProposals(Path::new(WORKS_DIR).join(log_base(&project)))
            } else {
                After::Nothing
            };
            self.push(
                &label("4. Estudiar la obra (ficha y términos)"),
                &khr,
                &args,
                false,
                after,
            );
        }
        if llm_translation {
            self.switch_model(&mut loaded, &model, &label);
            let mut args = vec![
                "run",
                "--project",
                &project_arg,
                "--stages",
                "translation",
                "--translator",
                &model,
                "--without-pages",
            ];
            args.extend(&page_args);
            self.push(
                &label(&format!("5. Traducir con {model}")),
                &khr,
                &args,
                false,
                After::Nothing,
            );
        }
        if self.review_step {
            self.switch_model(&mut loaded, &reviewer, &label);
            let mut args = vec!["revisar", "--project", &project_arg, "--model", &reviewer];
            args.extend(&page_args);
            if self.left_to_right {
                args.push("--left-to-right");
            }
            self.push(
                &label(&format!("6. Revisar con {reviewer}")),
                &khr,
                &args,
                false,
                After::Nothing,
            );
        }
        if let Some(loaded) = loaded {
            let after = if self.review_step {
                After::ShowCorrections(project.clone())
            } else if self.study {
                After::ShowProposals(project.clone())
            } else {
                After::Nothing
            };
            self.push(
                &label("Liberar modelo de LM Studio"),
                &khr,
                &["models", "unload", &loaded],
                true,
                after,
            );
        }
        self.status = format!("En cola: {}", self.queued_projects.join(", "));
        true
    }

    /// Queues loading `model`, freeing the one loaded before if it differs.
    fn switch_model(
        &mut self,
        loaded: &mut Option<String>,
        model: &str,
        label: &dyn Fn(&str) -> String,
    ) {
        if loaded.as_deref() == Some(model) {
            return;
        }
        let khr = self.khr.clone();
        if let Some(previous) = loaded.take() {
            self.push(
                &label("Liberar modelo de LM Studio"),
                &khr,
                &["models", "unload", &previous],
                true,
                After::Nothing,
            );
        }
        self.push(
            &label(&format!("Cargar {model}")),
            &khr,
            &["models", "load", model],
            false,
            After::Nothing,
        );
        *loaded = Some(model.to_owned());
    }

    fn learn(&mut self, ctx: &egui::Context) {
        let Some(project) = self.project().map(Path::to_path_buf) else {
            return;
        };
        let model = self.corrector.trim().to_owned();
        if model.is_empty() {
            self.status = "Escribe el modelo local.".to_owned();
            return;
        }
        self.lines.clear();
        self.log(format!("Proyecto: {}", project.display()));
        let khr = self.khr.clone();
        let lms = self.lms.clone();
        let project_arg = project.display().to_string();
        let name = project_name(&project);
        let label = |step: &str| format!("[{name}] {step}");
        self.push(
            &label("Iniciar servidor de LM Studio"),
            &lms,
            &["server", "start"],
            true,
            After::Nothing,
        );
        self.push(
            &label(&format!("Cargar {model}")),
            &khr,
            &["models", "load", &model],
            false,
            After::Nothing,
        );
        self.push(
            &label("Aprender de mis correcciones"),
            &khr,
            &["aprender", "--project", &project_arg, "--model", &model],
            false,
            After::Nothing,
        );
        self.push(
            &label("Liberar modelo de LM Studio"),
            &khr,
            &["models", "unload", &model],
            true,
            After::ShowProposals(project.clone()),
        );
        self.begin(ctx);
    }

    fn work_dir(&self) -> Option<PathBuf> {
        self.project()
            .map(|project| Path::new(WORKS_DIR).join(log_base(project)))
    }

    fn load_user_notes(&mut self) {
        let notes = self
            .work_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join("usuario.json")).ok())
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .unwrap_or_default();
        self.tags = notes["etiquetas"]
            .as_array()
            .map(|tags| {
                tags.iter()
                    .filter_map(|tag| tag.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        self.description = notes["descripcion"].as_str().unwrap_or_default().to_owned();
        self.gallery.clear();
    }

    /// Writes the tags and description for `khr estudiar`; empty fields
    /// remove the file so the study starts from the text alone.
    fn save_user_notes(&self) -> std::io::Result<()> {
        let Some(dir) = self.work_dir() else {
            return Ok(());
        };
        let path = dir.join("usuario.json");
        let tags: Vec<&str> = self
            .tags
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .collect();
        if tags.is_empty() && self.description.trim().is_empty() {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        std::fs::create_dir_all(&dir)?;
        let notes = serde_json::json!({
            "etiquetas": tags,
            "descripcion": self.description.trim(),
        });
        std::fs::write(
            path,
            serde_json::to_string_pretty(&notes).unwrap_or_default(),
        )
    }

    /// Asks `khr etiquetas` for the gallery's tags without blocking the window.
    fn fetch_gallery(&mut self, ctx: &egui::Context) {
        let input = self.gallery.trim().to_owned();
        if input.is_empty() {
            return;
        }
        self.fetching_gallery = true;
        self.status = format!("Buscando la galería {input} en e-hentai...");
        let khr = self.khr.clone();
        let tx = self.events.0.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = Command::new(khr)
                .args(["etiquetas", "--galeria", &input])
                .stdin(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|error| error.to_string())
                .and_then(|output| {
                    if output.status.success() {
                        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
                    } else {
                        let error = String::from_utf8_lossy(&output.stderr);
                        Err(error
                            .lines()
                            .last()
                            .unwrap_or("error desconocido")
                            .to_owned())
                    }
                });
            let _ = tx.send(Event::Gallery(result));
            ctx.request_repaint();
        });
    }

    /// Adds the fetched tags to the field, and the title when the
    /// description is still empty.
    fn apply_gallery(&mut self, result: Result<String, String>) {
        self.fetching_gallery = false;
        let gallery = match result.and_then(|json| {
            serde_json::from_str::<serde_json::Value>(&json).map_err(|error| error.to_string())
        }) {
            Ok(gallery) => gallery,
            Err(error) => {
                self.status = format!("No se pudieron traer las etiquetas: {error}");
                return;
            }
        };
        let mut tags: Vec<String> = self
            .tags
            .split([',', ';', '\n'])
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect();
        let before = tags.len();
        for tag in gallery["etiquetas"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tag| tag.as_str())
        {
            if !tags.iter().any(|known| known == tag) {
                tags.push(tag.to_owned());
            }
        }
        self.tags = tags.join(", ");
        if self.description.trim().is_empty() {
            let title = [&gallery["titulo_original"], &gallery["titulo"]]
                .into_iter()
                .filter_map(|title| title.as_str())
                .find(|title| !title.is_empty());
            if let Some(title) = title {
                self.description = format!("Título: {title}");
            }
        }
        self.status = format!("{} etiqueta(s) nuevas de e-hentai.", tags.len() - before);
    }

    fn load_glossary(&mut self) {
        let Some(dir) = self.work_dir() else { return };
        self.proposals = read_terms(&dir.join("propuestas.tsv"));
        self.work_terms = read_terms(&dir.join("glosario.tsv"));
        self.approved = self.work_terms.len();
    }

    /// Moves proposal `index` to the work's glossary or to its rejected list.
    fn decide(&mut self, index: usize, approve: bool) {
        let Some(dir) = self.work_dir() else { return };
        if index >= self.proposals.len() {
            return;
        }
        let term = self.proposals.remove(index);
        match move_terms(&dir, vec![term], approve, &self.proposals) {
            Ok(kept) if approve => self.approved = kept,
            Ok(_) => {}
            Err(error) => self.status = format!("No se pudo guardar el glosario: {error}"),
        }
    }

    /// Approves every pending proposal of the work in `dir`, used when
    /// studying is followed by translating so the new terms are in force.
    fn approve_all_in(&mut self, dir: &Path) {
        let pending = read_terms(&dir.join("propuestas.tsv"));
        if pending.is_empty() {
            return;
        }
        let count = pending.len();
        match move_terms(dir, pending, true, &[]) {
            Ok(_) => self.log(format!(
                "{count} término(s) propuestos aprobados solos antes de traducir."
            )),
            Err(error) => self.log(format!(
                "No se pudieron aprobar los términos propuestos: {error}"
            )),
        }
        if self.work_dir().as_deref() == Some(dir) {
            self.load_glossary();
        }
    }

    /// Copies one of this work's terms into the global glossary, replacing an
    /// entry with the same original there. The work keeps its copy, which
    /// still wins inside this work.
    fn promote(&mut self, index: usize) {
        let Some(term) = self.work_terms.get(index).cloned() else {
            return;
        };
        let global = Path::new(GLOBAL_GLOSSARY);
        let mut terms = read_terms(global);
        terms.retain(|existing| existing.source.to_lowercase() != term.source.to_lowercase());
        terms.push(term.clone());
        // Keep the file's own explanatory header.
        let header: String = std::fs::read_to_string(global)
            .unwrap_or_default()
            .lines()
            .take_while(|line| line.trim_start().starts_with('#'))
            .map(|line| format!("{line}\n"))
            .collect();
        let header = if header.is_empty() {
            "# Glosario global: original<TAB>traducción<TAB>nota. Vale para todas las obras.\n"
                .to_owned()
        } else {
            header
        };
        self.status = match write_terms(global, &header, &terms) {
            Ok(()) => format!(
                "\"{}\" → \"{}\" añadido al glosario global.",
                term.source, term.target
            ),
            Err(error) => format!("No se pudo guardar el glosario global: {error}"),
        };
    }

    fn open_file(&mut self, path: PathBuf) {
        if !path.exists() {
            self.status = format!("Aún no existe: {}", path.display());
            return;
        }
        if let Err(error) = Command::new("notepad").arg(&path).spawn() {
            self.status = format!("No se pudo abrir {}: {error}", path.display());
        }
    }

    fn glossary_window(&mut self, ctx: &egui::Context) {
        let mut open = self.glossary_open;
        let mut decision: Option<(usize, bool)> = None;
        let mut all: Option<bool> = None;
        let mut file: Option<PathBuf> = None;
        let mut promote: Option<usize> = None;
        let dir = self.work_dir();
        egui::Window::new("Glosario y ficha de la obra")
            .open(&mut open)
            .default_size([640.0, 420.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} término(s) aprobados en esta obra; {} propuesta(s) pendientes.",
                    self.approved,
                    self.proposals.len()
                ));
                ui.horizontal(|ui| {
                    if let Some(dir) = &dir {
                        if ui.button("Abrir ficha").clicked() {
                            file = Some(dir.join("ficha.md"));
                        }
                        if ui.button("Abrir glosario de la obra").clicked() {
                            file = Some(dir.join("glosario.tsv"));
                        }
                    }
                    if ui.button("Abrir glosario global").clicked() {
                        file = Some(PathBuf::from(GLOBAL_GLOSSARY));
                    }
                });
                if !self.work_terms.is_empty() {
                    ui.separator();
                    egui::CollapsingHeader::new(format!(
                        "Términos de esta obra ({}): pásalos al glosario global si sirven para otras",
                        self.work_terms.len()
                    ))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical().id_salt("work_terms").max_height(180.0).show(ui, |ui| {
                            egui::Grid::new("work_terms_grid").striped(true).show(ui, |ui| {
                                for (index, term) in self.work_terms.iter().enumerate() {
                                    ui.label(&term.source);
                                    ui.label(&term.target);
                                    if ui.button("→ Global").clicked() {
                                        promote = Some(index);
                                    }
                                    ui.end_row();
                                }
                            });
                        });
                    });
                }
                ui.separator();
                if self.proposals.is_empty() {
                    ui.label("No hay propuestas. Usa \"Estudiar la obra\" o \"Aprender de mis correcciones\".");
                    return;
                }
                ui.horizontal(|ui| {
                    if ui.button("Aprobar todas").clicked() {
                        all = Some(true);
                    }
                    if ui.button("Rechazar todas").clicked() {
                        all = Some(false);
                    }
                });
                ui.label("Puedes corregir la traducción antes de aprobarla.");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("proposals").striped(true).show(ui, |ui| {
                        for (index, term) in self.proposals.iter_mut().enumerate() {
                            ui.label(&term.source);
                            ui.add(egui::TextEdit::singleline(&mut term.target).desired_width(180.0));
                            ui.label(egui::RichText::new(&term.note).weak());
                            if ui.button("Aprobar").clicked() {
                                decision = Some((index, true));
                            }
                            if ui.button("Rechazar").clicked() {
                                decision = Some((index, false));
                            }
                            ui.end_row();
                        }
                    });
                });
            });
        self.glossary_open = open;
        if let Some(index) = promote {
            self.promote(index);
        }
        if let Some((index, approve)) = decision {
            self.decide(index, approve);
            self.work_terms = self
                .work_dir()
                .map(|dir| read_terms(&dir.join("glosario.tsv")))
                .unwrap_or_default();
        }
        if let Some(approve) = all {
            while !self.proposals.is_empty() {
                self.decide(0, approve);
            }
        }
        if let Some(path) = file {
            self.open_file(path);
        }
    }

    fn load_corrections(&mut self) {
        let Some(dir) = self.work_dir() else { return };
        self.corrections = read_corrections(&dir.join("correcciones.tsv"));
        self.page_balloons = read_balloons(&dir.join("revision-paginas.tsv"));
        self.approved_corrections = read_lines(&dir.join("correcciones-aprobadas.tsv")).len();
    }

    /// Moves correction `index` to the approved or the rejected list and
    /// rewrites the pending one. Approved text reaches the project only when
    /// the user applies the list, with Koharu closed.
    fn decide_correction(&mut self, index: usize, approve: bool) {
        let Some(dir) = self.work_dir() else { return };
        if index >= self.corrections.len() {
            return;
        }
        let correction = self.corrections.remove(index);
        let (file, header, line) = if approve {
            (
                "correcciones-aprobadas.tsv",
                "# Correcciones aprobadas pendientes de aplicar: id\tpropuesta\n",
                format!(
                    "{}\t{}",
                    correction.id,
                    correction.proposal.replace('\t', " ")
                ),
            )
        } else {
            (
                "correcciones-rechazadas.tsv",
                "# Correcciones rechazadas: id\ttraducción que se dejó\tpropuesta\n",
                format!(
                    "{}\t{}\t{}",
                    correction.id, correction.current, correction.proposal
                ),
            )
        };
        let mut kept = read_lines(&dir.join(file));
        kept.retain(|existing| !existing.starts_with(&format!("{}\t", correction.id)));
        kept.push(line);
        let mut text = header.to_owned();
        for line in &kept {
            text.push_str(line);
            text.push('\n');
        }
        let written = std::fs::write(dir.join(file), text)
            .and_then(|()| write_corrections(&dir.join("correcciones.tsv"), &self.corrections));
        if let Err(error) = written {
            self.status = format!("No se pudo guardar la corrección: {error}");
        }
        if approve {
            self.approved_corrections = kept.len();
            // The page around the next proposals shows the approved line.
            for balloon in &mut self.page_balloons {
                if balloon.id == correction.id {
                    balloon.translation = correction.proposal.replace(['\t', '\n'], " ");
                }
            }
        }
    }

    /// Adds one term to this work's glossary, replacing the same original.
    fn add_term(&mut self, source: String, target: String) {
        let Some(dir) = self.work_dir() else { return };
        let (source, target) = (source.trim().to_owned(), target.trim().to_owned());
        if source.is_empty() || target.is_empty() {
            return;
        }
        let _ = std::fs::create_dir_all(&dir);
        let mut terms = read_terms(&dir.join("glosario.tsv"));
        terms.retain(|term| term.source.to_lowercase() != source.to_lowercase());
        terms.push(Term {
            source,
            target,
            note: "Aprobado desde una corrección.".to_owned(),
        });
        let header =
            "# Glosario de esta obra: original<TAB>traducción<TAB>nota. Manda sobre el global.\n";
        match write_terms(&dir.join("glosario.tsv"), header, &terms) {
            Ok(()) => {
                self.status = format!("{} término(s) en el glosario de la obra.", terms.len())
            }
            Err(error) => self.status = format!("No se pudo guardar el glosario: {error}"),
        }
    }

    fn apply_corrections(&mut self, ctx: &egui::Context) {
        let Some(project) = self.project().map(Path::to_path_buf) else {
            return;
        };
        if self.current.is_some() || !confirm_koharu_closed() {
            return;
        }
        self.lines.clear();
        let khr = self.khr.clone();
        let project_arg = project.display().to_string();
        self.push(
            &format!(
                "[{}] Aplicar correcciones aprobadas",
                project_name(&project)
            ),
            &khr,
            &["aplicar", "--project", &project_arg],
            false,
            After::Nothing,
        );
        self.approved_corrections = 0;
        self.begin(ctx);
    }

    /// Renders the finished pages with khr, one at a time: exporting from the
    /// Koharu app kept every page in memory and took the PC down.
    fn export_pages(&mut self, ctx: &egui::Context) {
        let Some(project) = self.project().map(Path::to_path_buf) else {
            return;
        };
        if self.current.is_some() {
            return;
        }
        self.lines.clear();
        let out = Path::new(EXPORT_DIR).join(project_name(&project));
        let khr = self.khr.clone();
        let project_arg = project.display().to_string();
        let out_arg = out.display().to_string();
        self.push(
            &format!("[{}] Exportar páginas → {out_arg}", project_name(&project)),
            &khr,
            &["exportar", "--project", &project_arg, "--out", &out_arg],
            false,
            After::Nothing,
        );
        self.begin(ctx);
    }

    /// Proposals made before `revisar` saved the pages around them: read the
    /// pages now (quick, no model) and reopen the window with them.
    fn load_page_context(&mut self, ctx: &egui::Context) {
        let Some(project) = self.project().map(Path::to_path_buf) else {
            return;
        };
        self.lines.clear();
        let khr = self.khr.clone();
        let project_arg = project.display().to_string();
        let mut args = vec!["contexto", "--project", &project_arg];
        if self.left_to_right {
            args.push("--left-to-right");
        }
        let name = format!(
            "[{}] Leer las páginas de las correcciones",
            project_name(&project)
        );
        self.push(
            &name,
            &khr,
            &args,
            true,
            After::ShowCorrections(project.clone()),
        );
        self.begin(ctx);
    }

    fn corrections_window(&mut self, ctx: &egui::Context) {
        let mut open = self.corrections_open;
        let mut decision: Option<(usize, bool)> = None;
        let mut to_glossary: Option<usize> = None;
        let mut save_term = false;
        let mut cancel_term = false;
        let mut apply = false;
        let mut all: Option<bool> = None;
        let busy = self.current.is_some();
        egui::Window::new("Correcciones propuestas")
            .open(&mut open)
            .default_size([820.0, 480.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} pendiente(s); {} aprobada(s) sin aplicar.",
                    self.corrections.len(),
                    self.approved_corrections
                ));
                ui.horizontal(|ui| {
                    let label = "Aplicar las aprobadas al proyecto";
                    if ui
                        .add_enabled(!busy && self.approved_corrections > 0, egui::Button::new(label))
                        .clicked()
                    {
                        apply = true;
                    }
                    ui.label(egui::RichText::new("(con Koharu cerrado)").weak());
                });
                if let Some((source, target)) = &mut self.new_term {
                    ui.separator();
                    ui.label("Nuevo término del glosario: recorta el original a la palabra o frase que se repite.");
                    ui.horizontal(|ui| {
                        ui.label("Original:");
                        ui.add(egui::TextEdit::singleline(source).desired_width(240.0));
                        ui.label("Traducción:");
                        ui.add(egui::TextEdit::singleline(target).desired_width(240.0));
                        if ui.button("Guardar término").clicked() {
                            save_term = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            cancel_term = true;
                        }
                    });
                }
                ui.separator();
                if self.corrections.is_empty() {
                    ui.label("No hay correcciones pendientes. Se generan con el paso 6 (Revisar la traducción).");
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label("La página entera en orden de lectura;");
                    ui.label(diff_job(ui, &[(Piece::Removed, "tachado en rojo".to_owned())], Piece::Removed));
                    ui.label("lo que se quita,");
                    ui.label(diff_job(ui, &[(Piece::Added, "en verde".to_owned())], Piece::Added));
                    ui.label("lo que entra. La propuesta se puede editar antes de aprobarla.");
                });
                ui.horizontal(|ui| {
                    if ui.button("Aprobar todas").clicked() {
                        all = Some(true);
                    }
                    if ui.button("Rechazar todas").clicked() {
                        all = Some(false);
                    }
                });
                let corrections = &mut self.corrections;
                let balloons = &self.page_balloons;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut pages: Vec<String> = Vec::new();
                    for correction in corrections.iter() {
                        if !pages.contains(&correction.page) {
                            pages.push(correction.page.clone());
                        }
                    }
                    for page in &pages {
                        ui.add_space(6.0);
                        ui.heading(format!("Página {page}"));
                        let mut shown = Vec::new();
                        for balloon in balloons.iter().filter(|balloon| &balloon.page == page) {
                            match corrections.iter().position(|c| c.id == balloon.id) {
                                Some(index) => {
                                    shown.push(index);
                                    correction_card(ui, index, &mut corrections[index], &mut decision, &mut to_glossary);
                                }
                                None if balloon.translation.is_empty() => {
                                    ui.label(egui::RichText::new(format!("({})", balloon.original)).weak())
                                        .on_hover_text("Sin traducir");
                                }
                                None => {
                                    ui.horizontal_wrapped(|ui| {
                                        speaker_label(ui, &balloon.speaker);
                                        ui.label(&balloon.translation).on_hover_text(&balloon.original);
                                    });
                                }
                            }
                        }
                        // Proposals whose balloon the saved page does not
                        // hold (no context yet, or the page changed).
                        for index in 0..corrections.len() {
                            if &corrections[index].page == page && !shown.contains(&index) {
                                correction_card(ui, index, &mut corrections[index], &mut decision, &mut to_glossary);
                            }
                        }
                        ui.separator();
                    }
                });
            });
        self.corrections_open = open;
        if let Some((index, approve)) = decision {
            self.decide_correction(index, approve);
        }
        if let Some(approve) = all
            && self.work_dir().is_some()
        {
            // Edits made in the cards are kept: each one goes out as shown.
            while !self.corrections.is_empty() {
                self.decide_correction(0, approve);
            }
        }
        if let Some(index) = to_glossary
            && let Some(correction) = self.corrections.get(index).cloned()
        {
            self.new_term = Some((correction.original.clone(), correction.proposal.clone()));
            self.decide_correction(index, true);
        }
        if save_term && let Some((source, target)) = self.new_term.take() {
            self.add_term(source, target);
        }
        if cancel_term {
            self.new_term = None;
        }
        if apply {
            self.apply_corrections(ctx);
        }
    }

    /// What the selected project needs next, from what its work folder holds.
    fn next_step(&self) -> String {
        let name = self
            .project()
            .and_then(Path::file_stem)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("[{name}] {}", self.next_step_for_project())
    }

    fn next_step_for_project(&self) -> String {
        let Some(dir) = self.work_dir() else {
            return "Elige un proyecto (se crea en Koharu importando las imágenes).".to_owned();
        };
        if !dir.join("ficha.md").exists() {
            return "Siguiente: pasos 1-6 con el idioma original elegido (o añádelo a la cola)."
                .to_owned();
        }
        let proposals = read_terms(&dir.join("propuestas.tsv")).len();
        if proposals > 0 {
            return format!(
                "Siguiente: aprueba o rechaza {proposals} término(s) en \"Glosario y ficha\"; \
                 si cambias alguno, vuelve a traducir (paso 5)."
            );
        }
        let pending = read_corrections(&dir.join("correcciones.tsv")).len();
        if pending > 0 {
            return format!("Siguiente: revisa {pending} corrección(es) en \"Correcciones\".");
        }
        if !read_lines(&dir.join("correcciones-aprobadas.tsv")).is_empty() {
            return "Siguiente: \"Aplicar las aprobadas\" en la ventana Correcciones.".to_owned();
        }
        "Siguiente: abre el proyecto en Koharu, revisa y exporta. Si corriges a mano, \
         usa \"Aprender de mis correcciones\" para alimentar el glosario."
            .to_owned()
    }

    fn stop(&mut self) {
        self.queue.clear();
        // Stopped on purpose: nothing to resume, and no shutdown either.
        self.shutdown_when_done = false;
        let _ = std::fs::remove_file(QUEUE_FILE);
        let pid = self.child.lock().unwrap().as_ref().map(Child::id);
        if let Some(pid) = pid {
            // taskkill /T also ends whatever the step spawned itself.
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .creation_flags(CREATE_NO_WINDOW)
                .status();
            self.log("!!! Detenido por el usuario.");
        }
    }
}

impl eframe::App for Panel {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);
        let busy = self.current.is_some();

        egui::Panel::top("controls").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Proyecto:");
                let shown = self
                    .project()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "(ninguno)".to_owned());
                let mut changed = false;
                ui.add_enabled_ui(true, |ui| {
                    egui::ComboBox::from_id_salt("project")
                        .width(440.0)
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            for (index, path) in self.projects.iter().enumerate() {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                if ui.selectable_value(&mut self.selected, Some(index), name).clicked() {
                                    changed = true;
                                }
                            }
                        });
                    if ui.button("Examinar...").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("Proyecto de Koharu", &["khrproj"])
                            .set_directory(PROJECTS_DIR)
                            .pick_file()
                    {
                        let index = match self.projects.iter().position(|p| *p == path) {
                            Some(index) => index,
                            None => {
                                self.projects.insert(0, path);
                                0
                            }
                        };
                        self.selected = Some(index);
                        changed = true;
                    }
                    if ui.button("Recargar").clicked() {
                        self.reload_projects();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Nuevo desde carpeta..."))
                        .on_hover_text("Crea un proyecto con las imágenes de una carpeta, una página por imagen")
                        .clicked()
                        && let Some(folder) = rfd::FileDialog::new().pick_folder()
                    {
                        self.create_project(&ctx, &folder);
                    }
                });
                if changed {
                    self.project_changed();
                }
            });
            ui.add_space(6.0);

            ui.add_enabled_ui(true, |ui| {
                ui.columns(2, |cols| {
                    cols[0].strong("Pasos (en orden)");
                    for ((_, label), on) in STAGES.iter().zip(self.stages.iter_mut()) {
                        cols[0].checkbox(on, *label);
                    }
                    cols[0].checkbox(&mut self.study, "4. Estudiar la obra (ficha y términos)");
                    if self.study {
                        cols[0].indent("user_notes", |ui| {
                            ui.label("Antes de la ficha (opcional); la IA lo compara con lo que lee:");
                            let mut fetch = false;
                            ui.horizontal(|ui| {
                                let field = ui.add(
                                    egui::TextEdit::singleline(&mut self.gallery)
                                        .hint_text("Nº o enlace de galería e-hentai")
                                        .desired_width(200.0),
                                );
                                let entered =
                                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                                let ready = !self.fetching_gallery && !self.gallery.trim().is_empty();
                                let button = ui.add_enabled(ready, egui::Button::new("Traer etiquetas"));
                                fetch = ready && (button.clicked() || entered);
                                if self.fetching_gallery {
                                    ui.spinner();
                                }
                            });
                            if fetch {
                                self.fetch_gallery(ui.ctx());
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut self.tags)
                                    .hint_text("Etiquetas, separadas por comas")
                                    .desired_width(f32::INFINITY),
                            );
                            ui.add(
                                egui::TextEdit::multiline(&mut self.description)
                                    .hint_text("Descripción breve: de qué va, quién es quién, tono")
                                    .desired_rows(3)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    }
                    cols[0].checkbox(&mut self.translate, "5. Traducir");
                    cols[0].checkbox(
                        &mut self.review_step,
                        "6. Revisar la traducción (propone correcciones)",
                    );

                    cols[1].strong("Opciones");
                    let mut changed = false;
                    egui::Grid::new("modelos").num_columns(2).show(&mut cols[1], |ui| {
                        ui.label("Ficha (paso 4):");
                        changed |= model_menu(ui, "modelo_ficha", &mut self.study_model, &self.models, None);
                        ui.end_row();
                        ui.label("Traducción (paso 5):");
                        changed |= model_menu(
                            ui,
                            "modelo_traduccion",
                            &mut self.corrector,
                            &self.models,
                            Some(&mut self.llm_translation),
                        );
                        ui.end_row();
                        ui.label("Revisión (paso 6):");
                        changed |= model_menu(ui, "modelo_revision", &mut self.reviewer, &self.models, None);
                        ui.end_row();
                    });
                    if changed {
                        self.save_model_choices();
                    }
                    cols[1].horizontal(|ui| {
                        ui.label("Páginas (0 = todas):");
                        ui.add(egui::DragValue::new(&mut self.pages).range(0..=9999));
                    });
                    cols[1].horizontal(|ui| {
                        ui.label("Idioma original:");
                        egui::ComboBox::from_id_salt("idioma")
                            .selected_text(LANGUAGES[self.idioma].1)
                            .show_ui(ui, |ui| {
                                for (index, (_, name)) in LANGUAGES.iter().enumerate() {
                                    ui.selectable_value(&mut self.idioma, index, *name);
                                }
                            });
                    });
                    cols[1].checkbox(
                        &mut self.left_to_right,
                        "Leer de izquierda a derecha (desmarcado = manga)",
                    );
                });
            });
            ui.add_space(6.0);

            if !self.unfinished.is_empty() {
                let mut projects: Vec<&str> = Vec::new();
                for name in self.unfinished.iter().filter_map(Step::project) {
                    if !projects.contains(&name) {
                        projects.push(name);
                    }
                }
                let summary = format!(
                    "Quedó una cola sin terminar: {} paso(s){}",
                    self.unfinished.len(),
                    if projects.is_empty() {
                        String::new()
                    } else {
                        format!(" de {}", projects.join(", "))
                    }
                );
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), summary);
                    if ui.button("Reanudar").clicked() {
                        self.resume(&ctx);
                    }
                    if ui.button("Descartar").clicked() {
                        self.unfinished.clear();
                        if self.current.is_none() && self.queue.is_empty() {
                            let _ = std::fs::remove_file(QUEUE_FILE);
                        }
                    }
                });
                ui.add_space(4.0);
            }

            ui.horizontal(|ui| {
                let can_run = !busy && self.project().is_some();
                let run = egui::Button::new(egui::RichText::new("▶ Ejecutar").strong());
                if ui.add_enabled(can_run, run).clicked() {
                    self.run(&ctx);
                }
                let add = egui::Button::new("+ Añadir a la cola");
                if ui.add_enabled(self.project().is_some(), add).clicked() {
                    self.enqueue(&ctx);
                }
                if ui.add_enabled(busy, egui::Button::new("■ Detener")).clicked() {
                    self.stop();
                }
                ui.checkbox(&mut self.shutdown_when_done, "Apagar el PC al terminar");
                if ui.button("Abrir Koharu").clicked() {
                    self.status = match Command::new(&self.koharu).spawn() {
                        Ok(_) => "Koharu abierto: carga el proyecto desde su menú.".to_owned(),
                        Err(error) => format!("No se pudo abrir Koharu: {error}"),
                    };
                }
                if ui.add_enabled(can_run, egui::Button::new("Aprender de mis correcciones")).clicked()
                    && confirm_koharu_closed()
                {
                    self.learn(&ctx);
                }
                if ui
                    .add_enabled(can_run, egui::Button::new("Exportar páginas"))
                    .on_hover_text(format!("PNG terminadas en {EXPORT_DIR}\\<proyecto>. Guarda el proyecto en Koharu antes."))
                    .clicked()
                {
                    self.export_pages(&ctx);
                }
                if ui.add_enabled(self.project().is_some(), egui::Button::new("Glosario y ficha")).clicked() {
                    self.load_glossary();
                    self.glossary_open = true;
                }
                if ui.add_enabled(self.project().is_some(), egui::Button::new("Correcciones")).clicked() {
                    self.load_corrections();
                    self.corrections_open = true;
                    if !self.corrections.is_empty() && self.page_balloons.is_empty() && !busy {
                        self.load_page_context(&ctx);
                    }
                }
                if ui.add_enabled(!busy, egui::Button::new("Liberar VRAM")).clicked() {
                    self.lines.clear();
                    let khr = self.khr.clone();
                    self.push("Liberar VRAM", &khr, &["models", "unload"], true, After::Nothing);
                    self.begin(&ctx);
                }
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(self.next_step()).color(egui::Color32::from_rgb(120, 170, 230)));
            ui.add_space(6.0);
        });

        if self.glossary_open {
            self.glossary_window(&ctx);
        }
        if self.corrections_open {
            self.corrections_window(&ctx);
        }

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if busy {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
            let waiting = self.waiting_projects();
            if !waiting.is_empty() {
                let mut cancel = None;
                ui.horizontal_wrapped(|ui| {
                    ui.label("En cola:");
                    for name in &waiting {
                        ui.label(name);
                        if ui
                            .small_button("×")
                            .on_hover_text("Quitar de la cola")
                            .clicked()
                        {
                            cancel = Some(name.clone());
                        }
                    }
                });
                if let Some(name) = cancel {
                    self.cancel_queued(&name);
                }
            }
            if let Some(step) = &self.current {
                // The running step names its own project, whatever is selected.
                ui.label(egui::RichText::new(format!("En curso: {}", step.name)).strong());
            }
            if busy {
                let elapsed = short_duration(self.step_started.elapsed());
                match &self.progress {
                    Some(progress) => {
                        let left = match progress.remaining() {
                            Some(left) => format!("quedan ~{}", short_duration(left)),
                            None => "calculando el tiempo...".to_owned(),
                        };
                        ui.add(egui::ProgressBar::new(progress.fraction()).text(format!(
                            "{}: {} de {} · {elapsed} transcurridos · {left}",
                            progress.what, progress.done, progress.total
                        )));
                    }
                    None => {
                        ui.label(format!(
                            "Paso en curso: {elapsed} transcurridos (sin avance medible)"
                        ));
                    }
                }
                // Keep the clock moving between output lines.
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.lines {
                        let mut text = egui::RichText::new(line).monospace();
                        if line.starts_with("!!!") {
                            text = text.color(egui::Color32::from_rgb(235, 90, 90));
                        } else if line.starts_with("===") {
                            text = text.strong();
                        } else if line.starts_with("+ ") {
                            text = text.color(egui::Color32::from_rgb(110, 200, 120));
                        } else if line.starts_with("- ") {
                            text = text.color(egui::Color32::from_rgb(200, 140, 110));
                        }
                        ui.label(text);
                    }
                });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop();
    }
}

/// A project Koharu has open gets overwritten the next time Koharu saves it.
fn confirm_koharu_closed() -> bool {
    let running = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq koharu.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .to_lowercase()
                .contains("koharu.exe")
        })
        .unwrap_or(false);
    !running
        || rfd::MessageDialog::new()
            .set_title("Koharu abierto")
            .set_level(rfd::MessageLevel::Warning)
            .set_description(
                "Koharu está abierto. Si tiene este proyecto cargado y guardas después, \
                 se pierden los cambios hechos aquí.\n\n¿Continuar de todos modos?",
            )
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
}

fn modified(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn read_terms(path: &Path) -> Vec<Term> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('\t').map(str::trim);
            let source = fields.next()?.to_owned();
            let target = fields.next()?.to_owned();
            let note = fields.next().unwrap_or_default().to_owned();
            (!source.is_empty() && !target.is_empty()).then_some(Term {
                source,
                target,
                note,
            })
        })
        .collect()
}

/// Moves `terms` into the work's glossary (or its rejected list), replacing
/// entries with the same original, and leaves `pending` as the proposals.
/// Returns how many terms the target file now holds.
fn move_terms(
    dir: &Path,
    terms: Vec<Term>,
    approve: bool,
    pending: &[Term],
) -> std::io::Result<usize> {
    let target = if approve {
        "glosario.tsv"
    } else {
        "rechazados.tsv"
    };
    std::fs::create_dir_all(dir)?;
    let mut kept = read_terms(&dir.join(target));
    for term in terms {
        kept.retain(|existing| existing.source.to_lowercase() != term.source.to_lowercase());
        kept.push(term);
    }
    let header = if approve {
        "# Glosario de esta obra: original<TAB>traducción<TAB>nota. Manda sobre el global.\n"
    } else {
        "# Términos rechazados: khr no volverá a proponerlos.\n"
    };
    let pending_header = "# Propuestas pendientes: original<TAB>traducción<TAB>nota\n\
                          # Apruébalas o recházalas desde el panel.\n";
    write_terms(&dir.join(target), header, &kept)?;
    write_terms(&dir.join("propuestas.tsv"), pending_header, pending)?;
    Ok(kept.len())
}

fn write_terms(path: &Path, header: &str, terms: &[Term]) -> std::io::Result<()> {
    let mut text = header.to_owned();
    for term in terms {
        let target = term.target.replace('\t', " ");
        if term.note.is_empty() {
            text.push_str(&format!("{}\t{target}\n", term.source));
        } else {
            text.push_str(&format!("{}\t{target}\t{}\n", term.source, term.note));
        }
    }
    std::fs::write(path, text)
}

/// The queue a previous run left behind, if any.
fn load_queue() -> Vec<Step> {
    std::fs::read_to_string(QUEUE_FILE)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn read_corrections(path: &Path) -> Vec<Correction> {
    read_lines(path)
        .into_iter()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(Correction {
                id: fields.next()?.to_owned(),
                page: fields.next()?.to_owned(),
                original: fields.next()?.to_owned(),
                current: fields.next()?.to_owned(),
                proposal: fields.next()?.to_owned(),
                reason: fields.next().unwrap_or_default().to_owned(),
                speaker: fields.next().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

fn write_corrections(path: &Path, corrections: &[Correction]) -> std::io::Result<()> {
    let mut text = String::from(
        "# Correcciones propuestas: id\tpágina\toriginal\tactual\tpropuesta\tmotivo\thabla\n\
         # Apruébalas o recházalas desde el panel.\n",
    );
    for c in corrections {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.id,
            c.page,
            c.original,
            c.current,
            c.proposal.replace(['\t', '\n'], " "),
            c.reason,
            c.speaker
        ));
    }
    std::fs::write(path, text)
}

fn read_balloons(path: &Path) -> Vec<Balloon> {
    read_lines(path)
        .into_iter()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(Balloon {
                page: fields.next()?.to_owned(),
                id: fields.next()?.to_owned(),
                original: fields.next()?.to_owned(),
                translation: fields.next().unwrap_or_default().to_owned(),
                speaker: fields.next().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// Who speaks a balloon, as the reviewer read it, before its text; in orange
/// when the reviewer was unsure, so a proposal built on it is read with care.
fn speaker_label(ui: &mut egui::Ui, speaker: &str) {
    if speaker.is_empty() {
        return;
    }
    let (name, doubt) = match speaker.split_once(" (duda: ") {
        Some((name, doubt)) => (name, Some(doubt.trim_end_matches(')'))),
        None => (speaker, None),
    };
    let text = egui::RichText::new(format!("[{name}]")).small();
    match doubt {
        Some(doubt) => {
            ui.label(text.color(egui::Color32::from_rgb(230, 140, 40)))
                .on_hover_text(format!("El revisor no está seguro: {doubt}"));
        }
        None => {
            ui.label(text.weak())
                .on_hover_text("Quién habla y a quién, según el revisor");
        }
    }
}

/// A proposal inside its page: the original, the line as it is with what
/// goes struck out, the line as it would be with what comes in, the editable
/// proposal and the buttons.
fn correction_card(
    ui: &mut egui::Ui,
    index: usize,
    correction: &mut Correction,
    decision: &mut Option<(usize, bool)>,
    to_glossary: &mut Option<usize>,
) {
    let accent = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(220, 180, 80)
    } else {
        egui::Color32::from_rgb(190, 130, 20)
    };
    egui::Frame::group(ui.style())
        .stroke(egui::Stroke::new(1.5, accent))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                speaker_label(ui, &correction.speaker);
                ui.label(egui::RichText::new(&correction.original).weak());
            });
            let pieces = word_diff(&correction.current, &correction.proposal);
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new("Ahora:").strong());
                ui.label(diff_job(ui, &pieces, Piece::Removed));
            });
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new("Queda:").strong());
                ui.label(diff_job(ui, &pieces, Piece::Added));
            });
            ui.add(
                egui::TextEdit::multiline(&mut correction.proposal)
                    .desired_rows(1)
                    .desired_width(f32::INFINITY),
            );
            ui.label(egui::RichText::new(&correction.reason).italics().weak());
            ui.horizontal(|ui| {
                if ui.button("Aprobar").clicked() {
                    *decision = Some((index, true));
                }
                if ui.button("Rechazar").clicked() {
                    *decision = Some((index, false));
                }
                if ui.button("Aprobar + glosario").clicked() {
                    *to_glossary = Some(index);
                }
            });
        });
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Piece {
    Same,
    Removed,
    Added,
}

/// Word by word difference between the current line and the proposal
/// (longest common subsequence; balloons are short).
fn word_diff(before: &str, after: &str) -> Vec<(Piece, String)> {
    let a: Vec<&str> = before.split_whitespace().collect();
    let b: Vec<&str> = after.split_whitespace().collect();
    let mut common = vec![vec![0_usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            common[i][j] = if a[i] == b[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut pieces: Vec<(Piece, String)> = Vec::new();
    let mut push = |piece: Piece, word: &str| match pieces.last_mut() {
        Some((last, text)) if *last == piece => {
            text.push(' ');
            text.push_str(word);
        }
        _ => pieces.push((piece, word.to_owned())),
    };
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            push(Piece::Same, a[i]);
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || common[i + 1][j] >= common[i][j + 1]) {
            push(Piece::Removed, a[i]);
            i += 1;
        } else {
            push(Piece::Added, b[j]);
            j += 1;
        }
    }
    pieces
}

/// One side of a difference: the shared words plus the `side` ones, marked.
fn diff_job(ui: &egui::Ui, pieces: &[(Piece, String)], side: Piece) -> egui::text::LayoutJob {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let plain = ui.visuals().text_color();
    let (color, background) = match (side, ui.visuals().dark_mode) {
        (Piece::Removed, true) => (
            egui::Color32::from_rgb(255, 150, 140),
            egui::Color32::from_rgb(95, 35, 35),
        ),
        (Piece::Removed, false) => (
            egui::Color32::from_rgb(160, 30, 30),
            egui::Color32::from_rgb(255, 215, 215),
        ),
        (_, true) => (
            egui::Color32::from_rgb(150, 235, 150),
            egui::Color32::from_rgb(30, 80, 40),
        ),
        (_, false) => (
            egui::Color32::from_rgb(20, 110, 40),
            egui::Color32::from_rgb(210, 245, 210),
        ),
    };
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width().max(200.0);
    for (piece, text) in pieces
        .iter()
        .filter(|(piece, _)| *piece == Piece::Same || *piece == side)
    {
        if !job.text.is_empty() {
            job.append(" ", 0.0, egui::TextFormat::simple(font.clone(), plain));
        }
        let mut format = egui::TextFormat::simple(font.clone(), plain);
        if *piece != Piece::Same {
            format.color = color;
            format.background = background;
            if side == Piece::Removed {
                format.strikethrough = egui::Stroke::new(1.5, color);
            }
        }
        job.append(text, 0.0, format);
    }
    job
}

fn project_name(project: &Path) -> String {
    project
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn log_base(project: &Path) -> String {
    project
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The bundled fonts have no kana, hangul or hanzi, so originals showed as
/// boxes. Borrow Windows' own fonts as fallbacks, Japanese first.
fn install_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [
        ("yu-gothic", r"C:\Windows\Fonts\YuGothM.ttc"),
        ("malgun", r"C:\Windows\Fonts\malgun.ttf"),
        ("yahei", r"C:\Windows\Fonts\msyh.ttc"),
    ] {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .push(name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Koharu - Panel")
            .with_inner_size([900.0, 680.0])
            .with_min_inner_size([700.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Koharu - Panel",
        options,
        Box::new(|cc| {
            install_cjk_fonts(&cc.egui_ctx);
            Ok(Box::new(Panel::new()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> Step {
        Step {
            name: name.to_owned(),
            program: PathBuf::new(),
            args: Vec::new(),
            ignore_error: false,
            after: After::Nothing,
        }
    }

    #[test]
    fn the_difference_marks_only_changed_words() {
        let pieces = word_diff("Este primavera me caso", "Esta primavera me caso contigo");
        assert_eq!(
            pieces,
            vec![
                (Piece::Removed, "Este".to_owned()),
                (Piece::Added, "Esta".to_owned()),
                (Piece::Same, "primavera me caso".to_owned()),
                (Piece::Added, "contigo".to_owned()),
            ]
        );
    }

    #[test]
    fn planned_steps_name_their_project() {
        assert_eq!(
            step("[Sakurami EN] 5. Traducir").project(),
            Some("Sakurami EN")
        );
        assert_eq!(step("Liberar VRAM").project(), None);
    }

    #[test]
    fn the_estimate_waits_for_measured_work() {
        let mut progress = StepProgress {
            what: "ocr".to_owned(),
            done: 0,
            total: 10,
            first: None,
        };
        assert!(progress.remaining().is_none());
        progress.first = Some((Instant::now() - Duration::from_secs(20), 2));
        progress.done = 2;
        assert!(progress.remaining().is_none());
        progress.done = 4;
        // Two pages in 20 s leaves six pages, about 60 s.
        let left = progress.remaining().unwrap().as_secs();
        assert!((59..=61).contains(&left), "{left}");
        assert_eq!(short_duration(Duration::from_secs(125)), "2 min 05 s");
    }
}

/// The LLMs LM Studio has downloaded, by the key `lms load` takes.
fn installed_models(lms: &Path) -> Vec<String> {
    let Ok(output) = Command::new(lms)
        .args(["ls", "--json"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let mut models: Vec<String> = listed
        .iter()
        .filter(|model| model.get("type").and_then(|kind| kind.as_str()) == Some("llm"))
        .filter_map(|model| {
            model
                .get("modelKey")
                .and_then(|key| key.as_str())
                .map(str::to_owned)
        })
        .collect();
    models.sort();
    models
}

/// A menu with the installed models for one step; the translation menu also
/// offers DeepL, which clears `local`. Returns whether the choice changed.
fn model_menu(
    ui: &mut egui::Ui,
    id: &str,
    model: &mut String,
    models: &[String],
    mut local: Option<&mut bool>,
) -> bool {
    const DEEPL: &str = "DeepL (en línea)";
    let on_deepl = local.as_deref().is_some_and(|local| !*local);
    let shown = if on_deepl {
        DEEPL.to_owned()
    } else {
        model.clone()
    };
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .width(260.0)
        .show_ui(ui, |ui| {
            if let Some(local) = local.as_deref_mut() {
                if ui.selectable_label(!*local, DEEPL).clicked() && *local {
                    *local = false;
                    changed = true;
                }
            }
            // A saved model that is no longer installed stays visible, so the
            // menu never shows a choice that differs from what would run.
            let mut listed: Vec<String> = models.to_vec();
            if !model.is_empty() && !models.contains(model) {
                listed.push(model.clone());
            }
            for name in listed {
                let chosen = !on_deepl && name == *model;
                if ui.selectable_label(chosen, name.as_str()).clicked() && !chosen {
                    *model = name;
                    if let Some(local) = local.as_deref_mut() {
                        *local = true;
                    }
                    changed = true;
                }
            }
        });
    changed
}
