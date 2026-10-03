//! Turns the steps the user ticks for a project into the queue of khr and
//! LM Studio calls that carry them out. Koharu's own models run first, while
//! the card is free of the LLM; then each LM Studio model is loaded once for
//! the steps that use it, freeing the previous one, since two do not fit.

use std::path::Path;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use specta::Type;

use super::files::{self, UserNotes};
use super::queue::{After, Step};

const MODEL_CHOICES: &str = r"I:\Koharu\modelos.json";

/// Source languages; khr picks the OCR that reads each one best.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    Ja,
    Ko,
    Zh,
    En,
}

impl SourceLanguage {
    fn code(self) -> &'static str {
        match self {
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::Zh => "zh",
            Self::En => "en",
        }
    }
}

/// The LM Studio model each step uses, remembered between runs.
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ModelChoices {
    pub study: String,
    pub translation: String,
    /// Translate with DeepL instead of the translation model.
    pub deepl: bool,
    pub review: String,
}

impl Default for ModelChoices {
    fn default() -> Self {
        // Cydonia corrects Spanish better than Gemma (adverbs, word order),
        // so it studies and reviews; Gemma translates.
        Self {
            study: "thedrummer_cydonia-24b-v4.3".to_owned(),
            translation: "gemma-4-12b-it-qat".to_owned(),
            deepl: false,
            review: "thedrummer_cydonia-24b-v4.3".to_owned(),
        }
    }
}

impl ModelChoices {
    pub(crate) fn load() -> Self {
        std::fs::read_to_string(MODEL_CHOICES)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) -> Result<()> {
        std::fs::write(MODEL_CHOICES, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum KoharuStage {
    Detection,
    Ocr,
    Inpainting,
}

impl KoharuStage {
    fn key(self) -> &'static str {
        match self {
            Self::Detection => "detection",
            Self::Ocr => "ocr",
            Self::Inpainting => "inpainting",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Detection => "1. Detectar globos",
            Self::Ocr => "2. Reconocer texto (OCR)",
            Self::Inpainting => "3. Borrar texto original",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct Plan {
    pub project: String,
    pub stages: Vec<KoharuStage>,
    /// Step 4: read the whole work and write its ficha and term proposals.
    pub study: bool,
    /// e-hentai gallery whose tags are fetched right before the study.
    pub gallery: String,
    pub notes: UserNotes,
    pub translate: bool,
    pub review: bool,
    pub models: ModelChoices,
    /// Only the first pages; 0 is every page.
    #[specta(type = f64)]
    pub pages: u32,
    pub language: SourceLanguage,
    pub left_to_right: bool,
}

/// The steps of `plan` on the project at `project`. Writes the user's notes
/// for the study, which reads them from the work folder.
pub(crate) fn steps(plan: &Plan, project: &Path) -> Result<Vec<Step>> {
    let llm_translation = plan.translate && !plan.models.deepl;
    if plan.stages.is_empty() && !plan.study && !plan.translate && !plan.review {
        bail!("Marca al menos un paso.");
    }
    for (needed, model, what) in [
        (llm_translation, &plan.models.translation, "traducción"),
        (plan.study, &plan.models.study, "la ficha"),
        (plan.review, &plan.models.review, "revisión"),
    ] {
        if needed && model.trim().is_empty() {
            bail!("Elige el modelo de {what}.");
        }
    }
    if plan.study {
        files::write_notes(&files::dir(project), &plan.notes)?;
    }

    let project_arg = project.display().to_string();
    let pages = plan.pages.to_string();
    let page_args: Vec<&str> = if plan.pages > 0 {
        vec!["--pages", &pages]
    } else {
        Vec::new()
    };
    let khr = |label: &str, args: &[&str]| Step::khr(Some(project), label, args);
    let mut steps = Vec::new();

    if !plan.stages.is_empty() {
        steps.push(khr("Liberar VRAM", &["models", "unload"]).may_fail());
        let joined = plan
            .stages
            .iter()
            .map(|stage| stage.key())
            .collect::<Vec<_>>()
            .join(",");
        let mut args = vec![
            "run",
            "--project",
            &project_arg,
            "--stages",
            &joined,
            "--idioma",
            plan.language.code(),
        ];
        args.extend(&page_args);
        let label = plan
            .stages
            .iter()
            .map(|stage| stage.label())
            .collect::<Vec<_>>()
            .join(" · ");
        steps.push(khr(&label, &args));
    }
    if plan.translate && plan.models.deepl {
        let mut args = vec!["run", "--project", &project_arg, "--stages", "translation"];
        args.extend(&page_args);
        steps.push(khr("5. Traducir con DeepL", &args));
    }
    if plan.study || llm_translation || plan.review {
        steps.push(
            Step::lm_studio(
                Some(project),
                "Iniciar servidor de LM Studio",
                &["server", "start"],
            )
            .may_fail(),
        );
    }

    let mut loaded: Option<&str> = None;
    let study_model = plan.models.study.trim();
    let translation_model = plan.models.translation.trim();
    let review_model = plan.models.review.trim();

    if plan.study {
        let gallery = plan.gallery.trim();
        if !gallery.is_empty() {
            steps.push(
                khr(
                    "Traer etiquetas de la galería",
                    &["etiquetas", "--galeria", gallery, "--project", &project_arg],
                )
                .may_fail(),
            );
        }
        switch_model(&mut steps, &mut loaded, study_model, project);
        let mut args = vec![
            "estudiar",
            "--project",
            &project_arg,
            "--model",
            study_model,
        ];
        if plan.left_to_right {
            args.push("--left-to-right");
        }
        // Translating right after studying: the new terms go in approved.
        let after = if plan.translate {
            After::ApproveProposals
        } else {
            After::Nothing
        };
        steps.push(khr("4. Estudiar la obra (ficha y términos)", &args).then(after));
    }
    if llm_translation {
        switch_model(&mut steps, &mut loaded, translation_model, project);
        let mut args = vec![
            "run",
            "--project",
            &project_arg,
            "--stages",
            "translation",
            "--translator",
            translation_model,
            "--without-pages",
        ];
        args.extend(&page_args);
        steps.push(khr(&format!("5. Traducir con {translation_model}"), &args));
    }
    if plan.review {
        switch_model(&mut steps, &mut loaded, review_model, project);
        let mut args = vec![
            "revisar",
            "--project",
            &project_arg,
            "--model",
            review_model,
        ];
        args.extend(&page_args);
        if plan.left_to_right {
            args.push("--left-to-right");
        }
        steps.push(khr(&format!("6. Revisar con {review_model}"), &args));
    }
    if let Some(loaded) = loaded {
        let after = if plan.review {
            After::ShowCorrections
        } else if plan.study {
            After::ShowProposals
        } else {
            After::Nothing
        };
        steps.push(
            khr("Liberar modelo de LM Studio", &["models", "unload", loaded])
                .may_fail()
                .then(after),
        );
    }
    Ok(steps)
}

/// Queues loading `model`, freeing the one loaded before if it differs.
fn switch_model<'a>(
    steps: &mut Vec<Step>,
    loaded: &mut Option<&'a str>,
    model: &'a str,
    project: &Path,
) {
    if *loaded == Some(model) {
        return;
    }
    if let Some(previous) = loaded.take() {
        steps.push(
            Step::khr(
                Some(project),
                "Liberar modelo de LM Studio",
                &["models", "unload", previous],
            )
            .may_fail(),
        );
    }
    steps.push(Step::khr(
        Some(project),
        format!("Cargar {model}"),
        &["models", "load", model],
    ));
    *loaded = Some(model);
}

/// Learning from the user's own corrections: proposes glossary terms.
pub(crate) fn learn(project: &Path, model: &str) -> Vec<Step> {
    let project_arg = project.display().to_string();
    let khr = |label: &str, args: &[&str]| Step::khr(Some(project), label, args);
    vec![
        Step::lm_studio(
            Some(project),
            "Iniciar servidor de LM Studio",
            &["server", "start"],
        )
        .may_fail(),
        khr(&format!("Cargar {model}"), &["models", "load", model]),
        khr(
            "Aprender de mis correcciones",
            &["aprender", "--project", &project_arg, "--model", model],
        ),
        khr("Liberar modelo de LM Studio", &["models", "unload", model])
            .may_fail()
            .then(After::ShowProposals),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            project: "Obra".to_owned(),
            stages: vec![KoharuStage::Detection, KoharuStage::Ocr],
            study: false,
            gallery: String::new(),
            notes: UserNotes::default(),
            translate: true,
            review: true,
            models: ModelChoices {
                study: "a".to_owned(),
                translation: "a".to_owned(),
                deepl: false,
                review: "b".to_owned(),
            },
            pages: 0,
            language: SourceLanguage::Ja,
            left_to_right: false,
        }
    }

    #[test]
    fn each_model_is_loaded_once_and_freed_before_the_next() {
        let steps = steps(&plan(), Path::new(r"C:\p\Obra.khrproj")).unwrap();
        let calls: Vec<String> = steps
            .iter()
            .filter(|step| step.args.first().is_some_and(|arg| arg == "models"))
            .map(|step| step.args.join(" "))
            .collect();
        assert_eq!(
            calls,
            [
                "models unload",
                "models load a",
                "models unload a",
                "models load b",
                "models unload b"
            ]
        );
        assert_eq!(steps.last().unwrap().after, After::ShowCorrections);
    }

    #[test]
    fn a_plan_needs_a_step() {
        let empty = Plan {
            stages: Vec::new(),
            translate: false,
            review: false,
            ..plan()
        };
        assert!(steps(&empty, Path::new(r"C:\p\Obra.khrproj")).is_err());
    }
}
