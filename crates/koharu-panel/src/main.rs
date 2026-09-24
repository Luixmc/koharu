//! Small desktop panel over `khr`: pick a project, tick the stages, run them
//! in order and watch the output. Every step is a child process, so the panel
//! holds no models and a crash in a stage never takes the window down.

#![windows_subsystem = "windows"]

use std::collections::VecDeque;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use eframe::egui;

const PROJECTS_DIR: &str = r"I:\Usuario\Documentos\Koharu";
const INSTRUCTIONS: &str = r"I:\Koharu\prompts\05-postproceso.txt";
const LOGS_DIR: &str = r"I:\Koharu\registros";
const DEFAULT_CORRECTOR: &str = "thedrummer_cydonia-24b-v4.3";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_LINES: usize = 5000;

const STAGES: [(&str, &str); 4] = [
    ("detection", "1. Detectar globos"),
    ("ocr", "2. Reconocer texto (OCR)"),
    ("translation", "3. Traducir (DeepL)"),
    ("inpainting", "4. Limpiar texto original"),
];

/// What to do once a step exits successfully.
enum After {
    Nothing,
    ReviewCorrections { project: PathBuf, log: PathBuf },
    DeleteLog(PathBuf),
}

struct Step {
    name: String,
    program: PathBuf,
    args: Vec<String>,
    ignore_error: bool,
    after: After,
}

enum Event {
    Output(String),
    Exited(Option<i32>),
}

struct Panel {
    khr: PathBuf,
    lms: PathBuf,
    koharu: PathBuf,
    projects: Vec<PathBuf>,
    selected: Option<usize>,
    stages: [bool; 4],
    correct: bool,
    corrector: String,
    pages: u32,
    left_to_right: bool,
    review: bool,
    queue: VecDeque<Step>,
    current: Option<Step>,
    failed: bool,
    child: Arc<Mutex<Option<Child>>>,
    events: (Sender<Event>, Receiver<Event>),
    lines: Vec<String>,
    partial: String,
    status: String,
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
            stages: [true; 4],
            correct: true,
            corrector: DEFAULT_CORRECTOR.to_owned(),
            pages: 0,
            left_to_right: false,
            review: true,
            queue: VecDeque::new(),
            current: None,
            failed: false,
            child: Arc::new(Mutex::new(None)),
            events: channel(),
            lines: Vec::new(),
            partial: String::new(),
            status: "Listo. El proyecto se crea en Koharu (importar imágenes); aquí se procesa."
                .to_owned(),
        };
        panel.reload_projects();
        panel
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
            .or(if self.projects.is_empty() { None } else { Some(0) });
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

    fn push(&mut self, name: &str, program: &Path, args: &[&str], ignore_error: bool, after: After) {
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
                self.log(format!("!!! No se pudo lanzar {}: {error}", step.program.display()));
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
                        if !line.is_empty() {
                            self.log(line.to_owned());
                        }
                    }
                }
                Event::Exited(code) => {
                    if !self.partial.is_empty() {
                        let rest = std::mem::take(&mut self.partial);
                        self.log(rest);
                    }
                    let Some(step) = self.current.take() else { continue };
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
                        self.push("Liberar VRAM", &khr, &["models", "unload"], true, After::Nothing);
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
            After::DeleteLog(path) => {
                let _ = std::fs::remove_file(path);
            }
            After::ReviewCorrections { project, log } => self.review_corrections(&project, &log),
        }
    }

    fn review_corrections(&mut self, project: &Path, log: &Path) {
        let changes = read_changes(log);
        self.log("");
        self.log(format!("--- {} corrección(es) ---", changes.len()));
        for change in &changes {
            let field = |key: &str| change[key].as_str().unwrap_or_default().replace('\n', " / ");
            self.log(format!("- {}", field("before")));
            self.log(format!("+ {}", field("after")));
            self.log("");
        }
        if changes.is_empty() || !self.review {
            return;
        }
        let keep = rfd::MessageDialog::new()
            .set_title("Revisar correcciones")
            .set_description(format!(
                "Se aplicaron {} correcciones (están en el registro del panel).\n\n¿Las conservo?",
                changes.len()
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if keep == rfd::MessageDialogResult::No {
            self.push_revert(project, log);
        }
    }

    fn push_revert(&mut self, project: &Path, log: &Path) {
        let khr = self.khr.clone();
        let project_arg = project.display().to_string();
        let log_arg = log.display().to_string();
        self.push(
            "Deshacer correcciones",
            &khr,
            &["revert", "--project", &project_arg, "--log", &log_arg],
            false,
            After::DeleteLog(log.to_path_buf()),
        );
    }

    fn run(&mut self, ctx: &egui::Context) {
        let Some(project) = self.project().map(Path::to_path_buf) else { return };
        let stages: Vec<&str> = STAGES
            .iter()
            .zip(self.stages)
            .filter(|(_, on)| *on)
            .map(|((key, _), _)| *key)
            .collect();
        if stages.is_empty() && !self.correct {
            self.status = "Marca al menos una etapa.".to_owned();
            return;
        }
        let model = self.corrector.trim().to_owned();
        if self.correct && model.is_empty() {
            self.status = "Escribe el modelo corrector.".to_owned();
            return;
        }
        if !confirm_koharu_closed() {
            return;
        }
        self.lines.clear();
        self.log(format!("Proyecto: {}", project.display()));

        let khr = self.khr.clone();
        let project_arg = project.display().to_string();
        let pages = self.pages.to_string();
        let page_args: Vec<&str> = if self.pages > 0 { vec!["--pages", &pages] } else { Vec::new() };

        if !stages.is_empty() {
            // Koharu's stages need the card; release whatever LM Studio holds.
            self.push("Liberar VRAM", &khr, &["models", "unload"], true, After::Nothing);
            let joined = stages.join(",");
            let mut args = vec!["run", "--project", &project_arg, "--stages", &joined];
            args.extend(&page_args);
            let name = format!("Etapas: {}", stages.join(", "));
            self.push(&name, &khr, &args, false, After::Nothing);
        }
        if self.correct {
            let log = log_path(&project);
            let log_arg = log.display().to_string();
            let mut args = vec![
                "post",
                "--project",
                &project_arg,
                "--model",
                &model,
                "--instructions",
                INSTRUCTIONS,
                "--log",
                &log_arg,
            ];
            args.extend(&page_args);
            if self.left_to_right {
                args.push("--left-to-right");
            }
            let lms = self.lms.clone();
            self.push("Iniciar servidor de LM Studio", &lms, &["server", "start"], true, After::Nothing);
            self.push(&format!("Cargar {model}"), &khr, &["models", "load", &model], false, After::Nothing);
            self.push("Corregir traducciones", &khr, &args, false, After::Nothing);
            self.push(
                "Liberar modelo de LM Studio",
                &khr,
                &["models", "unload", &model],
                true,
                After::ReviewCorrections { project: project.clone(), log },
            );
        }
        self.begin(ctx);
    }

    fn revert_last(&mut self, ctx: &egui::Context) {
        let Some(project) = self.project().map(Path::to_path_buf) else { return };
        let Some(log) = last_log(&project) else {
            self.status = "No hay correcciones registradas para este proyecto.".to_owned();
            return;
        };
        let sure = rfd::MessageDialog::new()
            .set_title("Deshacer")
            .set_description(format!(
                "¿Deshacer la última corrección ({} cambios)?",
                read_changes(&log).len()
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if sure != rfd::MessageDialogResult::Yes || !confirm_koharu_closed() {
            return;
        }
        self.lines.clear();
        self.push_revert(&project, &log);
        self.begin(ctx);
    }

    fn stop(&mut self) {
        self.queue.clear();
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
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt("project")
                        .width(440.0)
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            for (index, path) in self.projects.iter().enumerate() {
                                let name = path.file_name().unwrap_or_default().to_string_lossy();
                                ui.selectable_value(&mut self.selected, Some(index), name);
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
                    }
                    if ui.button("Recargar").clicked() {
                        self.reload_projects();
                    }
                });
            });
            ui.add_space(6.0);

            ui.add_enabled_ui(!busy, |ui| {
                ui.columns(2, |cols| {
                    cols[0].strong("Etapas (en orden)");
                    for ((_, label), on) in STAGES.iter().zip(self.stages.iter_mut()) {
                        cols[0].checkbox(on, *label);
                    }
                    cols[0].checkbox(&mut self.correct, "5. Corregir con modelo local (LM Studio)");

                    cols[1].strong("Opciones");
                    cols[1].horizontal(|ui| {
                        ui.label("Corrector:");
                        ui.add(egui::TextEdit::singleline(&mut self.corrector).desired_width(240.0));
                    });
                    cols[1].horizontal(|ui| {
                        ui.label("Páginas (0 = todas):");
                        ui.add(egui::DragValue::new(&mut self.pages).range(0..=9999));
                    });
                    cols[1].checkbox(
                        &mut self.left_to_right,
                        "Leer de izquierda a derecha (desmarcado = manga)",
                    );
                    cols[1].checkbox(&mut self.review, "Preguntar si conservo las correcciones");
                });
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                let can_run = !busy && self.project().is_some();
                let run = egui::Button::new(egui::RichText::new("▶ Ejecutar").strong());
                if ui.add_enabled(can_run, run).clicked() {
                    self.run(&ctx);
                }
                if ui.add_enabled(busy, egui::Button::new("■ Detener")).clicked() {
                    self.stop();
                }
                let revert = egui::Button::new("Deshacer última corrección");
                if ui.add_enabled(can_run, revert).clicked() {
                    self.revert_last(&ctx);
                }
                if ui.button("Abrir Koharu").clicked() {
                    self.status = match Command::new(&self.koharu).spawn() {
                        Ok(_) => "Koharu abierto: carga el proyecto desde su menú.".to_owned(),
                        Err(error) => format!("No se pudo abrir Koharu: {error}"),
                    };
                }
                if ui.add_enabled(!busy, egui::Button::new("Liberar VRAM")).clicked() {
                    self.lines.clear();
                    let khr = self.khr.clone();
                    self.push("Liberar VRAM", &khr, &["models", "unload"], true, After::Nothing);
                    self.begin(&ctx);
                }
            });
            ui.add_space(6.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if busy {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
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
        .map(|out| String::from_utf8_lossy(&out.stdout).to_lowercase().contains("koharu.exe"))
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

fn read_changes(log: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(log)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|record| record["changes"].as_array().cloned())
        .unwrap_or_default()
}

fn log_base(project: &Path) -> String {
    project
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect()
}

fn log_path(project: &Path) -> PathBuf {
    let _ = std::fs::create_dir_all(LOGS_DIR);
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    Path::new(LOGS_DIR).join(format!("{}__{stamp}.json", log_base(project)))
}

fn last_log(project: &Path) -> Option<PathBuf> {
    let prefix = format!("{}__", log_base(project));
    std::fs::read_dir(LOGS_DIR)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(|stem| stem.strip_prefix(&prefix))
                    .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
        })
        .max_by_key(|path| modified(path))
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
        Box::new(|_| Ok(Box::new(Panel::new()))),
    )
}
