//! Lines that only their scene can disambiguate.
//!
//! "I'm coming" is a farewell at the door and a climax in bed; "it's so hard"
//! is an exam or an erection. The per-sentence bench cannot tell whether a
//! translator reads the scene, because it hands over isolated lines. Here every
//! case carries the balloons that precede it on the page and goes through the
//! same translator, prompt and instructions as a real page, so DeepL and a
//! local model are measured under identical conditions, with and without the
//! work's notes and glossary.
//!
//! With `--corrector`, the whole scene then goes through the same correction
//! pass as `khr post`, so "DeepL translates, a local model corrects with the
//! notes" can be weighed against "the local model translates with the notes".
//! Cases carry their source language, and scores are kept per language because
//! the answer need not be the same for English as for Japanese.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use koharu_pipeline::PipelineConfig;
use koharu_translator::{
    Glossary, Language, ModelSelection, Provider, ProvidersConfig, TranslationRequest, Translator,
};

use crate::{
    ChatMessage, ChatRequest, ChatResponse, CorrectionBatch, FORMAT_APPENDIX, correction_schema,
    extract_json, plausible_correction,
};

const BUNDLED: &str = include_str!("../assets/escenas.json");

#[derive(serde::Deserialize)]
struct CaseFile {
    ficha: String,
    glosario: String,
    casos: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    escena: Vec<String>,
    #[serde(rename = "ref")]
    reference: String,
    check: String,
    value: Vec<String>,
    trampa: String,
    #[serde(default)]
    control: bool,
    #[serde(default = "english")]
    idioma: String,
}

fn english() -> String {
    "en".to_owned()
}

fn passes(case: &Case, reply: &str) -> bool {
    let lowered = reply.to_lowercase();
    let any = case
        .value
        .iter()
        .any(|needle| lowered.contains(&needle.to_lowercase()));
    match case.check.as_str() {
        "contiene" => any,
        "no_contiene" => !reply.trim().is_empty() && !any,
        _ => false,
    }
}

/// The source language of a case and the prompt a real page in it would use.
fn language(code: &str) -> Result<(Language, &'static str)> {
    Ok(match code {
        "en" => (Language::English, "01-ingles.txt"),
        "ja" => (Language::Japanese, "02-japones.txt"),
        "ko" => (Language::Korean, "03-coreano.txt"),
        "zh" => (Language::ChineseSimplified, "04-chino.txt"),
        other => anyhow::bail!("unknown case language `{other}`; expected en, ja, ko or zh"),
    })
}

pub struct Options {
    pub provider: String,
    pub model: Option<String>,
    pub without_notes: bool,
    pub cases: Option<PathBuf>,
    pub runs: usize,
    pub corrector: Option<String>,
    pub corrector_prompt: PathBuf,
    pub prompts: PathBuf,
    pub base_url: String,
    pub idioma: Option<String>,
    pub resultados: Option<PathBuf>,
    pub etiqueta: Option<String>,
}

/// Sends a scene through the correction pass and returns its last line.
///
/// Mirrors `khr post`: the same prompt, format appendix, notes and glossary in
/// the system message, and the same plausibility filter, so a line the real
/// corrector would have discarded keeps its translation here too.
async fn correct(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    system: &str,
    sources: &[String],
    translations: &[String],
) -> Result<String> {
    let last = translations.last().cloned().unwrap_or_default();
    let payload = serde_json::json!({
        "bloques": sources
            .iter()
            .zip(translations)
            .enumerate()
            .map(|(id, (source, translation))| serde_json::json!({
                "id": id,
                "es": translation,
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
                content: system,
            },
            ChatMessage {
                role: "user",
                content: &user,
            },
        ],
        temperature: 0.2,
        stream: false,
        reasoning_effort: "none",
        response_format: correction_schema(),
    };
    let response: ChatResponse = client
        .post(endpoint)
        .json(&request)
        .send()
        .await
        .with_context(|| format!("request to {endpoint} failed"))?
        .error_for_status()
        .context("the corrector returned an error")?
        .json()
        .await
        .context("malformed response")?;
    let reply = response
        .choices
        .first()
        .map(|choice| choice.message.content.as_str())
        .unwrap_or_default();
    let Some(json) = extract_json(reply) else {
        return Ok(last);
    };
    let Ok(parsed) = serde_json::from_str::<CorrectionBatch>(json) else {
        return Ok(last);
    };
    let index = translations.len().saturating_sub(1);
    Ok(parsed
        .bloques
        .into_iter()
        .find(|item| item.id == index)
        .filter(|item| plausible_correction(&last, &sources[index], &item.es_corregido))
        .map(|item| item.es_corregido)
        .unwrap_or(last))
}

/// The translation prompt for a language, or the configured one when the
/// prompt folder has none.
fn read_prompt(dir: &Path, file: &str, fallback: Option<&str>) -> String {
    std::fs::read_to_string(dir.join(file))
        .ok()
        .or_else(|| fallback.map(str::to_owned))
        .unwrap_or_default()
}

pub async fn run(options: Options) -> Result<()> {
    let Options {
        provider,
        model,
        without_notes,
        cases,
        runs,
        corrector,
        corrector_prompt,
        prompts,
        base_url,
        idioma,
        resultados,
        etiqueta,
    } = options;
    anyhow::ensure!(runs > 0, "at least one run is required");
    let raw = match &cases {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?,
        None => BUNDLED.to_owned(),
    };
    let mut file: CaseFile = serde_json::from_str(&raw).context("malformed case file")?;
    if let Some(only) = &idioma {
        file.casos.retain(|case| &case.idioma == only);
        anyhow::ensure!(!file.casos.is_empty(), "no case in language `{only}`");
    }

    let pipeline = koharu_config::load::<PipelineConfig>("pipeline")?.read()?.clone();
    let providers = koharu_config::load::<ProvidersConfig>("providers")?;
    let selection = match provider.as_str() {
        "deepl" => ModelSelection {
            provider: Provider::DeepL,
            model: None,
            quantization: None,
            vision: false,
            reasoning: false,
        },
        "lm-studio" => ModelSelection {
            provider: Provider::LmStudio,
            model: Some(model.clone().context("--model is required for lm-studio")?),
            quantization: None,
            vision: false,
            reasoning: false,
        },
        other => anyhow::bail!("unknown provider `{other}`; expected deepl or lm-studio"),
    };
    let translator = Translator::from_config(koharu_ml::device(true), providers)?;
    let glossary = Glossary::parse(&file.glosario);
    let translator_label = match (&selection.provider, &selection.model) {
        (Provider::DeepL, _) => "DeepL".to_owned(),
        (_, Some(model)) => model.clone(),
        _ => provider.clone(),
    };
    let correction_system = match &corrector {
        Some(_) => {
            let text = std::fs::read_to_string(&corrector_prompt)
                .with_context(|| format!("failed to read {}", corrector_prompt.display()))?;
            format!("{}{FORMAT_APPENDIX}", text.trim())
        }
        None => String::new(),
    };
    let client = reqwest::Client::new();
    let endpoint = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

    // Per language: hits on traps and on controls, one entry per run.
    let mut scores: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for run in 1..=runs {
        eprintln!("--- run {run} of {runs} ---");
        let mut tally: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for case in &file.casos {
            let (source_language, prompt_file) = language(&case.idioma)?;
            let mut segments = case.escena.clone();
            segments.push(case.reference.clone());
            let texts: Vec<&str> = segments.iter().map(String::as_str).collect();
            let work = if without_notes {
                String::new()
            } else {
                koharu_translator::glossary::instructions_block(
                    Some(&file.ficha),
                    &glossary,
                    &texts,
                )
            };
            let base = read_prompt(
                &prompts,
                prompt_file,
                pipeline.translation.instructions.as_deref(),
            );
            let instructions = [base.as_str(), &work]
                .into_iter()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut request =
                TranslationRequest::new(segments.clone(), pipeline.translation.target_language);
            request.source_language = Some(source_language);
            if !instructions.is_empty() {
                request = request.with_instructions(instructions);
            }
            let translated = match translator
                .translate(&selection, pipeline.translation.generation, request)
                .await
            {
                Ok((_, translated)) => translated,
                Err(error) => {
                    eprintln!("  error: {error:#}");
                    Vec::new()
                }
            };
            let translated_last = translated.last().cloned().unwrap_or_default();
            let reply = match (&corrector, translated.len() == segments.len()) {
                (Some(model), true) => {
                    let system = if work.is_empty() {
                        correction_system.clone()
                    } else {
                        format!("{correction_system}\n\n{work}")
                    };
                    // A corrector that cannot be reached would leave every
                    // line uncorrected and score as if it had corrected them.
                    correct(&client, &endpoint, model, &system, &segments, &translated)
                        .await
                        .context("the corrector failed; is the LM Studio server running?")?
                }
                _ => translated_last.clone(),
            };
            let ok = passes(case, &reply);
            let entry = tally.entry(case.idioma.clone()).or_default();
            match (ok, case.control) {
                (true, false) => entry.0 += 1,
                (true, true) => entry.1 += 1,
                _ => {}
            }
            if !ok && run == 1 {
                eprintln!(
                    "  [{}] FALLA{}: {}",
                    case.idioma,
                    if case.control { " (control)" } else { "" },
                    case.trampa
                );
                if corrector.is_some() && reply != translated_last {
                    eprintln!("      {} -> {translated_last:?} -> {reply:?}", case.reference);
                } else {
                    eprintln!("      {} -> {reply:?}", case.reference);
                }
            }
        }
        for (code, score) in tally {
            eprintln!("  [{code}] trampas {}, controles {}", score.0, score.1);
            scores.entry(code).or_default().push(score);
        }
    }

    let notes_label = match (&selection.provider, without_notes, &corrector) {
        (_, true, _) => "sin ficha ni glosario",
        (Provider::DeepL, false, None) => "DeepL no recibe ficha ni glosario",
        (Provider::DeepL, false, Some(_)) => "ficha y glosario solo en la corrección",
        (_, false, _) => "con ficha y glosario",
    };
    let label = match &corrector {
        Some(model) => format!("{translator_label} -> corrige {model}"),
        None => translator_label.clone(),
    };
    println!();
    println!("=== {label} | {notes_label} ===");
    let mut rows = Vec::new();
    for code in file
        .casos
        .iter()
        .map(|case| case.idioma.clone())
        .collect::<std::collections::BTreeSet<_>>()
    {
        // A language whose every case failed in a run left no tally entry.
        let mut entries = scores.get(&code).cloned().unwrap_or_default();
        entries.resize(runs, (0, 0));
        let traps = file
            .casos
            .iter()
            .filter(|case| case.idioma == code && !case.control)
            .count();
        let controls = file
            .casos
            .iter()
            .filter(|case| case.idioma == code && case.control)
            .count();
        let mean = |pick: fn(&(usize, usize)) -> usize| {
            entries.iter().map(pick).sum::<usize>() as f64 / entries.len() as f64
        };
        let (trap_mean, control_mean) = (mean(|score| score.0), mean(|score| score.1));
        println!(
            "[{code}] trampas {trap_mean:.1}/{traps}  controles {control_mean:.1}/{controls}  global {:.0}%",
            (trap_mean + control_mean) / (traps + controls) as f64 * 100.0
        );
        rows.push(format!(
            "{}\t{translator_label}\t{}\t{}\t{code}\t{trap_mean:.1}\t{traps}\t{control_mean:.1}\t{controls}\t{runs}",
            etiqueta.as_deref().unwrap_or("-"),
            corrector.as_deref().unwrap_or("-"),
            if without_notes { "no" } else { "sí" },
        ));
    }

    if let Some(path) = resultados {
        let new = !path.exists();
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        if new {
            writeln!(
                out,
                "etiqueta\ttraductor\tcorrector\tficha\tidioma\ttrampas\tde\tcontroles\tde_c\tpasadas"
            )?;
        }
        for row in rows {
            writeln!(out, "{row}")?;
        }
    }
    Ok(())
}
