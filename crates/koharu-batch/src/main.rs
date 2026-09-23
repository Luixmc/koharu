//! Headless tooling for Koharu projects.
//!
//! Reads and writes the same `.khrproj` format as the desktop application, so a
//! project can move between this tool and the editor without conversion.

use std::collections::BTreeMap;
use std::path::PathBuf;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use koharu_pipeline::{
    Committer, Pipeline, PipelineConfig, Progress, Request, Scope, Stage, StageOutput,
};
use koharu_scene::{Authored, Session, SourceText, Translation};
use koharu_translator::ProvidersConfig;

#[derive(Debug, Parser)]
#[command(name = "khr", version, about = "Headless tooling for Koharu projects")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Score a model against the bundled per-language test cases.
    Bench {
        /// Base URL of the OpenAI-compatible server hosting the model.
        #[arg(long, default_value = "http://localhost:1234")]
        base_url: String,

        /// Model identifier, as the server reports it.
        #[arg(long)]
        model: String,

        /// File holding the correction instructions.
        #[arg(long, value_name = "FILE")]
        instructions: PathBuf,

        /// Comma-separated source languages: en, ja, ko, zh.
        #[arg(long)]
        languages: Option<String>,

        /// Repeat to measure how stable the model is.
        #[arg(long, default_value_t = 1)]
        runs: usize,
    },

    /// Print the recognized text of a project without modifying it.
    Dump {
        #[arg(short, long, value_name = "KHRPROJ")]
        project: PathBuf,

        /// Stop after this many pages.
        #[arg(long, value_name = "N")]
        pages: Option<usize>,

        /// Emit JSON instead of a readable report.
        #[arg(long)]
        json: bool,
    },

    /// Rewrite existing translations with a corrector model.
    Post {
        #[arg(short, long, value_name = "KHRPROJ")]
        project: PathBuf,

        /// Limit to the first N pages.
        #[arg(long, value_name = "N")]
        pages: Option<usize>,

        /// Base URL of the OpenAI-compatible server hosting the corrector.
        #[arg(long, default_value = "http://localhost:1234")]
        base_url: String,

        /// Corrector model identifier, as the server reports it.
        #[arg(long)]
        model: String,

        /// File holding the correction instructions.
        #[arg(long, value_name = "FILE")]
        instructions: PathBuf,

        /// Blocks sent per request.
        #[arg(long, default_value_t = 8)]
        batch: usize,

        /// Report changes without writing them.
        #[arg(long)]
        dry_run: bool,

        /// Where to record what was replaced, for `khr revert`.
        #[arg(long, value_name = "FILE")]
        log: Option<PathBuf>,
    },

    /// Inspect, load and unload models on the local LM Studio server.
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },

    /// Restore translations replaced by a previous `khr post` run.
    Revert {
        #[arg(short, long, value_name = "KHRPROJ")]
        project: PathBuf,

        /// Log written by the run to undo.
        #[arg(long, value_name = "FILE")]
        log: PathBuf,

        /// Report what would be restored without writing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run pipeline stages over a project, writing results back into it.
    Run {
        #[arg(short, long, value_name = "KHRPROJ")]
        project: PathBuf,

        /// Comma-separated stages: detection, ocr, translation, inpainting.
        #[arg(long, default_value = "detection,ocr,inpainting")]
        stages: String,

        /// Limit to the first N pages.
        #[arg(long, value_name = "N")]
        pages: Option<usize>,

        /// Force CPU execution.
        #[arg(long)]
        cpu: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ModelsAction {
    /// List installed models, or only those currently resident.
    List {
        /// Show what is loaded and how much memory it holds.
        #[arg(long)]
        loaded: bool,
    },

    /// Load a model, making it available for `khr post`.
    Load {
        model: String,

        /// Context window to allocate.
        #[arg(long, value_name = "TOKENS")]
        context: Option<u32>,
    },

    /// Release a model, freeing its memory.
    Unload {
        /// Model to release; every loaded model when omitted.
        model: Option<String>,
    },

    /// Show the suggested models and whether they are installed.
    Catalog {
        /// Restrict to models suited to a task: traducir or corregir.
        #[arg(long)]
        task: Option<String>,
    },

    /// Fetch a catalog model into the LM Studio library.
    Download {
        /// Catalog identifier, as `khr models catalog` prints it.
        id: String,
    },

    /// Delete a model's weights from disk.
    Remove {
        /// Catalog identifier, or part of an installed file name.
        id: String,
    },

    /// Add a .gguf obtained elsewhere to the LM Studio library.
    Import {
        file: PathBuf,

        /// Publisher directory to file it under.
        #[arg(long, default_value = "local")]
        publisher: String,

        /// Repository directory; taken from the file name when omitted.
        #[arg(long)]
        name: Option<String>,
    },
}

/// Writes each finished stage back into the open session.
struct SessionCommitter<'a>(&'a mut Session);

#[async_trait::async_trait]
impl Committer for SessionCommitter<'_> {
    async fn commit(&mut self, output: StageOutput) -> Result<koharu_scene::Snapshot> {
        Ok(self.0.commit(output.patch).await?.snapshot)
    }
}

fn parse_stages(value: &str) -> Result<Vec<Stage>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| match name.to_ascii_lowercase().as_str() {
            "detection" => Ok(Stage::Detection),
            "ocr" => Ok(Stage::Ocr),
            "translation" => Ok(Stage::Translation),
            "inpainting" => Ok(Stage::Inpainting),
            other => Err(anyhow::anyhow!(
                "unknown stage `{other}`; expected detection, ocr, translation or inpainting"
            )),
        })
        .collect()
}

/// The runtime downloads models on first use, so a cold start can fail on a
/// slow network before it succeeds.
async fn initialize_with_retry() {
    let mut delay = Duration::from_secs(1);
    let mut attempt = 0_u64;
    loop {
        attempt += 1;
        match koharu_ml::init().await {
            Ok(()) => return,
            Err(error) => {
                let wait = delay + Duration::from_millis((attempt.wrapping_mul(137)) % 251);
                eprintln!(
                    "runtime initialization attempt {attempt} failed: {error}; retrying in {:.1}s",
                    wait.as_secs_f64()
                );
                tokio::time::sleep(wait).await;
                delay = delay.saturating_mul(2).min(Duration::from_secs(30));
            }
        }
    }
}

async fn run(project: &PathBuf, stages: &str, limit: Option<usize>, cpu: bool) -> Result<()> {
    let stages = parse_stages(stages)?;
    anyhow::ensure!(!stages.is_empty(), "no stages selected");

    let mut session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;

    let mut pages: Vec<_> = session.snapshot().pages().map(|page| page.id()).collect();
    if let Some(limit) = limit {
        pages.truncate(limit);
    }
    anyhow::ensure!(!pages.is_empty(), "project has no pages");
    eprintln!(
        "running {} stage(s) over {} page(s)",
        stages.len(),
        pages.len()
    );

    initialize_with_retry().await;

    // One stage at a time across every page, rather than every stage per page.
    // Each stage owns its own pipeline, so dropping it releases that stage's
    // weights before the next stage loads its own: only one model is resident
    // at a time. Load cost is paid once per stage either way, and the
    // accelerator lane already serializes GPU work, so interleaving stages buys
    // residency pressure without buying throughput.
    let total = Instant::now();
    for stage in stages {
        let started = Instant::now();
        // Load the same configuration the desktop application reads, so stages
        // run with the selected models, prompts and provider credentials
        // instead of defaults.
        let pipeline = Pipeline::from_config(
            koharu_config::load::<PipelineConfig>("pipeline")?,
            koharu_config::load::<ProvidersConfig>("providers")?,
            koharu_ml::device(cpu),
        )?;
        let snapshot = session.snapshot();
        let mut committer = SessionCommitter(&mut session);
        pipeline
            .execute(
                snapshot,
                Request {
                    operation: koharu_pipeline::Operation::Only { stage },
                    scope: Scope::Pages(pages.clone()),
                    progress: Some(Arc::new(|event| {
                        if let Progress::Finished { stage, elapsed, .. } = event {
                            eprintln!("  {stage} {:.2}s", elapsed.as_secs_f64());
                        }
                    })),
                    ..Request::default()
                },
                &mut committer,
            )
            .await?;
        drop(pipeline);
        eprintln!(
            "{stage}: {} page(s) in {:.2}s (model released)",
            pages.len(),
            started.elapsed().as_secs_f64()
        );
    }
    eprintln!("all stages finished in {:.2}s", total.elapsed().as_secs_f64());
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Dump {
            project,
            pages,
            json,
        } => dump(&project, pages, json).await,
        Command::Run {
            project,
            stages,
            pages,
            cpu,
        } => run(&project, &stages, pages, cpu).await,
        Command::Post {
            project,
            pages,
            base_url,
            model,
            instructions,
            batch,
            dry_run,
            log,
        } => {
            post(
                &project,
                pages,
                &base_url,
                &model,
                instructions,
                batch,
                dry_run,
                log,
            )
            .await
        }
        Command::Bench {
            base_url,
            model,
            instructions,
            languages,
            runs,
        } => bench(&base_url, &model, instructions, languages, runs).await,
        Command::Models { action } => models(action).await,
        Command::Revert {
            project,
            log,
            dry_run,
        } => revert(&project, &log, dry_run).await,
    }
}

#[derive(serde::Serialize)]
struct DumpedText {
    entity: String,
    source: String,
    translation: Option<String>,
}

#[derive(serde::Serialize)]
struct DumpedPage {
    index: usize,
    label: String,
    texts: Vec<DumpedText>,
}

#[derive(serde::Serialize)]
struct DumpReport {
    project: String,
    pages: usize,
    shown: usize,
    texts: usize,
    outside: usize,
    page_list: Vec<DumpedPage>,
}

async fn dump(project: &PathBuf, limit: Option<usize>, json: bool) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();
    let total_pages = snapshot.pages().len();

    let mut pages: Vec<DumpedPage> = Vec::new();
    let mut index_of = BTreeMap::new();
    for (index, page) in snapshot.pages().enumerate() {
        if limit.is_some_and(|limit| index >= limit) {
            break;
        }
        index_of.insert(page.id(), index);
        pages.push(DumpedPage {
            index,
            label: page.page()?.label,
            texts: Vec::new(),
        });
    }

    // Text lives on its own entity, so walk up the hierarchy until a page is
    // reached. Text whose ancestors hold no listed page sits outside the
    // requested window; counting it separately keeps a page filtered out by
    // --pages distinguishable from a page that produced nothing.
    let mut outside = 0_usize;
    let mut total_texts = 0_usize;
    for entity in snapshot.entities_with::<SourceText>()? {
        let id = entity.id();
        let content = snapshot.text_content(id)?;
        let Some(source) = content.source()? else {
            continue;
        };
        total_texts += 1;
        let text = DumpedText {
            entity: format!("{id:?}"),
            source: source.text.value,
            translation: content
                .translation()?
                .map(|value: Translation| value.text.value),
        };

        let mut cursor = Some(id);
        let mut placed = false;
        while let Some(current) = cursor {
            if let Some(&index) = index_of.get(&current) {
                pages[index].texts.push(text);
                placed = true;
                break;
            }
            cursor = snapshot.parent(current)?;
        }
        if !placed {
            outside += 1;
        }
    }

    let shown: usize = pages.iter().map(|page| page.texts.len()).sum();

    if json {
        let report = DumpReport {
            project: project.display().to_string(),
            pages: total_pages,
            shown,
            texts: total_texts,
            outside,
            page_list: pages,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("project: {}", project.display());
    println!("pages:   {total_pages} total, {} shown", pages.len());
    println!("texts:   {total_texts} total, {shown} in shown pages, {outside} outside");
    println!();
    for page in &pages {
        let translated = page
            .texts
            .iter()
            .filter(|text| text.translation.is_some())
            .count();
        let empty = page
            .texts
            .iter()
            .filter(|text| text.source.trim().is_empty())
            .count();
        println!(
            "--- page {} [{}]: {} block(s), {translated} translated, {empty} empty",
            page.index,
            page.label,
            page.texts.len()
        );
        for text in &page.texts {
            println!("    {}", text.source.replace('\n', " / "));
            if let Some(translation) = &text.translation {
                println!("      -> {}", translation.replace('\n', " / "));
            }
        }
    }
    Ok(())
}

/// Tells the corrector how to frame a batch.
///
/// The correction prompt describes the editorial task; this appendix only fixes
/// the wire format. Both travel in the system message so the model never sees
/// the framing mixed into the text it must edit, which is what let an earlier
/// in-band marker leak into the replies.
const FORMAT_APPENDIX: &str = "\n\nFORMATO DE INTERCAMBIO\n\n\
ENTRADA: un objeto JSON con la clave \"bloques\". Cada bloque trae:\n\
- \"id\": identificador\n\
- \"es\": el texto en espanol que debes corregir\n\
- \"ref\": el texto de partida, SOLO COMO CONSULTA INTERNA\n\n\
SALIDA: para cada bloque, \"id\" y \"es_corregido\".\n\n\
QUE ES \"es_corregido\":\n\
Es el globo de dialogo listo para imprimir en la pagina. Contiene\n\
EXCLUSIVAMENTE el texto en espanol que leera el lector.\n\n\
PROHIBIDO en \"es_corregido\":\n\
- Copiar o anexar el contenido de \"ref\"\n\
- Escribir \"ref:\", \"es:\", comillas de encuadre o cualquier etiqueta\n\
- Anadir comentarios, notas o explicaciones\n\n\
Conserva la puntuacion final del texto: si \"es\" termina en punto, signo de\n\
exclamacion, interrogacion o puntos suspensivos, \"es_corregido\" tambien.\n\
Si \"es\" ya esta bien, repitelo identico.\n\
Devuelve exactamente un elemento por cada bloque recibido, con su \"id\".";

#[derive(serde::Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(serde::Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    temperature: f32,
    stream: bool,
    response_format: serde_json::Value,
}

/// Constrains the reply to the expected shape.
///
/// Asking for the shape in the prompt alone was not enough: the model misspelled
/// the key on one block and emitted invalid JSON on another within a single
/// reply. A served schema removes both failures, leaving only the editorial
/// quality of the text to judge.
fn correction_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "correcciones",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "bloques": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "integer"},
                                "es_corregido": {"type": "string"}
                            },
                            "required": ["id", "es_corregido"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["bloques"],
                "additionalProperties": false
            }
        }
    })
}

#[derive(serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(serde::Deserialize)]
struct ChatChoice {
    message: ChatContent,
}

#[derive(serde::Deserialize)]
struct ChatContent {
    content: String,
}

#[derive(serde::Deserialize)]
struct CorrectionBatch {
    bloques: Vec<CorrectionItem>,
}

#[derive(serde::Deserialize)]
struct CorrectionItem {
    id: usize,
    es_corregido: String,
}

/// Rejects a reply that cannot be an edit of the given translation.
///
/// A corrector that collapses a block to a fraction of its length, answers with
/// an empty string, or echoes a bracketed label has failed to follow the format
/// rather than improved the text. Writing such a reply would replace a usable
/// translation with noise, so it is dropped and reported.
fn plausible_correction(previous: &str, reference: &str, candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    if candidate.starts_with('[') && candidate.ends_with(']') {
        return false;
    }
    // The reference travels with each block for the corrector to consult, and a
    // reply that carries it back has echoed the request instead of editing the
    // translation. A short reference can legitimately coincide with the
    // corrected wording, so only a substantial one counts as contamination.
    let reference = reference.trim();
    if reference.chars().count() >= 8 && candidate.contains(reference) {
        return false;
    }
    let previous_length = previous.trim().chars().count();
    let candidate_length = candidate.chars().count();
    // Short blocks legitimately change length a lot; long ones should not.
    previous_length < 12 || candidate_length.saturating_mul(3) >= previous_length
}

/// Extracts the JSON object from a reply that may be wrapped in prose or fences.
fn extract_json(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    (end > start).then(|| &reply[start..=end])
}

#[allow(clippy::too_many_arguments)]
async fn post(
    project: &PathBuf,
    limit: Option<usize>,
    base_url: &str,
    model: &str,
    instructions: PathBuf,
    batch: usize,
    dry_run: bool,
    log: Option<PathBuf>,
) -> Result<()> {
    anyhow::ensure!(batch > 0, "batch size must be at least 1");
    let instructions = std::fs::read_to_string(&instructions)
        .with_context(|| format!("failed to read {}", instructions.display()))?;
    let system = format!("{}{FORMAT_APPENDIX}", instructions.trim());

    let mut session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();

    let allowed = limit.map(|limit| {
        snapshot
            .pages()
            .take(limit)
            .map(|page| page.id())
            .collect::<std::collections::BTreeSet<_>>()
    });

    // Only entities carrying both texts qualify: the corrector rewrites an
    // existing translation and never creates one.
    let mut pending = Vec::new();
    for entity in snapshot.entities_with::<SourceText>()? {
        let id = entity.id();
        let content = snapshot.text_content(id)?;
        let (Some(source), Some(translation)) = (content.source()?, content.translation()?) else {
            continue;
        };
        if let Some(allowed) = &allowed {
            let mut cursor = Some(id);
            let mut inside = false;
            while let Some(current) = cursor {
                if allowed.contains(&current) {
                    inside = true;
                    break;
                }
                cursor = snapshot.parent(current)?;
            }
            if !inside {
                continue;
            }
        }
        pending.push((id, source.text.value, translation));
    }

    if pending.is_empty() {
        eprintln!("nothing to correct: no entity carries both a source text and a translation");
        return Ok(());
    }
    eprintln!("correcting {} block(s) in batches of {batch}", pending.len());

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

    let mut accepted = Vec::new();
    let mut rejected = 0_usize;
    for (index, chunk) in pending.chunks(batch).enumerate() {
        let payload = serde_json::json!({
            "bloques": chunk
                .iter()
                .enumerate()
                .map(|(id, (_, source, translation))| serde_json::json!({
                    "id": id,
                    "es": translation.text.value,
                    "ref": source,
                }))
                .collect::<Vec<_>>(),
        });
        let user = serde_json::to_string(&payload)?;
        let request = ChatRequest {
            model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: &system,
                },
                ChatMessage {
                    role: "user",
                    content: &user,
                },
            ],
            temperature: 0.2,
            stream: false,
            response_format: correction_schema(),
        };

        let response: ChatResponse = client
            .post(&endpoint)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("batch {index}: request to {endpoint} failed"))?
            .error_for_status()
            .with_context(|| format!("batch {index}: the corrector returned an error"))?
            .json()
            .await
            .with_context(|| format!("batch {index}: malformed response"))?;

        let reply = response
            .choices
            .first()
            .map(|choice| choice.message.content.as_str())
            .unwrap_or_default();
        let Some(json) = extract_json(reply) else {
            eprintln!("  batch {index}: reply contained no JSON object, skipped");
            rejected += chunk.len();
            continue;
        };
        let parsed: CorrectionBatch = match serde_json::from_str(json) {
            Ok(parsed) => parsed,
            Err(error) => {
                eprintln!("  batch {index}: unparsable reply ({error}), skipped");
                rejected += chunk.len();
                continue;
            }
        };

        let mut applied = 0_usize;
        for item in parsed.bloques {
            let Some((id, source, translation)) = chunk.get(item.id) else {
                continue;
            };
            let previous = &translation.text.value;
            if !plausible_correction(previous, source, &item.es_corregido) {
                rejected += 1;
                continue;
            }
            if previous.trim() == item.es_corregido.trim() {
                continue;
            }
            accepted.push((*id, translation.clone(), item.es_corregido));
            applied += 1;
        }
        eprintln!("  batch {index}: {applied} correction(s) of {}", chunk.len());
    }

    eprintln!(
        "{} correction(s) accepted, {rejected} rejected as implausible",
        accepted.len()
    );

    if dry_run {
        for (_, previous, candidate) in &accepted {
            println!("- {}", previous.text.value.replace('\n', " / "));
            println!("+ {}", candidate.replace('\n', " / "));
        }
        eprintln!("dry run: the project was not modified");
        return Ok(());
    }
    if accepted.is_empty() {
        return Ok(());
    }

    let patch = session.snapshot().patch(|edit| {
        for (id, previous, candidate) in &accepted {
            edit.set(
                *id,
                &Translation {
                    text: Authored {
                        value: candidate.clone(),
                        origin: previous.text.origin.clone(),
                    },
                    language: previous.language.clone(),
                },
            )?;
        }
        Ok(())
    })?;
    session.commit(patch).await?;
    eprintln!(
        "wrote {} correction(s) to {}",
        accepted.len(),
        project.display()
    );

    // The scene keeps an undo history only for the lifetime of an open session,
    // so once this process exits nothing in the project can roll these edits
    // back. A corrector is a generative model rewriting text that was already
    // valid, and a subtly wrong rewrite passes every guard above, so the
    // replaced text is recorded on disk for `khr revert`.
    if let Some(path) = log {
        let record = RevertLog {
            project: project.display().to_string(),
            model: model.to_owned(),
            changes: accepted
                .iter()
                .map(|(id, previous, candidate)| RevertEntry {
                    entity: *id,
                    before: previous.text.value.clone(),
                    after: candidate.clone(),
                })
                .collect(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&record)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        eprintln!("recorded {} replacement(s) in {}", record.changes.len(), path.display());
    } else {
        eprintln!("no --log given: these replacements cannot be reverted automatically");
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RevertEntry {
    entity: koharu_scene::EntityId,
    before: String,
    after: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RevertLog {
    project: String,
    model: String,
    changes: Vec<RevertEntry>,
}

async fn revert(project: &PathBuf, log: &PathBuf, dry_run: bool) -> Result<()> {
    let record: RevertLog = serde_json::from_str(
        &std::fs::read_to_string(log)
            .with_context(|| format!("failed to read {}", log.display()))?,
    )
    .with_context(|| format!("failed to parse {}", log.display()))?;
    eprintln!(
        "log holds {} replacement(s) made by {}",
        record.changes.len(),
        record.model
    );

    let mut session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();

    // Restore only where the text still matches what the run wrote. Anything
    // edited by hand since then is left alone: reverting a correction must not
    // silently discard later work.
    let mut restorable = Vec::new();
    let mut diverged = 0_usize;
    let mut missing = 0_usize;
    for entry in &record.changes {
        let Ok(content) = snapshot.text_content(entry.entity) else {
            missing += 1;
            continue;
        };
        let Some(translation) = content.translation()? else {
            missing += 1;
            continue;
        };
        if translation.text.value.trim() != entry.after.trim() {
            diverged += 1;
            continue;
        }
        restorable.push((entry, translation));
    }

    eprintln!(
        "{} restorable, {diverged} changed since the run, {missing} no longer present",
        restorable.len()
    );
    if dry_run {
        eprintln!("dry run: the project was not modified");
        return Ok(());
    }
    if restorable.is_empty() {
        return Ok(());
    }

    let patch = session.snapshot().patch(|edit| {
        for (entry, translation) in &restorable {
            edit.set(
                entry.entity,
                &Translation {
                    text: Authored {
                        value: entry.before.clone(),
                        origin: translation.text.origin.clone(),
                    },
                    language: translation.language.clone(),
                },
            )?;
        }
        Ok(())
    })?;
    session.commit(patch).await?;
    eprintln!("restored {} translation(s)", restorable.len());
    Ok(())
}

/// Locates the LM Studio CLI.
///
/// The installer puts it in the user profile rather than on PATH, so the well
/// known location is tried before falling back to PATH for a custom install.
fn lms_command() -> std::process::Command {
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let bundled = PathBuf::from(home).join(".lmstudio").join("bin").join("lms");
        let bundled = bundled.with_extension(std::env::consts::EXE_EXTENSION);
        if bundled.exists() {
            return std::process::Command::new(bundled);
        }
    }
    std::process::Command::new("lms")
}

fn run_lms(arguments: &[&str]) -> Result<String> {
    let output = lms_command()
        .args(arguments)
        .output()
        .context("failed to run the LM Studio CLI; is LM Studio installed?")?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "lms {} failed: {}",
            arguments.join(" "),
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        );
    }
    Ok(stdout)
}

/// Strips the spinner frames and cursor escapes the CLI writes while loading, so
/// a progress animation does not end up in the report.
fn clean_output(raw: &str) -> String {
    raw.lines()
        .map(|line| {
            line.split(['\r', '\u{1b}'])
                .next_back()
                .unwrap_or(line)
                .trim_end()
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn models(action: ModelsAction) -> Result<()> {
    match action {
        ModelsAction::List { loaded } => {
            let raw = if loaded {
                run_lms(&["ps"])?
            } else {
                run_lms(&["ls"])?
            };
            println!("{}", clean_output(&raw));
        }
        ModelsAction::Load { model, context } => {
            let context = context.unwrap_or(8192).to_string();
            eprintln!("loading {model}...");
            let raw = run_lms(&[
                "load",
                &model,
                "--gpu",
                "max",
                "--context-length",
                &context,
                "-y",
            ])?;
            println!("{}", clean_output(&raw));
        }
        ModelsAction::Unload { model } => {
            let raw = match &model {
                Some(model) => run_lms(&["unload", model])?,
                None => run_lms(&["unload", "--all"])?,
            };
            println!("{}", clean_output(&raw));
        }
        ModelsAction::Catalog { task } => show_catalog(task.as_deref())?,
        ModelsAction::Download { id } => download_model(&id).await?,
        ModelsAction::Remove { id } => remove_model(&id)?,
        ModelsAction::Import {
            file,
            publisher,
            name,
        } => import_model(&file, &publisher, name)?,
    }
    Ok(())
}

/// Suggested models, shipped with the binary so the catalog is available
/// offline and stays versioned with the code that reads it.
const MODEL_CATALOG: &str = include_str!("../assets/model-catalog.json");

#[derive(serde::Deserialize)]
struct CatalogEntry {
    id: String,
    repo: String,
    archivo: String,
    gb: f64,
    tareas: Vec<String>,
    #[serde(default)]
    medido: BTreeMap<String, String>,
    nota: String,
}

#[derive(serde::Deserialize)]
struct Catalog {
    modelos: Vec<CatalogEntry>,
}

fn catalog() -> Result<Catalog> {
    serde_json::from_str(MODEL_CATALOG).context("the bundled model catalog is malformed")
}

/// Where LM Studio expects a model: a publisher directory holding the weights.
fn models_root() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .context("could not locate the user profile")?;
    Ok(PathBuf::from(home).join(".lmstudio").join("models"))
}

fn installed_path(entry: &CatalogEntry) -> Result<PathBuf> {
    let (publisher, repository) = entry
        .repo
        .split_once('/')
        .context("catalog entries use publisher/repository")?;
    Ok(models_root()?.join(publisher).join(repository).join(&entry.archivo))
}

fn show_catalog(task: Option<&str>) -> Result<()> {
    let catalog = catalog()?;
    for entry in &catalog.modelos {
        if let Some(task) = task
            && !entry.tareas.iter().any(|value| value == task)
        {
            continue;
        }
        let installed = installed_path(entry).map(|path| path.exists()).unwrap_or(false);
        println!(
            "{}  [{}]  {:.1} GB  {}",
            entry.id,
            entry.tareas.join(", "),
            entry.gb,
            if installed { "INSTALADO" } else { "" }
        );
        for (task, verdict) in &entry.medido {
            println!("    medido ({task}): {verdict}");
        }
        println!("    {}", entry.nota);
    }
    Ok(())
}

async fn download_model(id: &str) -> Result<()> {
    let catalog = catalog()?;
    let entry = catalog
        .modelos
        .iter()
        .find(|entry| entry.id == id)
        .with_context(|| format!("`{id}` is not in the catalog; run `khr models catalog`"))?;
    let target = installed_path(entry)?;
    if target.exists() {
        eprintln!("{} is already installed at {}", entry.id, target.display());
        return Ok(());
    }
    std::fs::create_dir_all(target.parent().expect("the path has a parent"))?;

    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        entry.repo, entry.archivo
    );
    eprintln!("downloading {} ({:.1} GB)", entry.id, entry.gb);
    eprintln!("  from {url}");

    // Download beside the target and rename once complete, so an interrupted
    // transfer never leaves a half-written file that looks installed.
    let partial = target.with_extension("part");
    let response = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?
        .error_for_status()
        .context("the download was refused")?;
    let bytes = response.bytes().await.context("the transfer failed")?;
    std::fs::write(&partial, &bytes)
        .with_context(|| format!("failed to write {}", partial.display()))?;
    std::fs::rename(&partial, &target)?;
    eprintln!(
        "installed {} ({:.2} GB) at {}",
        entry.id,
        bytes.len() as f64 / 1_073_741_824.0,
        target.display()
    );
    Ok(())
}

fn remove_model(id: &str) -> Result<()> {
    let catalog = catalog()?;
    // A catalog entry names its file exactly; anything else is matched by
    // scanning, so models imported by hand can be removed too.
    if let Some(entry) = catalog.modelos.iter().find(|entry| entry.id == id) {
        let path = installed_path(entry)?;
        anyhow::ensure!(path.exists(), "{id} is not installed");
        let size = std::fs::metadata(&path)?.len();
        std::fs::remove_file(&path)?;
        let _ = std::fs::remove_dir(path.parent().expect("the path has a parent"));
        eprintln!(
            "removed {id} ({:.2} GB) from {}",
            size as f64 / 1_073_741_824.0,
            path.display()
        );
        return Ok(());
    }

    let root = models_root()?;
    let mut found = Vec::new();
    for publisher in std::fs::read_dir(&root)? {
        let publisher = publisher?.path();
        if !publisher.is_dir() {
            continue;
        }
        for repository in std::fs::read_dir(&publisher)? {
            let repository = repository?.path();
            if !repository.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(&repository)? {
                let file = file?.path();
                let name = file.file_name().and_then(|name| name.to_str()).unwrap_or("");
                if name.to_ascii_lowercase().contains(&id.to_ascii_lowercase()) {
                    found.push(file);
                }
            }
        }
    }
    anyhow::ensure!(!found.is_empty(), "no installed model matches `{id}`");
    anyhow::ensure!(
        found.len() == 1,
        "`{id}` matches {} files; use a more specific name",
        found.len()
    );
    let path = &found[0];
    let size = std::fs::metadata(path)?.len();
    std::fs::remove_file(path)?;
    let _ = std::fs::remove_dir(path.parent().expect("the path has a parent"));
    eprintln!(
        "removed {:.2} GB from {}",
        size as f64 / 1_073_741_824.0,
        path.display()
    );
    Ok(())
}

fn import_model(file: &PathBuf, publisher: &str, name: Option<String>) -> Result<()> {
    anyhow::ensure!(file.exists(), "{} does not exist", file.display());
    anyhow::ensure!(
        file.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("gguf")),
        "only .gguf weights can be imported"
    );
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("the file has no usable name")?;
    let repository = name.unwrap_or_else(|| stem.to_owned());
    let directory = models_root()?.join(publisher).join(&repository);
    std::fs::create_dir_all(&directory)?;
    let target = directory.join(file.file_name().expect("the file has a name"));
    anyhow::ensure!(!target.exists(), "{} already exists", target.display());

    // Try a hard link first: the weights are several gigabytes and a link keeps
    // one copy on disk. It only works within a volume, so a copy is the
    // fallback.
    match std::fs::hard_link(file, &target) {
        Ok(()) => eprintln!("linked {} (no extra disk use)", target.display()),
        Err(_) => {
            std::fs::copy(file, &target)
                .with_context(|| format!("failed to copy into {}", target.display()))?;
            eprintln!("copied to {}", target.display());
        }
    }
    eprintln!("restart LM Studio if it does not list it yet");
    Ok(())
}

/// Test cases, shipped with the binary so a run needs no external files and the
/// cases stay versioned alongside the checks that read them.
const BENCH_CASES: &str = include_str!("../assets/bench-cases.json");

#[derive(serde::Deserialize)]
struct BenchCase {
    es: String,
    #[serde(rename = "ref")]
    reference: String,
    check: String,
    #[serde(default)]
    value: Vec<String>,
    trampa: String,
}

fn bench_cases(languages: Option<&str>) -> Result<BTreeMap<String, Vec<BenchCase>>> {
    let raw: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(BENCH_CASES).context("the bundled test cases are malformed")?;
    let wanted: Option<Vec<&str>> =
        languages.map(|value| value.split(',').map(str::trim).collect());

    let mut cases = BTreeMap::new();
    for (language, value) in raw {
        // Keys beginning with an underscore carry documentation, not cases.
        if language.starts_with('_') {
            continue;
        }
        if let Some(wanted) = &wanted
            && !wanted.contains(&language.as_str())
        {
            continue;
        }
        cases.insert(
            language,
            serde_json::from_value(value).context("a language holds malformed cases")?,
        );
    }
    anyhow::ensure!(!cases.is_empty(), "no cases matched the requested languages");
    Ok(cases)
}

/// Decides whether a reply satisfies a case.
///
/// The checks are deliberately loose about wording: a correction may legitimately
/// phrase things differently, so a case asserts the presence or absence of a
/// distinguishing fragment rather than an exact sentence. `igual` is the
/// exception, and ignores only trailing punctuation.
fn case_passes(case: &BenchCase, reply: &str) -> bool {
    let reply = reply.trim();
    let lowered = reply.to_lowercase();
    match case.check.as_str() {
        "contiene" => case
            .value
            .iter()
            .any(|needle| lowered.contains(&needle.to_lowercase())),
        "no_contiene" => !case
            .value
            .iter()
            .any(|needle| lowered.contains(&needle.to_lowercase())),
        "igual" => {
            let normalize = |text: &str| {
                text.trim()
                    .trim_end_matches(['.', '!', '?', '\u{a1}', '\u{bf}'])
                    .to_lowercase()
            };
            normalize(reply) == normalize(&case.es)
        }
        _ => false,
    }
}

async fn bench(
    base_url: &str,
    model: &str,
    instructions: PathBuf,
    languages: Option<String>,
    runs: usize,
) -> Result<()> {
    anyhow::ensure!(runs > 0, "at least one run is required");
    let cases = bench_cases(languages.as_deref())?;
    let instructions = std::fs::read_to_string(&instructions)
        .with_context(|| format!("failed to read {}", instructions.display()))?;
    let system = format!("{}{FORMAT_APPENDIX}", instructions.trim());

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut totals: BTreeMap<String, Vec<usize>> = BTreeMap::new();

    for run in 1..=runs {
        eprintln!("--- run {run} of {runs} ---");
        for (language, items) in &cases {
            let payload = serde_json::json!({
                "bloques": items
                    .iter()
                    .enumerate()
                    .map(|(id, case)| serde_json::json!({
                        "id": id,
                        "es": case.es,
                        "ref": case.reference,
                    }))
                    .collect::<Vec<_>>(),
            });
            let user = serde_json::to_string(&payload)?;
            let request = ChatRequest {
                model,
                messages: vec![
                    ChatMessage {
                        role: "system",
                        content: &system,
                    },
                    ChatMessage {
                        role: "user",
                        content: &user,
                    },
                ],
                temperature: 0.2,
                stream: false,
                response_format: correction_schema(),
            };
            let response: ChatResponse = client
                .post(&endpoint)
                .json(&request)
                .send()
                .await
                .with_context(|| format!("request to {endpoint} failed"))?
                .error_for_status()
                .context("the model returned an error")?
                .json()
                .await
                .context("malformed response")?;
            let reply = response
                .choices
                .first()
                .map(|choice| choice.message.content.as_str())
                .unwrap_or_default();

            // An unparsable reply scores zero rather than aborting: failing to
            // hold the format is itself a result worth recording, and it is how
            // a model gets ruled out as a corrector.
            let parsed = extract_json(reply)
                .and_then(|json| serde_json::from_str::<CorrectionBatch>(json).ok());
            let Some(parsed) = parsed else {
                eprintln!("  {language}: unparsable reply, scored 0/{}", items.len());
                totals.entry(language.clone()).or_default().push(0);
                continue;
            };
            let by_id: BTreeMap<usize, String> = parsed
                .bloques
                .into_iter()
                .map(|item| (item.id, item.es_corregido))
                .collect();

            let mut passed = 0;
            for (id, case) in items.iter().enumerate() {
                let reply = by_id.get(&id).map(String::as_str).unwrap_or_default();
                if case_passes(case, reply) {
                    passed += 1;
                } else if run == 1 {
                    eprintln!("  {language} FALLA: {}", case.trampa);
                    eprintln!("      entrada: {:?}", case.es);
                    eprintln!("      salida : {reply:?}");
                }
            }
            eprintln!("  {language}: {passed}/{}", items.len());
            totals.entry(language.clone()).or_default().push(passed);
        }
    }

    println!();
    println!("=== {model} ===");
    let mut total = 0;
    let mut possible = 0;
    for (language, scores) in &totals {
        let items = cases[language].len();
        let best = scores.iter().max().copied().unwrap_or(0);
        let worst = scores.iter().min().copied().unwrap_or(0);
        let sum: usize = scores.iter().sum();
        total += sum;
        possible += items * scores.len();
        let estable = if best == worst { "estable" } else { "variable" };
        println!(
            "{language}: {:.1}/{items} de media  (peor {worst}, mejor {best}, {estable})",
            sum as f64 / scores.len() as f64
        );
    }
    println!(
        "global: {:.0}% ({total}/{possible})",
        total as f64 / possible as f64 * 100.0
    );
    Ok(())
}
