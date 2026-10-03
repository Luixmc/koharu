//! What khr keeps for each work in `WORKS_DIR\<slug>`, beside the project and
//! never inside it: the notes the user gives the study, the ficha it writes,
//! the glossary and its proposals, and the reviewer's corrections. khr owns
//! the formats; this module reads them and records the user's decisions in
//! the same files khr reads back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use specta::Type;

pub(crate) const WORKS_DIR: &str = r"I:\Koharu\obras";
pub(crate) const GLOBAL_GLOSSARY: &str = r"I:\Koharu\glosario.tsv";

const WORK_GLOSSARY_HEADER: &str =
    "# Glosario de esta obra: original<TAB>traducción<TAB>nota. Manda sobre el global.\n";
const REJECTED_HEADER: &str = "# Términos rechazados: khr no volverá a proponerlos.\n";
const PROPOSALS_HEADER: &str = "# Propuestas pendientes: original<TAB>traducción<TAB>nota\n\
                                # Apruébalas o recházalas desde Koharu.\n";
const GLOBAL_HEADER: &str =
    "# Glosario global: original<TAB>traducción<TAB>nota. Vale para todas las obras.\n";
const CORRECTIONS_HEADER: &str = "# Correcciones propuestas: id\tpágina\toriginal\tactual\tpropuesta\tmotivo\thabla\n\
     # Apruébalas o recházalas desde Koharu.\n";
const APPROVED_CORRECTIONS_HEADER: &str =
    "# Correcciones aprobadas pendientes de aplicar: id\tpropuesta\n";
const REJECTED_CORRECTIONS_HEADER: &str =
    "# Correcciones rechazadas: id\ttraducción que se dejó\tpropuesta\n";

/// The work folder of a project; khr derives the name the same way.
pub(crate) fn dir(project: &Path) -> PathBuf {
    let slug: String = project
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
        .collect();
    Path::new(WORKS_DIR).join(slug)
}

/// Lines of a khr file, comments and blank lines left out. A missing file
/// means the step that writes it has not run yet.
fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn rows(path: &Path) -> Vec<Vec<String>> {
    lines(path)
        .iter()
        .map(|line| {
            line.split('\t')
                .map(|field| field.trim().to_owned())
                .collect()
        })
        .collect()
}

fn field(row: &[String], index: usize) -> String {
    row.get(index).cloned().unwrap_or_default()
}

/// One glossary line: original, rendering and an optional note.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct Term {
    pub source: String,
    pub target: String,
    pub note: String,
}

/// One correction proposed by `khr revisar`.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct Correction {
    pub id: String,
    pub page: String,
    pub original: String,
    pub current: String,
    pub proposal: String,
    pub reason: String,
    /// Who the reviewer took to be speaking and to whom, with its doubt.
    pub speaker: String,
}

/// One balloon of a reviewed page, to show proposals within their page.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Balloon {
    pub page: String,
    pub id: String,
    pub original: String,
    pub translation: String,
    pub speaker: String,
}

/// Tags and a short description the user gives `khr estudiar` to start from.
#[derive(Clone, Debug, Default, Deserialize, Serialize, Type)]
pub struct UserNotes {
    pub tags: Vec<String>,
    pub description: String,
}

/// `usuario.json`, as khr reads it.
#[derive(Default, Deserialize, Serialize)]
struct NotesFile {
    #[serde(default)]
    etiquetas: Vec<String>,
    #[serde(default)]
    descripcion: String,
}

/// What the work needs next, from what its folder holds.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NextStep {
    Process,
    Proposals {
        #[specta(type = f64)]
        count: usize,
    },
    Corrections {
        #[specta(type = f64)]
        count: usize,
    },
    Apply,
    Finish,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Work {
    pub ficha: Option<String>,
    pub notes: UserNotes,
    pub proposals: Vec<Term>,
    pub terms: Vec<Term>,
    pub corrections: Vec<Correction>,
    #[specta(type = f64)]
    pub approved_corrections: usize,
    pub balloons: Vec<Balloon>,
    pub next: NextStep,
}

pub(crate) fn read(dir: &Path) -> Work {
    let ficha = std::fs::read_to_string(dir.join("ficha.md")).ok();
    let proposals = read_terms(&dir.join("propuestas.tsv"));
    let corrections = read_corrections(&dir);
    let approved_corrections = lines(&dir.join("correcciones-aprobadas.tsv")).len();
    let next = if ficha.is_none() {
        NextStep::Process
    } else if !proposals.is_empty() {
        NextStep::Proposals {
            count: proposals.len(),
        }
    } else if !corrections.is_empty() {
        NextStep::Corrections {
            count: corrections.len(),
        }
    } else if approved_corrections > 0 {
        NextStep::Apply
    } else {
        NextStep::Finish
    };
    Work {
        ficha,
        notes: read_notes(dir),
        proposals,
        terms: read_terms(&dir.join("glosario.tsv")),
        corrections,
        approved_corrections,
        balloons: rows(&dir.join("revision-paginas.tsv"))
            .into_iter()
            .filter(|row| row.len() >= 2)
            .map(|row| Balloon {
                page: field(&row, 0),
                id: field(&row, 1),
                original: field(&row, 2),
                translation: field(&row, 3),
                speaker: field(&row, 4),
            })
            .collect(),
        next,
    }
}

pub(crate) fn read_notes(dir: &Path) -> UserNotes {
    let file: NotesFile = std::fs::read_to_string(dir.join("usuario.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    UserNotes {
        tags: file.etiquetas,
        description: file.descripcion,
    }
}

/// Writes the notes `khr estudiar` reads; empty notes remove the file so the
/// study starts from the text alone.
pub(crate) fn write_notes(dir: &Path, notes: &UserNotes) -> Result<()> {
    let path = dir.join("usuario.json");
    let notes = NotesFile {
        etiquetas: notes
            .tags
            .iter()
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect(),
        descripcion: notes.description.trim().to_owned(),
    };
    if notes.etiquetas.is_empty() && notes.descripcion.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, serde_json::to_string_pretty(&notes)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn read_terms(path: &Path) -> Vec<Term> {
    rows(path)
        .into_iter()
        .map(|row| Term {
            source: field(&row, 0),
            target: field(&row, 1),
            note: field(&row, 2),
        })
        .filter(|term| !term.source.is_empty() && !term.target.is_empty())
        .collect()
}

fn write_terms(path: &Path, header: &str, terms: &[Term]) -> Result<()> {
    let mut text = header.to_owned();
    for term in terms {
        let target = term.target.replace(['\t', '\n'], " ");
        if term.note.is_empty() {
            text.push_str(&format!("{}\t{target}\n", term.source));
        } else {
            text.push_str(&format!("{}\t{target}\t{}\n", term.source, term.note));
        }
    }
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

fn same_source(a: &Term, b: &Term) -> bool {
    a.source.to_lowercase() == b.source.to_lowercase()
}

/// Adds `terms` to `kept`, each replacing an entry with the same original.
fn merge_terms(kept: &mut Vec<Term>, terms: &[Term]) {
    for term in terms {
        kept.retain(|existing| !same_source(existing, term));
        kept.push(term.clone());
    }
}

/// Moves proposals into the work's glossary or its rejected list, as the user
/// left them (the rendering may have been edited), and drops them from the
/// pending proposals.
pub(crate) fn decide_terms(dir: &Path, terms: &[Term], approve: bool) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let (file, header) = if approve {
        ("glosario.tsv", WORK_GLOSSARY_HEADER)
    } else {
        ("rechazados.tsv", REJECTED_HEADER)
    };
    let mut kept = read_terms(&dir.join(file));
    merge_terms(&mut kept, terms);
    write_terms(&dir.join(file), header, &kept)?;
    let mut pending = read_terms(&dir.join("propuestas.tsv"));
    pending.retain(|proposal| !terms.iter().any(|term| same_source(proposal, term)));
    write_terms(&dir.join("propuestas.tsv"), PROPOSALS_HEADER, &pending)
}

/// Approves every pending proposal; used when the study is followed by a
/// translation, so the new terms are in force for it.
pub(crate) fn approve_all_terms(dir: &Path) -> Result<usize> {
    let pending = read_terms(&dir.join("propuestas.tsv"));
    if !pending.is_empty() {
        decide_terms(dir, &pending, true)?;
    }
    Ok(pending.len())
}

/// Adds one term to the work's glossary, replacing the same original.
pub(crate) fn add_term(dir: &Path, term: &Term) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut terms = read_terms(&dir.join("glosario.tsv"));
    merge_terms(&mut terms, std::slice::from_ref(term));
    write_terms(&dir.join("glosario.tsv"), WORK_GLOSSARY_HEADER, &terms)
}

/// Copies a work's term into the global glossary, replacing an entry with the
/// same original there; the work keeps its copy, which still wins inside it.
pub(crate) fn promote_term(term: &Term) -> Result<()> {
    let global = Path::new(GLOBAL_GLOSSARY);
    let mut terms = read_terms(global);
    merge_terms(&mut terms, std::slice::from_ref(term));
    // The file's own explanatory header stays.
    let header: String = std::fs::read_to_string(global)
        .unwrap_or_default()
        .lines()
        .take_while(|line| line.trim_start().starts_with('#'))
        .map(|line| format!("{line}\n"))
        .collect();
    let header = if header.is_empty() {
        GLOBAL_HEADER
    } else {
        &header
    };
    write_terms(global, header, &terms)
}

fn read_corrections(dir: &Path) -> Vec<Correction> {
    lines(&dir.join("correcciones.tsv"))
        .iter()
        .filter_map(|line| {
            let row: Vec<String> = line.split('\t').map(str::to_owned).collect();
            (row.len() >= 5).then(|| Correction {
                id: field(&row, 0),
                page: field(&row, 1),
                original: field(&row, 2),
                current: field(&row, 3),
                proposal: field(&row, 4),
                reason: field(&row, 5),
                speaker: field(&row, 6),
            })
        })
        .collect()
}

/// Moves corrections, as the user left them, to the approved or the rejected
/// list. Approved text reaches the project only when `khr aplicar` runs.
pub(crate) fn decide_corrections(
    dir: &Path,
    corrections: &[Correction],
    approve: bool,
) -> Result<()> {
    let (file, header) = if approve {
        ("correcciones-aprobadas.tsv", APPROVED_CORRECTIONS_HEADER)
    } else {
        ("correcciones-rechazadas.tsv", REJECTED_CORRECTIONS_HEADER)
    };
    let mut kept = lines(&dir.join(file));
    for correction in corrections {
        let proposal = correction.proposal.replace(['\t', '\n'], " ");
        kept.retain(|line| !line.starts_with(&format!("{}\t", correction.id)));
        kept.push(if approve {
            format!("{}\t{proposal}", correction.id)
        } else {
            format!("{}\t{}\t{proposal}", correction.id, correction.current)
        });
    }
    let mut text = header.to_owned();
    for line in &kept {
        text.push_str(line);
        text.push('\n');
    }
    std::fs::write(dir.join(file), text)?;

    let mut pending = read_corrections(dir);
    pending.retain(|pending| !corrections.iter().any(|done| done.id == pending.id));
    let mut text = CORRECTIONS_HEADER.to_owned();
    for c in &pending {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.id, c.page, c.original, c.current, c.proposal, c.reason, c.speaker
        ));
    }
    std::fs::write(dir.join("correcciones.tsv"), text)
        .with_context(|| format!("failed to write the corrections of {}", dir.display()))
}

pub(crate) fn pending_terms(dir: &Path) -> usize {
    read_terms(&dir.join("propuestas.tsv")).len()
}

pub(crate) fn pending_corrections(dir: &Path) -> usize {
    read_corrections(dir).len()
}

/// One balloon the reviewer has something to say about, keyed by the id of
/// its text content.
#[derive(Debug, Clone, Default, Serialize, Type)]
pub struct ReviewNote {
    pub content: String,
    /// Who speaks and to whom, as the reviewer read it.
    pub speaker: Option<String>,
    /// Why the reviewer is unsure of that reading.
    pub doubt: Option<String>,
    /// A correction awaiting approval.
    pub proposal: Option<String>,
    pub reason: Option<String>,
}

fn non_empty(value: String) -> Option<String> {
    Some(value).filter(|value| !value.is_empty())
}

/// Splits khr's "speaker → listener (duda: why)" into the reading and the doubt.
fn split_doubt(speaker: &str) -> (Option<String>, Option<String>) {
    match speaker.find(" (duda: ") {
        Some(start) => {
            let doubt = speaker[start + " (duda: ".len()..].trim_end_matches(')');
            (
                non_empty(speaker[..start].to_owned()),
                non_empty(doubt.to_owned()),
            )
        }
        None => (non_empty(speaker.to_owned()), None),
    }
}

pub(crate) fn review_notes(dir: &Path) -> Vec<ReviewNote> {
    let work = read(dir);
    let mut notes = BTreeMap::<String, ReviewNote>::new();
    for balloon in work.balloons {
        let (speaker, doubt) = split_doubt(&balloon.speaker);
        if speaker.is_none() && doubt.is_none() {
            continue;
        }
        let note = notes.entry(balloon.id).or_default();
        note.speaker = speaker;
        note.doubt = doubt;
    }
    for correction in work.corrections {
        let note = notes.entry(correction.id).or_default();
        note.proposal = non_empty(correction.proposal);
        note.reason = non_empty(correction.reason);
    }
    notes
        .into_iter()
        .map(|(content, note)| ReviewNote { content, ..note })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubt_is_split_from_the_speaker() {
        assert_eq!(
            split_doubt("Aki → Yuu (duda: el original no dice quién)"),
            (
                Some("Aki → Yuu".to_owned()),
                Some("el original no dice quién".to_owned())
            )
        );
        assert_eq!(split_doubt("narrador"), (Some("narrador".to_owned()), None));
        assert_eq!(split_doubt(""), (None, None));
    }

    #[test]
    fn deciding_a_proposal_moves_it_with_its_edit() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path();
        std::fs::write(
            work.join("propuestas.tsv"),
            "# cabecera\nお兄ちゃん\thermano\tnota\n先輩\tsenpai\n",
        )
        .unwrap();
        let edited = Term {
            source: "お兄ちゃん".to_owned(),
            target: "hermanito".to_owned(),
            note: "nota".to_owned(),
        };
        decide_terms(work, &[edited], true).unwrap();
        let read = read(work);
        assert_eq!(read.terms.len(), 1);
        assert_eq!(read.terms[0].target, "hermanito");
        assert_eq!(read.proposals.len(), 1);
        assert_eq!(read.proposals[0].source, "先輩");
    }
}
