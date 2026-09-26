//! Reading a work before translating it, and learning from the user's edits.
//!
//! `estudiar` hands a local model the whole text in reading order and asks it
//! for notes on the work: who the characters are, how they relate, how each
//! one talks, and which terms recur. The notes travel with every page the
//! translator and the corrector see afterwards.
//!
//! `aprender` compares what the machine wrote with what the user left after
//! editing by hand, and asks the model which vocabulary choices those edits
//! reveal. Nothing it finds is applied: every term becomes a proposal the user
//! approves or rejects, because an edit that fitted one scene is not a rule.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use koharu_scene::{EntityId, Origin, Session, Snapshot, SourceText, Translation};
use koharu_translator::{Glossary, GlossaryEntry};

use crate::obra::{BalloonNote, Machine, PageNote, Work};
use crate::{ChatMessage, ChatRequest, ChatResponse, extract_json, placement, sort_reading_order};

/// Characters of source text sent per request while studying. The corrector
/// is loaded with an 8K context; this leaves room for the notes so far and
/// for the reply.
const STUDY_CHUNK: usize = 4000;
/// With vision, pages shown per part of the whole-work study, and their size:
/// small enough that four images and the notes still fit an 8k context.
const IMAGES_PER_PART: usize = 4;
const STUDY_IMAGE_SIDE: u32 = 768;
const LEARN_BATCH: usize = 15;
const LONGEST_TERM: usize = 40;

pub struct Block {
    pub id: EntityId,
    pub source: String,
    pub translation: Option<Translation>,
}

/// Every page's text blocks, pages in project order and blocks in reading order.
pub fn pages_in_order(snapshot: &Snapshot, right_to_left: bool) -> Result<Vec<Vec<Block>>> {
    let page_order: BTreeMap<_, _> = snapshot
        .pages()
        .enumerate()
        .map(|(index, page)| (page.id(), index))
        .collect();
    let mut pages: Vec<Vec<Block>> = (0..page_order.len()).map(|_| Vec::new()).collect();

    for entity in snapshot.entities_with::<SourceText>()? {
        let id = entity.id();
        let content = snapshot.text_content(id)?;
        let Some(source) = content.source()? else {
            continue;
        };
        if source.text.value.trim().is_empty() {
            continue;
        }
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            if let Some(&index) = page_order.get(&current) {
                pages[index].push(Block {
                    id,
                    source: source.text.value.clone(),
                    translation: content.translation()?,
                });
                break;
            }
            cursor = snapshot.parent(current)?;
        }
    }

    for blocks in &mut pages {
        let mut placed = Vec::new();
        for (position, block) in blocks.iter().enumerate() {
            let place = snapshot
                .text_content(block.id)
                .ok()
                .and_then(|content| content.source_region().ok().flatten())
                .and_then(|region| {
                    snapshot
                        .component::<koharu_scene::Geometry>(region.id())
                        .ok()
                        .flatten()
                })
                .as_ref()
                .and_then(placement);
            if let Some(place) = place {
                placed.push((position, place));
            }
        }
        if placed.len() == blocks.len() {
            sort_reading_order(&mut placed, right_to_left);
            let mut taken: Vec<Option<Block>> = blocks.drain(..).map(Some).collect();
            *blocks = placed
                .iter()
                .filter_map(|(position, _)| taken[*position].take())
                .collect();
        }
    }
    Ok(pages)
}

/// Records the machine's current output for every balloon it last wrote.
///
/// A balloon the user edited keeps its earlier record, which is what lets
/// `aprender` see the edit. `overrides` holds text the caller has just written
/// on the machine's behalf, such as a corrector's output kept under the user's
/// authorship.
pub fn record_machine(
    project: &Path,
    snapshot: &Snapshot,
    overrides: &BTreeMap<EntityId, String>,
) -> Result<()> {
    let work = Work::of(project);
    let mut baseline = work.baseline();
    for block in pages_in_order(snapshot, true)?.into_iter().flatten() {
        let Some(translation) = &block.translation else {
            continue;
        };
        let text = match overrides.get(&block.id) {
            Some(text) => text.clone(),
            None if matches!(translation.text.origin, Origin::User) => continue,
            None => translation.text.value.clone(),
        };
        baseline.insert(
            block.id.to_string(),
            Machine {
                original: block.source.clone(),
                maquina: text,
            },
        );
    }
    work.write_baseline(&baseline)
}

async fn chat_json(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    system: &str,
    user: &str,
    name: &str,
    schema: serde_json::Value,
) -> Result<Option<serde_json::Value>> {
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let request = ChatRequest {
        model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: system,
            },
            ChatMessage {
                role: "user",
                content: user,
            },
        ],
        temperature: 0.2,
        stream: false,
        reasoning_effort: "none",
        response_format: serde_json::json!({
            "type": "json_schema",
            "json_schema": { "name": name, "strict": true, "schema": schema },
        }),
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
    Ok(extract_json(reply).and_then(|json| serde_json::from_str(json).ok()))
}

fn terms_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "original": {"type": "string"},
                "traduccion": {"type": "string"},
                "nota": {"type": "string"}
            },
            "required": ["original", "traduccion", "nota"],
            "additionalProperties": false
        }
    })
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Term {
    original: String,
    traduccion: String,
    nota: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Character {
    nombre: String,
    descripcion: String,
    relaciones: String,
    habla: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Overview {
    genero: String,
    tono: String,
    resumen: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Notes {
    obra: Overview,
    personajes: Vec<Character>,
    terminos: Vec<Term>,
}

const STUDY_PROMPT: &str = "\
Eres un editor que prepara la traducción al español latino de un manga para adultos. \
No traduces: lees el texto original y escribes una FICHA que el traductor consultará \
en cada página.

Recibes la ficha que llevas hasta ahora y un tramo nuevo del texto, página por página \
y en orden de lectura. Devuelve la ficha COMPLETA actualizada: conserva lo que ya \
sabías, corrígelo si el tramo nuevo lo contradice y añade lo nuevo.

- obra: género, tono (cómico, romántico, sucio, dramático...) y un resumen breve de \
lo que pasa hasta ahora.
- personajes: nombre tal como aparece, quién es, sus relaciones con los demás (quién \
es hermano, madre, pareja, jefe de quién) y cómo habla (formal, vulgar, tímido, \
cómo llama a los demás). Esto decide el registro y el género gramatical de la \
traducción, así que sé concreto.
- terminos: palabras o expresiones que se repiten y deben traducirse siempre igual: \
nombres propios, apodos, honoríficos (onii-chan, senpai), formas de llamarse \
(\"big sis\"), jerga sexual recurrente y objetos importantes. Propón la traducción \
al español latino neutro y una nota breve. No incluyas frases enteras ni palabras \
comunes que no necesitan regla.

Escribe todo en español. Si el texto trae ruido de OCR (letras sueltas, marcas de \
agua), ignóralo.";

fn notes_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "obra": {
                "type": "object",
                "properties": {
                    "genero": {"type": "string"},
                    "tono": {"type": "string"},
                    "resumen": {"type": "string"}
                },
                "required": ["genero", "tono", "resumen"],
                "additionalProperties": false
            },
            "personajes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "nombre": {"type": "string"},
                        "descripcion": {"type": "string"},
                        "relaciones": {"type": "string"},
                        "habla": {"type": "string"}
                    },
                    "required": ["nombre", "descripcion", "relaciones", "habla"],
                    "additionalProperties": false
                }
            },
            "terminos": terms_schema()
        },
        "required": ["obra", "personajes", "terminos"],
        "additionalProperties": false
    })
}

fn render_notes(name: &str, notes: &Notes) -> String {
    let mut text = format!(
        "# Ficha: {name}\n\n\
         <!-- Generada por khr estudiar. Edítala a mano: el traductor y el corrector la leen tal cual. -->\n\n\
         Género: {}\nTono: {}\nResumen: {}\n\n## Personajes\n",
        notes.obra.genero.trim(),
        notes.obra.tono.trim(),
        notes.obra.resumen.trim()
    );
    for character in &notes.personajes {
        text.push_str(&format!(
            "- {}: {} Relaciones: {} Habla: {}\n",
            character.nombre.trim(),
            character.descripcion.trim(),
            character.relaciones.trim(),
            character.habla.trim()
        ));
    }
    text
}

/// Models like to write `あなた (Anata)`; the glossary is matched against the
/// balloon text, so the romanisation must leave the source and go to the note.
fn split_reading(original: &str) -> (String, Option<String>) {
    let original = original.trim();
    if let Some(open) = original.rfind(['(', '（'])
        && open > 0
        && original.ends_with([')', '）'])
    {
        let inner = original[open..]
            .trim_start_matches(['(', '（'])
            .trim_end_matches([')', '）'])
            .trim();
        if !inner.is_empty() && inner.is_ascii() {
            return (original[..open].trim().to_owned(), Some(inner.to_owned()));
        }
    }
    (original.to_owned(), None)
}

fn to_glossary(terms: Vec<Term>) -> Glossary {
    Glossary {
        entries: terms
            .into_iter()
            .map(|term| {
                let (source, reading) = split_reading(&term.original);
                let note = term.nota.trim().replace(['\t', '\n'], " ");
                GlossaryEntry {
                    source,
                    target: term.traduccion.trim().to_owned(),
                    note: match reading {
                        Some(reading) if !note.contains(&reading) => format!("{reading}. {note}"),
                        _ => note,
                    },
                }
            })
            .filter(|entry| {
                !entry.source.is_empty()
                    && !entry.target.is_empty()
                    && entry.source.chars().count() <= LONGEST_TERM
                    && !entry.source.contains('\t')
            })
            .collect(),
    }
}

pub async fn study(
    project: &Path,
    base_url: &str,
    model: &str,
    right_to_left: bool,
    vision: bool,
    think: bool,
) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();
    let ids: Vec<EntityId> = snapshot.pages().map(|page| page.id()).collect();
    let pages = pages_in_order(&snapshot, right_to_left)?;

    // Each part is its text plus, with vision, the pages it covers.
    let mut chunks: Vec<(String, Vec<EntityId>)> = vec![(String::new(), Vec::new())];
    for (index, (id, blocks)) in ids.iter().zip(&pages).enumerate() {
        if blocks.is_empty() {
            continue;
        }
        let mut page = format!("Página {}:\n", index + 1);
        for block in blocks {
            page.push_str(&format!("- {}\n", block.source.replace('\n', " ")));
        }
        let (text, shown) = chunks.last_mut().expect("seeded with one chunk");
        let full = text.len() + page.len() > STUDY_CHUNK
            || (vision && shown.len() >= IMAGES_PER_PART);
        if !text.is_empty() && full {
            chunks.push((page, vec![*id]));
        } else {
            text.push_str(&page);
            shown.push(*id);
        }
    }
    chunks.retain(|(text, _)| !text.is_empty());
    anyhow::ensure!(!chunks.is_empty(), "the project has no recognized text; run OCR first");
    eprintln!(
        "studying {} page(s) in {} part(s){}{}",
        pages.len(),
        chunks.len(),
        if vision { ", with their images" } else { "" },
        if think { ", thinking" } else { "" }
    );

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut notes = Notes::default();
    for (index, (chunk, shown)) in chunks.iter().enumerate() {
        let user = serde_json::to_string(&serde_json::json!({
            "ficha_actual": notes,
            "texto": chunk,
        }))?;
        let mut content = vec![serde_json::json!({"type": "text", "text": user})];
        if vision {
            for id in shown {
                if let Some(url) = page_image_sized(&snapshot, *id, STUDY_IMAGE_SIDE).await? {
                    content.push(serde_json::json!({"type": "image_url", "image_url": {"url": url}}));
                }
            }
        }
        let request = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": STUDY_PROMPT},
                {"role": "user", "content": content},
            ],
            "temperature": 0.2,
            "stream": false,
            "reasoning_effort": if think { "medium" } else { "none" },
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "ficha", "strict": true, "schema": notes_schema()},
            },
        });
        let response: ChatResponse = client
            .post(&endpoint)
            .json(&request)
            .send()
            .await
            .with_context(|| format!("request to {endpoint} failed"))?
            .error_for_status()
            .with_context(|| format!("part {}: the model returned an error", index + 1))?
            .json()
            .await
            .context("malformed response")?;
        let reply = response
            .choices
            .first()
            .map(|choice| choice.message.content.as_str())
            .unwrap_or_default();
        match extract_json(reply)
            .and_then(|json| serde_json::from_str::<Notes>(json).ok())
        {
            Some(updated) => {
                notes = updated;
                eprintln!(
                    "  parte {}/{}: {} personaje(s), {} término(s)",
                    index + 1,
                    chunks.len(),
                    notes.personajes.len(),
                    notes.terminos.len()
                );
            }
            None => eprintln!("  parte {}/{}: respuesta ilegible, se omite", index + 1, chunks.len()),
        }
    }

    let work = Work::of(project);
    if let Some(previous) = work.notes() {
        let backup = work.notes_path().with_extension("anterior.md");
        std::fs::write(&backup, previous)?;
        eprintln!("previous notes kept in {}", backup.display());
    }
    let name = crate::obra::slug(project);
    let rendered = render_notes(&name, &notes);
    work.write_notes(&rendered)?;
    println!("{rendered}");
    eprintln!("notes written to {}", work.notes_path().display());

    let added = work.propose(to_glossary(std::mem::take(&mut notes.terminos)))?;
    eprintln!(
        "{added} new term(s) proposed in {}",
        work.proposals_path().display()
    );
    Ok(())
}

const PAGE_PROMPT: &str = "\
Eres un editor que prepara la traducción al español latino de un manga para adultos. \
No traduces: estudias UNA página para que el traductor sepa qué está pasando.

Recibes la ficha de la obra, lo que pasaba en la página anterior y los globos de esta \
página numerados en orden de lectura. Si viene la imagen de la página, mírala: los \
dibujos dicen quién habla (la cola del globo, quién tiene la boca abierta), qué hace \
cada uno y el ambiente.

Devuelve:
- escena: qué pasa en esta página en una a tres frases: quiénes están, qué hacen y el \
ambiente (sexo, discusión, comedia, rutina...). Si es sexual, dilo con claridad: el \
traductor lo necesita para no confundir un orgasmo con una despedida.
- globos: uno por cada globo recibido, con el mismo número:
  - habla: quién lo dice, con el nombre de la ficha; \"narrador\", \"sonido\" o \"?\" \
si no se puede saber.
  - a_quien: a quién se dirige, o \"?\".
  - nota: solo si la frase es ambigua fuera de contexto: qué significa aquí, el sujeto \
omitido o el doble sentido. Vacío si se entiende sola.

No inventes: si ni el texto ni la imagen lo dicen, pon \"?\". Escribe en español.";

fn page_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "escena": {"type": "string"},
            "globos": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "n": {"type": "integer"},
                        "habla": {"type": "string"},
                        "a_quien": {"type": "string"},
                        "nota": {"type": "string"}
                    },
                    "required": ["n", "habla", "a_quien", "nota"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["escena", "globos"],
        "additionalProperties": false
    })
}

#[derive(serde::Deserialize)]
struct PageReply {
    escena: String,
    globos: Vec<BalloonReply>,
}

#[derive(serde::Deserialize)]
struct BalloonReply {
    n: usize,
    habla: String,
    a_quien: String,
    nota: String,
}

/// Longest side, in pixels, of the page shown to the model. A vision model
/// spends tokens in proportion to the pixels; balloons stay legible at this
/// size and the reply still fits the context.
const PAGE_SIDE: u32 = 1280;

/// The page's source image as a JPEG data URL, scaled down to `PAGE_SIDE`.
async fn page_image(snapshot: &Snapshot, page: EntityId) -> Result<Option<String>> {
    page_image_sized(snapshot, page, PAGE_SIDE).await
}

async fn page_image_sized(
    snapshot: &Snapshot,
    page: EntityId,
    side: u32,
) -> Result<Option<String>> {
    use base64::Engine as _;
    let role = koharu_scene::AssetRole::new("source")?;
    let Some(asset) = snapshot.asset(page, &role)? else {
        return Ok(None);
    };
    let bytes = snapshot.read_blob(asset.blob).await?;
    let image = image::load_from_memory(&bytes).context("failed to decode the page image")?;
    let image = if image.width().max(image.height()) > side {
        image.resize(side, side, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image
        .to_rgb8()
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .context("failed to encode the page image")?;
    Ok(Some(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(jpeg.into_inner())
    )))
}

/// Studies the work page by page: what happens and who says each balloon.
///
/// The notes on the whole work say who the characters are; they cannot say
/// that on page 12 it is Mio who says "出た" about Kaito. That is what this
/// pass writes, one page at a time, carrying the previous page's scene along so
/// a scene that spans pages is not read cold. With `vision`, the model also
/// sees the page, which is often the only place the speaker is recorded.
pub async fn study_pages(
    project: &Path,
    base_url: &str,
    model: &str,
    right_to_left: bool,
    vision: bool,
    limit: Option<usize>,
) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();
    let ids: Vec<EntityId> = snapshot.pages().map(|page| page.id()).collect();
    let pages = pages_in_order(&snapshot, right_to_left)?;
    let work = Work::of(project);
    let notes = work.notes().unwrap_or_default();
    if notes.is_empty() {
        eprintln!("no notes for this work yet; run `khr estudiar` first for better results");
    }
    let count = limit.unwrap_or(pages.len()).min(pages.len());
    eprintln!(
        "studying {count} page(s) {}",
        if vision { "with their images" } else { "from their text only" }
    );

    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));
    let mut studied: Vec<PageNote> = Vec::new();
    let mut previous = String::new();
    for (index, (id, blocks)) in ids.iter().zip(&pages).take(count).enumerate() {
        if blocks.is_empty() {
            continue;
        }
        let balloons: Vec<_> = blocks
            .iter()
            .enumerate()
            .map(|(n, block)| serde_json::json!({"n": n + 1, "texto": block.source}))
            .collect();
        let text = serde_json::to_string(&serde_json::json!({
            "ficha": notes,
            "pagina_anterior": if previous.is_empty() { "(ninguna)" } else { previous.as_str() },
            "pagina": index + 1,
            "globos": balloons,
        }))?;
        let mut content = vec![serde_json::json!({"type": "text", "text": text})];
        if vision {
            match page_image(&snapshot, *id).await? {
                Some(url) => content.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {"url": url},
                })),
                None => eprintln!("  página {}: sin imagen, solo texto", index + 1),
            }
        }
        let request = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": PAGE_PROMPT},
                {"role": "user", "content": content},
            ],
            "temperature": 0.2,
            "stream": false,
            "reasoning_effort": "none",
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "pagina", "strict": true, "schema": page_schema()},
            },
        });
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
        let Some(parsed) = extract_json(reply)
            .and_then(|json| serde_json::from_str::<PageReply>(json).ok())
        else {
            eprintln!("  página {}: respuesta ilegible, se omite", index + 1);
            continue;
        };
        let globos = blocks
            .iter()
            .enumerate()
            .map(|(n, block)| {
                let found = parsed.globos.iter().find(|balloon| balloon.n == n + 1);
                BalloonNote {
                    original: block.source.clone(),
                    habla: found.map(|b| b.habla.clone()).unwrap_or_else(|| "?".to_owned()),
                    a_quien: found.map(|b| b.a_quien.clone()).unwrap_or_else(|| "?".to_owned()),
                    nota: found.map(|b| b.nota.clone()).unwrap_or_default(),
                }
            })
            .collect();
        eprintln!("  página {}: {}", index + 1, parsed.escena.trim());
        previous = parsed.escena.clone();
        studied.push(PageNote {
            pagina: index + 1,
            id: id.to_string(),
            escena: parsed.escena,
            globos,
        });
    }

    anyhow::ensure!(!studied.is_empty(), "no page could be studied");
    let path = work.page_notes_path();
    if path.exists() {
        let backup = path.with_extension("anterior.json");
        std::fs::copy(&path, &backup)?;
        eprintln!("previous page study kept in {}", backup.display());
    }
    // Pages outside this run keep their earlier study.
    let fresh = studied.len();
    let mut merged: Vec<PageNote> = work
        .page_notes()
        .into_iter()
        .filter(|old| !studied.iter().any(|new| new.id == old.id))
        .collect();
    merged.append(&mut studied);
    merged.sort_by_key(|note| note.pagina);
    work.write_page_notes(&merged)?;
    eprintln!("{fresh} page(s) studied, written to {}", path.display());
    Ok(())
}

const LEARN_PROMPT: &str = "\
Eres un terminólogo. Recibes globos de un manga con tres textos: el ORIGINAL, lo que \
escribió la MÁQUINA y lo que dejó el USUARIO al corregirlo a mano. Tu tarea es \
descubrir qué decisiones de VOCABULARIO revela la corrección del usuario, para \
aplicarlas en el resto de la obra.

Devuelve solo términos reutilizables: nombres propios, apodos, honoríficos, formas \
de llamarse, jerga (sexual o no) y onomatopeyas que el usuario tradujo de una forma \
concreta. \"original\" es la palabra o expresión corta del texto ORIGINAL; \
\"traduccion\" es cómo la escribió el USUARIO; \"nota\" explica en pocas palabras \
cuándo aplica.

NO devuelvas: frases completas, cambios de puntuación o de orden, arreglos \
gramaticales de una sola vez, ni términos que la máquina ya había traducido igual \
que el usuario. Si una corrección no revela ningún término, no devuelvas nada por ella.";

pub async fn learn(project: &Path, base_url: &str, model: &str) -> Result<()> {
    let session = Session::open(project)
        .await
        .with_context(|| format!("failed to open {}", project.display()))?;
    let snapshot = session.snapshot();
    let work = Work::of(project);
    let mut baseline = work.baseline();

    let mut edits: Vec<(String, String, String, String)> = Vec::new();
    let mut unknown = 0_usize;
    for block in pages_in_order(&snapshot, true)?.into_iter().flatten() {
        let Some(translation) = &block.translation else {
            continue;
        };
        let key = block.id.to_string();
        let current = translation.text.value.trim();
        match baseline.get(&key) {
            Some(machine) if machine.maquina.trim() != current => edits.push((
                key,
                block.source.clone(),
                machine.maquina.clone(),
                current.to_owned(),
            )),
            Some(_) => {}
            None if matches!(translation.text.origin, Origin::User) => unknown += 1,
            None => {}
        }
    }
    if unknown > 0 {
        eprintln!(
            "{unknown} balloon(s) were edited by hand before khr recorded the machine's \
             version; they cannot be compared"
        );
    }
    if edits.is_empty() {
        eprintln!("no hand edits since the last run");
        return record_machine(project, &snapshot, &BTreeMap::new());
    }
    eprintln!("learning from {} hand edit(s)", edits.len());

    let client = reqwest::Client::new();
    let mut found: Vec<Term> = Vec::new();
    for chunk in edits.chunks(LEARN_BATCH) {
        let user = serde_json::to_string(&serde_json::json!({
            "globos": chunk
                .iter()
                .map(|(_, original, machine, human)| serde_json::json!({
                    "original": original,
                    "maquina": machine,
                    "usuario": human,
                }))
                .collect::<Vec<_>>(),
        }))?;
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "terminos": terms_schema() },
            "required": ["terminos"],
            "additionalProperties": false
        });
        let Some(reply) =
            chat_json(&client, base_url, model, LEARN_PROMPT, &user, "terminos", schema).await?
        else {
            eprintln!("  a batch returned no usable reply, skipped");
            continue;
        };
        let terms: Vec<Term> =
            serde_json::from_value(reply["terminos"].clone()).unwrap_or_default();
        // Keep only what the edits actually show: the term must be in an
        // original and its rendering in the user's text, or the model invented it.
        for term in terms {
            let source = term.original.to_lowercase();
            let target = term.traduccion.to_lowercase();
            if source.trim().is_empty() || target.trim().is_empty() {
                continue;
            }
            let shown = chunk.iter().any(|(_, original, _, human)| {
                original.to_lowercase().contains(source.trim())
                    && human.to_lowercase().contains(target.trim())
            });
            if shown {
                found.push(term);
            }
        }
    }

    let proposed = to_glossary(found);
    for entry in &proposed.entries {
        println!("{} -> {}  ({})", entry.source, entry.target, entry.note);
    }
    let added = work.propose(proposed)?;
    eprintln!(
        "{added} new term(s) proposed in {}",
        work.proposals_path().display()
    );

    // The user's version becomes the reference, so the same edit is not
    // learned twice.
    for (key, original, _, human) in edits {
        baseline.insert(
            key,
            Machine {
                original,
                maquina: human,
            },
        );
    }
    work.write_baseline(&baseline)?;
    record_machine(project, &snapshot, &BTreeMap::new())
}
