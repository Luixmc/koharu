//! Headless tooling for Koharu projects.
//!
//! Reads and writes the same `.khrproj` format as the desktop application, so a
//! project can move between this tool and the editor without conversion.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use koharu_scene::{Session, SourceText, Translation};

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
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Dump {
            project,
            pages,
            json,
        } => dump(&project, pages, json).await,
    }
}

#[derive(serde::Serialize)]
struct DumpedText {
    entity: String,
    source: String,
    translation: Option<String>,
}

#[derive(serde::Serialize)]
struct DumpReport {
    project: String,
    pages: usize,
    texts: Vec<DumpedText>,
}

async fn dump(project: &PathBuf, limit: Option<usize>, json: bool) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();

    let page_count = snapshot.pages().len();
    let _ = limit;

    let mut texts = Vec::new();
    for entity in snapshot.entities_with::<SourceText>()? {
        let id = entity.id();
        let content = snapshot.text_content(id)?;
        let Some(source) = content.source()? else {
            continue;
        };
        let translation = content.translation()?.map(|value: Translation| value.text.value);
        texts.push(DumpedText {
            entity: format!("{id:?}"),
            source: source.text.value,
            translation,
        });
    }

    if json {
        let report = DumpReport {
            project: project.display().to_string(),
            pages: page_count,
            texts,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let translated = texts.iter().filter(|t| t.translation.is_some()).count();
    let empty = texts.iter().filter(|t| t.source.trim().is_empty()).count();
    println!("project:     {}", project.display());
    println!("pages:       {page_count}");
    println!("text blocks: {}", texts.len());
    println!("translated:  {translated}");
    println!("empty OCR:   {empty}");
    println!();
    for (index, text) in texts.iter().enumerate() {
        println!("[{index}] {}", text.source.replace('\n', " / "));
        if let Some(translation) = &text.translation {
            println!("     -> {}", translation.replace('\n', " / "));
        }
    }
    Ok(())
}
