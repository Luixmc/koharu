//! The work around a project that khr does outside the editor: studying it
//! into a ficha and glossary, translating and reviewing it with LM Studio
//! models, and the user's decisions on what those steps propose.

mod files;
mod plan;
mod queue;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, State, WebviewWindow, ipc::Channel};
use tauri_runtime_cef::CefRuntime;

use super::Error;
use super::project::{CurrentProject, ProjectLibrary};
use files::{Correction, ReviewNote, Term, UserNotes, Work};
use plan::{ModelChoices, Plan};
pub(crate) use queue::Queue;
use queue::{QueueEvent, QueueSnapshot, Step};

/// Finished pages go to a folder named after the project in here.
const EXPORT_DIR: &str = r"I:\Koharu\output";

type CommandResult<T> = std::result::Result<T, Error>;

#[tauri::command]
#[specta::specta]
pub(crate) fn subscribe_queue(
    on_event: Channel<QueueEvent>,
    queue: State<'_, Queue>,
) -> CommandResult<QueueSnapshot> {
    Ok(queue.subscribe(on_event))
}

#[tauri::command]
#[specta::specta]
pub(crate) fn enqueue(
    plan: Plan,
    handle: AppHandle<CefRuntime>,
    library: State<'_, ProjectLibrary>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    let project = library.path(&plan.project)?;
    let steps = plan::steps(&plan, &project)?;
    queue.push(&handle, steps);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn learn_from_corrections(
    project: String,
    model: String,
    handle: AppHandle<CefRuntime>,
    library: State<'_, ProjectLibrary>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    let model = model.trim();
    if model.is_empty() {
        return Err(anyhow!("Elige el modelo de traducción.").into());
    }
    let project = library.path(&project)?;
    queue.push(&handle, plan::learn(&project, model));
    Ok(())
}

/// Renders the finished pages with khr one at a time; exporting every page
/// from memory took the PC down.
#[tauri::command]
#[specta::specta]
pub(crate) fn export_pages(
    project: String,
    handle: AppHandle<CefRuntime>,
    library: State<'_, ProjectLibrary>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    let path = library.path(&project)?;
    let out = PathBuf::from(EXPORT_DIR)
        .join(&project)
        .display()
        .to_string();
    let project_arg = path.display().to_string();
    queue.push(
        &handle,
        vec![Step::khr(
            Some(&path),
            format!("Exportar páginas → {out}"),
            &["exportar", "--project", &project_arg, "--out", &out],
        )],
    );
    Ok(())
}

/// Proposals made before `revisar` saved the pages around them: reads the
/// pages now (quick, no model).
#[tauri::command]
#[specta::specta]
pub(crate) fn read_page_context(
    project: String,
    left_to_right: bool,
    handle: AppHandle<CefRuntime>,
    library: State<'_, ProjectLibrary>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    let path = library.path(&project)?;
    let project_arg = path.display().to_string();
    let mut args = vec!["contexto", "--project", &project_arg];
    if left_to_right {
        args.push("--left-to-right");
    }
    queue.push(
        &handle,
        vec![Step::khr(Some(&path), "Leer las páginas de las correcciones", &args).may_fail()],
    );
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn apply_corrections(
    project: String,
    handle: AppHandle<CefRuntime>,
    library: State<'_, ProjectLibrary>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    let path = library.path(&project)?;
    let project_arg = path.display().to_string();
    queue.push(
        &handle,
        vec![Step::khr(
            Some(&path),
            "Aplicar correcciones aprobadas",
            &["aplicar", "--project", &project_arg],
        )],
    );
    Ok(())
}

/// Frees the LM Studio models.
#[tauri::command]
#[specta::specta]
pub(crate) fn free_memory(
    handle: AppHandle<CefRuntime>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    queue.push(
        &handle,
        vec![Step::khr(None, "Liberar VRAM", &["models", "unload"]).may_fail()],
    );
    Ok(())
}

/// A new project named after a folder, with its images as pages.
#[tauri::command]
#[specta::specta]
pub(crate) async fn create_project_from_folder(
    window: WebviewWindow<CefRuntime>,
    handle: AppHandle<CefRuntime>,
    library: State<'_, ProjectLibrary>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Ok(());
    };
    let name = folder.file_name();
    let project = library.path(&name)?;
    if project.exists() {
        return Err(anyhow!("Ya existe un proyecto llamado {name}.").into());
    }
    let (project_arg, folder_arg) = (
        project.display().to_string(),
        folder.path().display().to_string(),
    );
    queue.push(
        &handle,
        vec![
            Step::khr(
                Some(&project),
                "Crear el proyecto",
                &[
                    "crear",
                    "--project",
                    &project_arg,
                    "--imagenes",
                    &folder_arg,
                ],
            )
            .then(queue::After::Created),
        ],
    );
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn stop_queue(queue: State<'_, Queue>) -> CommandResult<()> {
    queue.stop();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn resume_queue(
    handle: AppHandle<CefRuntime>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    queue.resume(&handle);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn discard_unfinished_queue(queue: State<'_, Queue>) -> CommandResult<()> {
    queue.discard_unfinished();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn cancel_queued(project: String, queue: State<'_, Queue>) -> CommandResult<()> {
    queue.cancel(&project);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn set_shutdown_when_done(shutdown: bool, queue: State<'_, Queue>) -> CommandResult<()> {
    queue.set_shutdown_when_done(shutdown);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn get_work(project: String, library: State<'_, ProjectLibrary>) -> CommandResult<Work> {
    Ok(files::read(&files::dir(&library.path(&project)?)))
}

#[tauri::command]
#[specta::specta]
pub(crate) fn save_user_notes(
    project: String,
    notes: UserNotes,
    library: State<'_, ProjectLibrary>,
) -> CommandResult<()> {
    files::write_notes(&files::dir(&library.path(&project)?), &notes)?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn decide_terms(
    project: String,
    terms: Vec<Term>,
    approve: bool,
    library: State<'_, ProjectLibrary>,
) -> CommandResult<()> {
    files::decide_terms(&files::dir(&library.path(&project)?), &terms, approve)?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn add_term(
    project: String,
    term: Term,
    library: State<'_, ProjectLibrary>,
) -> CommandResult<()> {
    if term.source.trim().is_empty() || term.target.trim().is_empty() {
        return Err(anyhow!("El término necesita el original y la traducción.").into());
    }
    let term = Term {
        source: term.source.trim().to_owned(),
        target: term.target.trim().to_owned(),
        ..term
    };
    files::add_term(&files::dir(&library.path(&project)?), &term)?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn promote_term(term: Term) -> CommandResult<()> {
    files::promote_term(&term)?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn decide_corrections(
    project: String,
    corrections: Vec<Correction>,
    approve: bool,
    library: State<'_, ProjectLibrary>,
) -> CommandResult<()> {
    files::decide_corrections(&files::dir(&library.path(&project)?), &corrections, approve)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkFile {
    Ficha,
    Glossary,
    GlobalGlossary,
    Folder,
}

/// Opens one of the work's files in the program Windows uses for it.
#[tauri::command]
#[specta::specta]
pub(crate) fn open_work_file(
    project: String,
    file: WorkFile,
    library: State<'_, ProjectLibrary>,
) -> CommandResult<()> {
    let dir = files::dir(&library.path(&project)?);
    let path = match file {
        WorkFile::Ficha => dir.join("ficha.md"),
        WorkFile::Glossary => dir.join("glosario.tsv"),
        WorkFile::GlobalGlossary => PathBuf::from(files::GLOBAL_GLOSSARY),
        WorkFile::Folder => dir,
    };
    if !path.exists() {
        return Err(anyhow!("Aún no existe: {}", path.display()).into());
    }
    open::that_detached(&path).with_context(|| format!("failed to open {}", path.display()))?;
    Ok(())
}

/// Runs khr with `args` and reads the JSON it prints.
async fn khr_json<T: serde::de::DeserializeOwned>(
    queue: &Queue,
    args: &[&str],
) -> anyhow::Result<T> {
    let mut command = tokio::process::Command::new(queue.khr());
    command.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let output = command.output().await.context("khr could not be started")?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{}", error.lines().last().unwrap_or("error desconocido"));
    }
    serde_json::from_slice(&output.stdout).context("khr printed no JSON")
}

/// What an e-hentai gallery says about the work.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Gallery {
    pub title: String,
    pub original_title: String,
    pub tags: Vec<String>,
}

/// `khr etiquetas` output.
#[derive(Deserialize)]
struct GalleryOutput {
    titulo: String,
    titulo_original: String,
    etiquetas: Vec<String>,
}

/// Asks `khr etiquetas` for a gallery's title and tags, given its number or
/// its e-hentai or exhentai link.
#[tauri::command]
#[specta::specta]
pub(crate) async fn fetch_gallery(
    gallery: String,
    queue: State<'_, Queue>,
) -> CommandResult<Gallery> {
    let output: GalleryOutput =
        khr_json(&queue, &["etiquetas", "--galeria", gallery.trim()]).await?;
    Ok(Gallery {
        title: output.titulo,
        original_title: output.titulo_original,
        tags: output.etiquetas,
    })
}

/// The LLMs LM Studio has downloaded, by the key `lms load` takes.
#[tauri::command]
#[specta::specta]
pub(crate) async fn get_llm_models(queue: State<'_, Queue>) -> CommandResult<Vec<String>> {
    let mut command = tokio::process::Command::new(queue.lms());
    command.args(["ls", "--json"]).stdin(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let output = command
        .output()
        .await
        .context("LM Studio's lms could not be started")?;
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let mut models: Vec<String> = listed
        .iter()
        .filter(|model| model.get("type").and_then(|kind| kind.as_str()) == Some("llm"))
        .filter_map(|model| model.get("modelKey")?.as_str().map(str::to_owned))
        .collect();
    models.sort();
    Ok(models)
}

#[tauri::command]
#[specta::specta]
pub(crate) fn get_model_choices() -> CommandResult<ModelChoices> {
    Ok(ModelChoices::load())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn save_model_choices(choices: ModelChoices) -> CommandResult<()> {
    choices.save()?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_review_notes(
    project: State<'_, CurrentProject>,
) -> CommandResult<Vec<ReviewNote>> {
    let path = project
        .project
        .lock()
        .await
        .as_ref()
        .map(|project| project.path.clone());
    Ok(path
        .map(|path| files::review_notes(&files::dir(&path)))
        .unwrap_or_default())
}

/// A model of khr's catalog of suggested LM Studio models.
#[derive(Clone, Debug, Serialize, Type)]
pub struct CatalogModel {
    pub id: String,
    pub tasks: Vec<String>,
    pub gb: f64,
    /// Per source language; "?" until measured.
    pub languages: BTreeMap<String, String>,
    /// What the test bench measured, per task.
    pub measured: BTreeMap<String, String>,
    pub note: String,
    pub installed: bool,
}

/// What khr recommends: the OCR each source language uses and its catalog.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Recommendations {
    pub ocr: BTreeMap<String, String>,
    pub models: Vec<CatalogModel>,
}

/// `khr models recommend` output.
#[derive(Deserialize)]
struct RecommendationsOutput {
    ocr: BTreeMap<String, String>,
    modelos: Vec<CatalogOutput>,
}

#[derive(Deserialize)]
struct CatalogOutput {
    id: String,
    tareas: Vec<String>,
    gb: f64,
    idiomas: BTreeMap<String, String>,
    medido: BTreeMap<String, String>,
    nota: String,
    instalado: bool,
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_recommendations(queue: State<'_, Queue>) -> CommandResult<Recommendations> {
    let output: RecommendationsOutput = khr_json(&queue, &["models", "recommend"]).await?;
    Ok(Recommendations {
        ocr: output.ocr,
        models: output
            .modelos
            .into_iter()
            .map(|model| CatalogModel {
                id: model.id,
                tasks: model.tareas,
                gb: model.gb,
                languages: model.idiomas,
                measured: model.medido,
                note: model.nota,
                installed: model.instalado,
            })
            .collect(),
    })
}

/// Fetches a catalog model into the LM Studio library, as a queue step so
/// its progress shows in the log.
#[tauri::command]
#[specta::specta]
pub(crate) fn download_model(
    id: String,
    handle: AppHandle<CefRuntime>,
    queue: State<'_, Queue>,
) -> CommandResult<()> {
    queue.push(
        &handle,
        vec![Step::khr(
            None,
            format!("Descargar {id}"),
            &["models", "download", &id],
        )],
    );
    Ok(())
}
