//! What khr knows about one work, kept beside the project rather than in it.
//!
//! The project format belongs to Koharu and is rewritten whenever the editor
//! saves, so everything learned about a work lives in its own folder:
//!
//! - `ficha.md`: characters, relationships and tone, written by `khr estudiar`
//!   and meant to be edited by hand.
//! - `glosario.tsv`: renderings the user approved for this work.
//! - `propuestas.tsv`: renderings awaiting approval.
//! - `rechazados.tsv`: renderings the user turned down, never proposed again.
//! - `maquina.json`: what the machine last wrote in each balloon, so that a
//!   later hand edit can be told apart from the machine's own output.
//! - `paginas.json`: what happens on each page and who says each balloon,
//!   written by `khr estudiar --paginas` and editable by hand.
//! - `usuario.json`: tags and a short description the user writes before
//!   studying; `khr estudiar` starts from them and compares them with the text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use koharu_translator::Glossary;

pub const WORKS_DIR: &str = r"I:\Koharu\obras";
pub const GLOBAL_GLOSSARY: &str = r"I:\Koharu\glosario.tsv";
const SOURCE_SLANG_DIR: &str = r"I:\Koharu\jerga-origen";

/// Folder name for a project; Koharu derives the same name.
pub fn slug(project: &Path) -> String {
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

pub struct Work {
    dir: PathBuf,
}

impl Work {
    pub fn of(project: &Path) -> Self {
        Self {
            dir: Path::new(WORKS_DIR).join(slug(project)),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn notes_path(&self) -> PathBuf {
        self.dir.join("ficha.md")
    }

    pub fn glossary_path(&self) -> PathBuf {
        self.dir.join("glosario.tsv")
    }

    pub fn proposals_path(&self) -> PathBuf {
        self.dir.join("propuestas.tsv")
    }

    fn rejected_path(&self) -> PathBuf {
        self.dir.join("rechazados.tsv")
    }

    pub fn user_notes_path(&self) -> PathBuf {
        self.dir.join("usuario.json")
    }

    pub fn page_notes_path(&self) -> PathBuf {
        self.dir.join("paginas.json")
    }

    fn baseline_path(&self) -> PathBuf {
        self.dir.join("maquina.json")
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create {}", self.dir.display()))
    }

    pub fn notes(&self) -> Option<String> {
        std::fs::read_to_string(self.notes_path())
            .ok()
            .filter(|text| !text.trim().is_empty())
    }

    pub fn write_notes(&self, text: &str) -> Result<()> {
        self.ensure_dir()?;
        std::fs::write(self.notes_path(), text)
            .with_context(|| format!("failed to write {}", self.notes_path().display()))
    }

    /// The global glossary overlaid with this work's own.
    /// Meanings of sexual and vulgar words of every source language, from
    /// `I:\Koharu\jerga-origen\*.tsv`, without the terms the glossary
    /// already fixes. Each file is `word<TAB>meaning[<TAB>marks]`.
    pub fn source_slang(&self) -> Result<Glossary> {
        let glossary = self.glossary()?;
        let mut slang = Glossary::default();
        let mut files: Vec<PathBuf> = std::fs::read_dir(SOURCE_SLANG_DIR)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "tsv"))
            .collect();
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            slang.entries.extend(
                Glossary::parse(&text).entries.into_iter().filter(|entry| {
                    !glossary.contains(&entry.source) && !short_kana(&entry.source)
                }),
            );
        }
        Ok(slang)
    }

    pub fn glossary(&self) -> Result<Glossary> {
        let mut glossary = Glossary::load(Path::new(GLOBAL_GLOSSARY))
            .with_context(|| format!("failed to read {GLOBAL_GLOSSARY}"))?;
        glossary.merge(
            Glossary::load(&self.glossary_path())
                .with_context(|| format!("failed to read {}", self.glossary_path().display()))?,
        );
        Ok(glossary)
    }

    pub fn proposals(&self) -> Result<Glossary> {
        Glossary::load(&self.proposals_path())
            .with_context(|| format!("failed to read {}", self.proposals_path().display()))
    }

    /// Adds proposals not already approved or pending; returns how many were new.
    pub fn propose(&self, candidates: Glossary) -> Result<usize> {
        let approved = self.glossary()?;
        let rejected = Glossary::load(&self.rejected_path())?;
        let mut pending = self.proposals()?;
        let mut added = 0;
        for entry in candidates.entries {
            if approved.contains(&entry.source)
                || pending.contains(&entry.source)
                || rejected.contains(&entry.source)
            {
                continue;
            }
            pending.entries.push(entry);
            added += 1;
        }
        if added > 0 {
            self.ensure_dir()?;
            let header = "# Propuestas pendientes: original<TAB>traducción<TAB>nota\n\
                          # Apruébalas o recházalas desde Koharu.\n";
            std::fs::write(
                self.proposals_path(),
                format!("{header}{}", pending.to_tsv()),
            )?;
        }
        Ok(added)
    }

    /// What the user said about the work, if anything.
    pub fn user_notes(&self) -> Option<UserNotes> {
        std::fs::read_to_string(self.user_notes_path())
            .ok()
            .and_then(|text| serde_json::from_str::<UserNotes>(&text).ok())
            .filter(|notes| !notes.is_empty())
    }

    pub fn write_user_notes(&self, notes: &UserNotes) -> Result<()> {
        self.ensure_dir()?;
        std::fs::write(self.user_notes_path(), serde_json::to_string_pretty(notes)?)
            .with_context(|| format!("failed to write {}", self.user_notes_path().display()))
    }

    pub fn page_notes(&self) -> Vec<PageNote> {
        std::fs::read_to_string(self.page_notes_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn write_page_notes(&self, notes: &[PageNote]) -> Result<()> {
        self.ensure_dir()?;
        std::fs::write(self.page_notes_path(), serde_json::to_string_pretty(notes)?)
            .with_context(|| format!("failed to write {}", self.page_notes_path().display()))
    }

    /// Each studied page's context as the translator reads it, by page id.
    pub fn page_context(&self) -> BTreeMap<String, String> {
        self.page_notes()
            .into_iter()
            .map(|note| (note.id.clone(), note.render()))
            .filter(|(_, text)| !text.is_empty())
            .collect()
    }

    pub fn baseline(&self) -> BTreeMap<String, Machine> {
        std::fs::read_to_string(self.baseline_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn write_baseline(&self, baseline: &BTreeMap<String, Machine>) -> Result<()> {
        self.ensure_dir()?;
        std::fs::write(
            self.baseline_path(),
            serde_json::to_string_pretty(baseline)?,
        )
        .with_context(|| format!("failed to write {}", self.baseline_path().display()))
    }
}

/// Tags and a short description the user gives before the work is studied.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct UserNotes {
    #[serde(default)]
    pub etiquetas: Vec<String>,
    #[serde(default)]
    pub descripcion: String,
}

impl UserNotes {
    pub fn is_empty(&self) -> bool {
        self.descripcion.trim().is_empty() && self.etiquetas.iter().all(|tag| tag.trim().is_empty())
    }
}

/// A balloon as the machine left it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Machine {
    pub original: String,
    pub maquina: String,
}

/// What one page shows, as studied by a model that saw it.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PageNote {
    pub pagina: usize,
    pub id: String,
    pub escena: String,
    pub globos: Vec<BalloonNote>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BalloonNote {
    pub original: String,
    pub habla: String,
    pub a_quien: String,
    pub nota: String,
}

impl PageNote {
    /// The note as plain lines. Each balloon is quoted in its original
    /// language, because the translator may receive them in another order.
    pub fn render(&self) -> String {
        let known = |value: &str| {
            let value = value.trim();
            (!value.is_empty() && value != "?").then(|| value.to_owned())
        };
        let mut text = String::new();
        if let Some(scene) = known(&self.escena) {
            text.push_str(&format!("Qué pasa: {scene}\n"));
        }
        let mut balloons = String::new();
        for balloon in &self.globos {
            let (speaker, listener, note) = (
                known(&balloon.habla),
                known(&balloon.a_quien),
                known(&balloon.nota),
            );
            if speaker.is_none() && note.is_none() {
                continue;
            }
            let mut line = format!("- «{}»", balloon.original.replace('\n', " ").trim());
            if let Some(speaker) = speaker {
                line.push_str(&format!(" — dice {speaker}"));
                if let Some(listener) = listener {
                    line.push_str(&format!(" a {listener}"));
                }
            }
            if let Some(note) = note {
                line.push_str(&format!(". {note}"));
            }
            balloons.push_str(&line);
            balloons.push('\n');
        }
        if !balloons.is_empty() {
            text.push_str("Globos:\n");
            text.push_str(&balloons);
        }
        text.trim_end().to_owned()
    }
}

/// One or two kana ("いい", "する", "いる") occur on nearly every Japanese
/// page as ordinary words, and Japanese has no spaces to tell them apart, so
/// their slang readings would mislead more than help.
fn short_kana(term: &str) -> bool {
    term.chars().count() <= 2 && term.chars().all(|c| matches!(c, '\u{3040}'..='\u{30ff}'))
}
