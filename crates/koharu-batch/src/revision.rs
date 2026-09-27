//! Proposing corrections instead of applying them.
//!
//! `revisar` shows a local model each page's balloons, original and
//! translation side by side with the work's notes and glossary, and asks only
//! for real errors: grammar, the wrong person or subject, a meaning the
//! original does not carry, an ignored glossary term, words left untranslated.
//! Nothing is written to the project. The proposals land in `correcciones.tsv`
//! for the user to approve or reject in the panel, because the rewriting
//! corrector this replaces cost more good lines than it fixed.
//!
//! `aplicar` then writes the approved ones, as the user's own text.
//!
//! Next to the proposals goes `revision-paginas.tsv`, every balloon of every
//! page in reading order, so the panel can show a proposal with the rest of
//! its page around it: a line is hard to judge alone.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use koharu_scene::{Authored, EntityId, Session, Translation};

use crate::obra::Work;
use crate::{ChatMessage, ChatRequest, ChatResponse, extract_json};

const PROMPT: &str = r#"Eres revisor de una traducción de manga al español latino neutro.
Recibes la ficha de la obra, su glosario y los globos de UNA página, cada uno con el
original y la traducción actual, en orden de lectura.

Propón un cambio SOLO si el globo tiene alguno de estos errores:
- gramática: ortografía, concordancia de género o número, conjugación, signos mal puestos.
- persona: el sujeto, el objeto o el trato no coinciden con el original o con quién habla
  según la ficha (quién hace qué a quién; tú, usted o ustedes).
- sentido: la traducción dice algo que el original no dice, o se come una parte importante.
- glosario: un término del glosario aparece traducido de otra forma.
- sin traducir: quedaron palabras en japonés, coreano, chino o inglés.

Para revisar la persona, en cada globo pregúntate quién hace cada acción en el original
(yo, tú, él, ellos) y si la traducción conjuga el verbo con esa misma persona. En japonés
el sujeto suele omitirse: dedúcelo por el hablante, la ficha y los globos vecinos.

NO cambies el estilo, los sinónimos, las groserías ni el registro si ya son correctos.
NO cambies palabras correctas por regionalismos (dormitorio, recámara, etc. valen igual).
NO toques la puntuación si ya es correcta en español (¡¿...?! es correcto).
NO corrijas onomatopeyas, gemidos ni risas.
El cambio debe ser mínimo: conserva la frase y corrige solo lo necesario, que suene natural.
Si un globo está bien, no lo incluyas. Es normal que una página no tenga ningún cambio.

Responde solo el JSON pedido. "motivo" es una de: gramática, persona, sentido, glosario,
sin traducir; seguida de una explicación de pocas palabras."#;

/// One proposal awaiting the user's decision.
struct Proposal {
    id: String,
    page: usize,
    original: String,
    current: String,
    proposal: String,
    reason: String,
}

fn proposals_path(work: &Work) -> PathBuf {
    work.dir().join("correcciones.tsv")
}

fn approved_path(work: &Work) -> PathBuf {
    work.dir().join("correcciones-aprobadas.tsv")
}

fn rejected_path(work: &Work) -> PathBuf {
    work.dir().join("correcciones-rechazadas.tsv")
}

fn context_path(work: &Work) -> PathBuf {
    work.dir().join("revision-paginas.tsv")
}

const CONTEXT_HEADER: &str =
    "# Globos de cada página en orden de lectura: página\tid\toriginal\ttraducción\n";

/// Writes every balloon of `pages`, translated or not, in reading order.
fn write_context(work: &Work, pages: &[Vec<crate::estudio::Block>]) -> Result<()> {
    let mut text = String::from(CONTEXT_HEADER);
    for (index, blocks) in pages.iter().enumerate() {
        for block in blocks {
            let translation = block
                .translation
                .as_ref()
                .map(|translation| clean(translation.text.value.trim()))
                .unwrap_or_default();
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\n",
                index + 1,
                block.id,
                clean(&block.source),
                translation
            ));
        }
    }
    std::fs::create_dir_all(work.dir())?;
    std::fs::write(context_path(work), text)?;
    Ok(())
}

/// Only the page context, for proposals made before it was written.
pub async fn context(project: &Path, right_to_left: bool) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let pages = crate::estudio::pages_in_order(&session.snapshot(), right_to_left)?;
    let work = Work::of(project);
    write_context(&work, &pages)?;
    println!("contexto de {} página(s) en {}", pages.len(), context_path(&work).display());
    Ok(())
}

fn clean(text: &str) -> String {
    text.replace(['\t', '\r'], " ").replace('\n', " / ")
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "cambios": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "n": {"type": "integer"},
                        "propuesta": {"type": "string"},
                        "motivo": {"type": "string"}
                    },
                    "required": ["n", "propuesta", "motivo"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["cambios"],
        "additionalProperties": false
    })
}

/// A reason that concludes the line was fine after all ("no hay un error
/// grave", "se puede dejar como está"): the model argued itself out of the
/// change but still sent it.
fn talks_itself_out(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    ["no hay un error", "no hay error", "se puede dejar", "puede quedar", "está bien así"]
        .iter()
        .any(|phrase| reason.contains(phrase))
}

/// Pairs (balloon id, current translation) the user already turned down, so
/// the same proposal on the same text is not offered again.
fn rejected(work: &Work) -> BTreeSet<(String, String)> {
    std::fs::read_to_string(rejected_path(work))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some((fields.next()?.to_owned(), fields.next()?.to_owned()))
        })
        .collect()
}

pub async fn review(
    project: &Path,
    base_url: &str,
    model: &str,
    right_to_left: bool,
    limit: Option<usize>,
) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();
    let pages = crate::estudio::pages_in_order(&snapshot, right_to_left)?;
    let work = Work::of(project);
    let notes = work.notes().unwrap_or_default();
    let glossary: Vec<_> = work
        .glossary()?
        .entries
        .iter()
        .map(|entry| serde_json::json!({"original": entry.source, "traduccion": entry.target}))
        .collect();
    let skip = rejected(&work);
    let count = limit.unwrap_or(pages.len()).min(pages.len());
    eprintln!("reviewing {count} page(s)");

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut found = Vec::new();
    for (index, blocks) in pages.iter().take(count).enumerate() {
        crate::progress(index, count, "revisión");
        let balloons: Vec<_> = blocks
            .iter()
            .filter_map(|block| Some((block, block.translation.as_ref()?)))
            .filter(|(_, translation)| !translation.text.value.trim().is_empty())
            .collect();
        if balloons.is_empty() {
            continue;
        }
        let listed: Vec<_> = balloons
            .iter()
            .enumerate()
            .map(|(n, (block, translation))| {
                serde_json::json!({
                    "n": n + 1,
                    "original": block.source,
                    "traduccion": translation.text.value,
                })
            })
            .collect();
        let user = serde_json::to_string(&serde_json::json!({
            "ficha": notes,
            "glosario": glossary,
            "pagina": index + 1,
            "globos": listed,
        }))?;
        let request = ChatRequest {
            model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: PROMPT,
                },
                ChatMessage {
                    role: "user",
                    content: &user,
                },
            ],
            temperature: 0.2,
            stream: false,
            reasoning_effort: "none",
            response_format: serde_json::json!({
                "type": "json_schema",
                "json_schema": {"name": "revision", "strict": true, "schema": schema()},
            }),
        };
        let response: ChatResponse = client
            .post(&endpoint)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("request to {endpoint} failed"))?
            .error_for_status()
            .with_context(|| format!("page {}: the model returned an error", index + 1))?
            .json()
            .await
            .context("malformed response")?;
        let reply = response
            .choices
            .first()
            .map(|choice| choice.message.content.as_str())
            .unwrap_or_default();
        let Some(value) = extract_json(reply)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        else {
            eprintln!("  página {}: respuesta ilegible, se omite", index + 1);
            continue;
        };
        let mut on_page = 0;
        for change in value["cambios"].as_array().into_iter().flatten() {
            let Some(n) = change["n"].as_u64().map(|n| n as usize) else {
                continue;
            };
            let Some((block, translation)) = n.checked_sub(1).and_then(|i| balloons.get(i)) else {
                continue;
            };
            let proposal = change["propuesta"].as_str().unwrap_or_default().trim();
            let current = translation.text.value.trim();
            let reason = change["motivo"].as_str().unwrap_or_default().trim();
            if proposal.is_empty() || proposal == current || talks_itself_out(reason) {
                continue;
            }
            let id = block.id.to_string();
            if skip.contains(&(id.clone(), clean(current))) {
                continue;
            }
            found.push(Proposal {
                id,
                page: index + 1,
                original: clean(&block.source),
                current: clean(current),
                proposal: clean(proposal),
                reason: clean(reason),
            });
            on_page += 1;
        }
        eprintln!("  página {}: {} propuesta(s)", index + 1, on_page);
    }

    let mut text = String::from(
        "# Correcciones propuestas: id\tpágina\toriginal\tactual\tpropuesta\tmotivo\n\
         # Apruébalas o recházalas desde el panel.\n",
    );
    for p in &found {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\n",
            p.id, p.page, p.original, p.current, p.proposal, p.reason
        ));
    }
    std::fs::create_dir_all(work.dir())?;
    std::fs::write(proposals_path(&work), text)?;
    write_context(&work, &pages)?;
    println!("{} propuesta(s) en {}", found.len(), proposals_path(&work).display());
    Ok(())
}

/// Writes the corrections the user approved, as the user's own text, and
/// empties the approved list.
pub async fn apply(project: &Path) -> Result<()> {
    let work = Work::of(project);
    let path = approved_path(&work);
    let approved: BTreeMap<String, String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let (id, text) = line.split_once('\t')?;
            Some((id.to_owned(), text.replace(" / ", "\n")))
        })
        .collect();
    if approved.is_empty() {
        eprintln!("no approved corrections in {}", path.display());
        return Ok(());
    }
    let mut session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();
    let mut writes = Vec::new();
    for (id, text) in &approved {
        let Ok(entity) = serde_json::from_value::<EntityId>(serde_json::json!(id)) else {
            eprintln!("  id inválido: {id}");
            continue;
        };
        let Ok(content) = snapshot.text_content(entity) else {
            eprintln!("  el globo {id} ya no existe");
            continue;
        };
        let Some(previous) = content.translation()? else {
            continue;
        };
        writes.push((entity, previous, text.clone()));
    }
    let patch = snapshot.patch(|edit| {
        for (entity, previous, text) in &writes {
            edit.set(
                *entity,
                &Translation {
                    text: Authored::user(text.clone()),
                    language: previous.language.clone(),
                },
            )?;
        }
        Ok(())
    })?;
    session.commit(patch).await?;
    refresh_context(&work, &approved)?;
    std::fs::write(&path, "# Correcciones aprobadas pendientes de aplicar: id\tpropuesta\n")?;
    println!("{} corrección(es) aplicada(s)", writes.len());
    Ok(())
}

/// Puts the applied text into the page context, so the panel does not show
/// the old line around the next proposals.
fn refresh_context(work: &Work, applied: &BTreeMap<String, String>) -> Result<()> {
    let Ok(text) = std::fs::read_to_string(context_path(work)) else {
        return Ok(());
    };
    let mut out = String::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match (line.starts_with('#'), fields.as_slice()) {
            (false, [page, id, original, _]) if applied.contains_key(*id) => {
                out.push_str(&format!("{page}\t{id}\t{original}\t{}\n", clean(&applied[*id])));
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    std::fs::write(context_path(work), out)?;
    Ok(())
}
