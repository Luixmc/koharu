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
use koharu_translator::{
    GenerationConfig, ModelSelection, ProvidersConfig, TranslationRequest, Translator,
};

#[derive(Debug, Parser)]
#[command(name = "khr", version, about = "Headless tooling for Koharu projects")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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

        /// Translation provider hosting the corrector.
        #[arg(long, default_value = "lm-studio")]
        provider: String,

        /// Corrector model identifier.
        #[arg(long)]
        model: Option<String>,

        /// File holding the correction instructions.
        #[arg(long, value_name = "FILE")]
        instructions: Option<PathBuf>,

        /// Blocks sent per request.
        #[arg(long, default_value_t = 8)]
        batch: usize,

        /// Report changes without writing them.
        #[arg(long)]
        dry_run: bool,

        /// Force CPU execution.
        #[arg(long)]
        cpu: bool,
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
            provider,
            model,
            instructions,
            batch,
            dry_run,
            cpu,
        } => {
            post(
                &project,
                pages,
                &provider,
                model,
                instructions,
                batch,
                dry_run,
                cpu,
            )
            .await
        }
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

/// Pairs the original with its translation for the corrector.
///
/// The corrector needs both: wording that a machine translation softened or
/// dropped cannot be recovered from the translation alone.
fn correction_segment(source: &str, translation: &str) -> String {
    format!("[ORIGINAL]\n{source}\n[TRADUCCION]\n{translation}")
}

#[allow(clippy::too_many_arguments)]
async fn post(
    project: &PathBuf,
    limit: Option<usize>,
    provider: &str,
    model: Option<String>,
    instructions: Option<PathBuf>,
    batch: usize,
    dry_run: bool,
    cpu: bool,
) -> Result<()> {
    anyhow::ensure!(batch > 0, "batch size must be at least 1");

    let instructions = match instructions {
        Some(path) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?,
        ),
        None => None,
    };

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

    let pipeline_config = koharu_config::load::<PipelineConfig>("pipeline")?;
    let target_language = pipeline_config.read()?.translation.target_language;
    let selection = ModelSelection {
        provider: provider
            .parse()
            .map_err(|_| anyhow::anyhow!("unknown provider `{provider}`"))?,
        model,
        quantization: None,
        vision: false,
        reasoning: false,
    };
    let translator = Translator::from_config(
        koharu_ml::device(cpu),
        koharu_config::load::<ProvidersConfig>("providers")?,
    )?;

    let mut corrected = Vec::new();
    for (index, chunk) in pending.chunks(batch).enumerate() {
        let segments: Vec<String> = chunk
            .iter()
            .map(|(_, source, translation)| correction_segment(source, &translation.text.value))
            .collect();
        let mut request = TranslationRequest::new(segments, target_language);
        request.instructions = instructions.clone();

        let (_, results) = translator
            .translate(&selection, GenerationConfig::default(), request)
            .await
            .with_context(|| format!("correction batch {index} failed"))?;
        eprintln!("  batch {index}: {} block(s)", results.len());
        for ((id, _, translation), result) in chunk.iter().zip(results) {
            corrected.push((*id, translation.clone(), result));
        }
    }

    let changed = corrected
        .iter()
        .filter(|(_, previous, result)| previous.text.value.trim() != result.trim())
        .count();
    eprintln!("{changed} of {} block(s) changed", corrected.len());

    if dry_run {
        eprintln!("dry run: the project was not modified");
        return Ok(());
    }

    let patch = session.snapshot().patch(|edit| {
        for (id, previous, result) in &corrected {
            if previous.text.value.trim() == result.trim() {
                continue;
            }
            edit.set(
                *id,
                &Translation {
                    text: Authored {
                        value: result.clone(),
                        origin: previous.text.origin.clone(),
                    },
                    language: previous.language.clone(),
                },
            )?;
        }
        Ok(())
    })?;
    session.commit(patch).await?;
    eprintln!("wrote {changed} correction(s) to {}", project.display());
    Ok(())
}
