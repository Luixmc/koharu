//! What khr's reviewer (`khr revisar`) left for the open project, so the
//! editor can point at the balloons worth a second look.
//!
//! khr keeps each work's files in `WORKS_DIR\<slug>`, beside the project and
//! never inside it; the folder name is derived the same way khr and its panel
//! derive it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use specta::Type;
use tauri::State;

use super::Error;
use super::project::CurrentProject;

const WORKS_DIR: &str = r"I:\Koharu\obras";

/// One balloon the reviewer has something to say about, keyed by the id of
/// its text content.
#[derive(Debug, Clone, Default, Serialize, Type)]
pub(crate) struct ReviewNote {
    pub(crate) content: String,
    /// Who speaks and to whom, as the reviewer read it.
    pub(crate) speaker: Option<String>,
    /// Why the reviewer is unsure of that reading.
    pub(crate) doubt: Option<String>,
    /// A correction awaiting approval in the panel.
    pub(crate) proposal: Option<String>,
    pub(crate) reason: Option<String>,
}

fn slug(project: &Path) -> String {
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

/// Rows of a khr TSV file, comments and blank lines left out. A missing file
/// means the step has not run yet.
fn rows(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            line.split('\t')
                .map(|field| field.trim().to_owned())
                .collect()
        })
        .collect()
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value.filter(|value| !value.is_empty()).cloned()
}

/// Splits khr's "speaker → listener (duda: why)" into the reading and the doubt.
fn split_doubt(speaker: &str) -> (Option<String>, Option<String>) {
    match speaker.find(" (duda: ") {
        Some(start) => {
            let doubt = speaker[start + " (duda: ".len()..].trim_end_matches(')');
            (
                Some(speaker[..start].to_owned()).filter(|s| !s.is_empty()),
                Some(doubt.to_owned()).filter(|s| !s.is_empty()),
            )
        }
        None => (Some(speaker.to_owned()).filter(|s| !s.is_empty()), None),
    }
}

pub(crate) fn notes_for(project: &Path) -> Vec<ReviewNote> {
    let dir = Path::new(WORKS_DIR).join(slug(project));
    let mut notes = BTreeMap::<String, ReviewNote>::new();
    // revision-paginas.tsv: page, id, original, translation, speaker
    for row in rows(&dir.join("revision-paginas.tsv")) {
        let (Some(id), Some(speaker)) = (row.get(1), row.get(4)) else {
            continue;
        };
        let (speaker, doubt) = split_doubt(speaker);
        if speaker.is_none() && doubt.is_none() {
            continue;
        }
        let note = notes.entry(id.clone()).or_default();
        note.speaker = speaker;
        note.doubt = doubt;
    }
    // correcciones.tsv: id, page, original, current, proposal, reason, speaker
    for row in rows(&dir.join("correcciones.tsv")) {
        let Some(id) = row.first() else { continue };
        let note = notes.entry(id.clone()).or_default();
        note.proposal = non_empty(row.get(4));
        note.reason = non_empty(row.get(5));
    }
    notes
        .into_iter()
        .map(|(content, note)| ReviewNote { content, ..note })
        .collect()
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_review_notes(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Vec<ReviewNote>, Error> {
    let path = project
        .project
        .lock()
        .await
        .as_ref()
        .map(|project| project.path.clone());
    Ok(path.map(|path| notes_for(&path)).unwrap_or_default())
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
}
