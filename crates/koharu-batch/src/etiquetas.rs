//! Turns e-hentai tags into something the study can use. Tags about the
//! scan or the upload (language, scanmark, tankoubon) say nothing about how
//! to translate and are dropped; the ones that decide tone, register or
//! vocabulary come with a short guide in Spanish, so the model does not have
//! to guess what "netorare" or "mind break" mean for the dialogue.
//!
//! `I:\Koharu\etiquetas.tsv` (tag, tab, guide) adds or overrides guides; a
//! guide of `-` drops the tag. `I:\Koharu\etiquetas-que-detienen.txt` lists,
//! one per line, the tags the user does not want to translate (gore,
//! yaoi...): the study stops on them, as it always does on minors.

use std::collections::BTreeMap;

const USER_GUIDES: &str = r"I:\Koharu\etiquetas.tsv";
const USER_STOPS: &str = r"I:\Koharu\etiquetas-que-detienen.txt";

/// Namespaces that describe the upload, not the story.
const DROPPED_NAMESPACES: [&str; 5] = ["language", "artist", "group", "reclass", "cosplayer"];

/// Tags of the `other` namespace that describe the story; the rest of
/// `other` (scanmark, tankoubon, censorship...) is dropped.
const KEPT_OTHER: [&str; 4] = ["story arc", "comedy", "full color", "incomplete"];

/// Tags that mean the work involves minors; the study stops on them.
const MINORS: [&str; 9] = [
    "lolicon",
    "loli",
    "shotacon",
    "low lolicon",
    "low shotacon",
    "toddlercon",
    "oppai loli",
    "schoolgirl uniform",
    "schoolboy uniform",
];

const GUIDES: [(&str, &str); 38] = [
    (
        "netorare",
        "infidelidad desde el engañado: humillación, celos, culpa; el amante suele hablar con burla y superioridad",
    ),
    (
        "cheating",
        "infidelidad: culpa y excusas al principio, descaro después; no suavizar",
    ),
    (
        "netori",
        "alguien roba la pareja de otro: el que roba habla con seguridad y desprecio hacia el engañado",
    ),
    (
        "netorase",
        "el engañado lo consiente o lo pide: mezcla de celos y excitación",
    ),
    (
        "mind break",
        "el habla cambia a lo largo de la obra: de pudorosa o firme a vulgar, sumisa y entrecortada; anotar desde qué página",
    ),
    (
        "corruption",
        "caída progresiva: vocabulario cada vez más sucio y sumiso; anotar el cambio por páginas",
    ),
    ("drugs", "habla confusa o arrastrada bajo efecto de drogas"),
    (
        "sole female",
        "una sola mujer: el género gramatical de la segunda persona casi siempre es femenino",
    ),
    (
        "sole male",
        "un solo hombre: el género gramatical de la segunda persona casi siempre es masculino",
    ),
    (
        "milf",
        "mujer madura: habla adulta, a veces maternal o autoritaria; los jóvenes pueden tratarla de usted al inicio",
    ),
    ("mature", "personajes maduros: registro adulto"),
    (
        "teacher",
        "docente: trato de usted o \"profesora/profesor\" por parte de los demás",
    ),
    (
        "slave",
        "esclavitud sexual: tratamiento de amo/ama; la esclava habla con sumisión (\"sí, amo\")",
    ),
    (
        "dominatrix",
        "ella domina: órdenes cortas, desprecio, apodos humillantes",
    ),
    (
        "femdom",
        "ella domina: órdenes cortas, desprecio, apodos humillantes",
    ),
    (
        "humiliation",
        "humillación: insultos y apodos degradantes; traducirlos con la misma dureza",
    ),
    (
        "exhibitionism",
        "exhibicionismo: miedo a ser descubiertos, susurros, frases cortadas",
    ),
    (
        "public use",
        "uso en público: varios hombres, órdenes y burlas en grupo",
    ),
    (
        "gangbang",
        "varios hombres: muchas voces cortas; distinguir quién habla por los globos vecinos",
    ),
    (
        "prostitution",
        "prostitución: trato de cliente, precios, lenguaje de negocio mezclado con sexo",
    ),
    (
        "blackmail",
        "chantaje: amenazas frías de un lado, miedo y resistencia del otro",
    ),
    (
        "rape",
        "sexo no consentido: resistencia, súplicas y amenazas; no suavizar ni volverlo consentido",
    ),
    (
        "ahegao",
        "gemidos y frases rotas: repetir sílabas, cortar palabras, sin gramática completa",
    ),
    (
        "dirty talk",
        "hablan sucio a propósito: vocabulario explícito, sin eufemismos",
    ),
    (
        "impregnation",
        "embarazar: \"preñar\", \"llenar\", \"echar adentro\"; mantener el mismo verbo",
    ),
    (
        "pregnant",
        "embarazo: \"panza\", \"embarazada\", \"preñada\" en boca vulgar",
    ),
    ("lactation", "leche materna: \"leche\", \"ordeñar\""),
    (
        "anal",
        "sexo anal: \"culo\", \"por atrás\"; mantener la misma palabra en toda la obra",
    ),
    (
        "paizuri",
        "paja con las tetas: usar siempre la misma expresión (\"rusa\" o \"entre las tetas\")",
    ),
    (
        "fellatio",
        "sexo oral a él: \"mamada\", \"chupar\"; mantener la misma palabra",
    ),
    (
        "cunnilingus",
        "sexo oral a ella: \"comer\", \"lamer\"; mantener la misma palabra",
    ),
    (
        "big penis",
        "se habla del tamaño: \"verga enorme\", comparaciones con la pareja",
    ),
    (
        "piercing",
        "piercings (en pezones, clítoris, lengua...): nombrarlos igual en toda la obra",
    ),
    (
        "tattoo",
        "tatuajes, a veces con texto obsceno: traducir el texto del tatuaje si aparece",
    ),
    (
        "incest",
        "incesto: los títulos familiares (mamá, hijo, hermana) se mantienen siempre, también en el sexo",
    ),
    (
        "inseki",
        "familia política (suegra, cuñada...): mantener el título familiar",
    ),
    (
        "harem",
        "harén: varias mujeres alrededor de un hombre; cada una con su forma de hablar",
    ),
    (
        "vanilla",
        "sexo consentido y cariñoso: tono afectuoso, sin insultos",
    ),
];

/// What the tags say, ready for the study.
#[derive(Debug, Default, serde::Serialize)]
pub struct Reading {
    /// The tags worth reading, without namespace.
    pub etiquetas: Vec<String>,
    /// "tag: guide" for the tags that decide how to translate.
    pub guia: Vec<String>,
    /// Tags that mean minors are involved.
    #[serde(skip)]
    pub menores: Vec<String>,
    /// Tags from the user's stop list.
    #[serde(skip)]
    pub rechazadas: Vec<String>,
}

fn user_guides() -> BTreeMap<String, String> {
    std::fs::read_to_string(USER_GUIDES)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let (tag, guide) = line.split_once('\t')?;
            Some((tag.trim().to_lowercase(), guide.trim().to_owned()))
        })
        .filter(|(tag, _)| !tag.is_empty())
        .collect()
}

/// The user's stop list, one tag per line; `#` starts a comment.
fn user_stops() -> Vec<String> {
    std::fs::read_to_string(USER_STOPS)
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_lowercase())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

pub fn read(tags: &[String]) -> Reading {
    read_with(tags, &user_guides(), &user_stops())
}

fn read_with(tags: &[String], extra: &BTreeMap<String, String>, stops: &[String]) -> Reading {
    let mut reading = Reading::default();
    for tag in tags {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() {
            continue;
        }
        let (namespace, name) = match tag.split_once(':') {
            Some((namespace, name)) => (namespace.trim(), name.trim()),
            None => ("", tag.as_str()),
        };
        if MINORS.contains(&name) {
            reading.menores.push(name.to_owned());
            continue;
        }
        if stops.iter().any(|stop| stop == name) {
            if !reading.rechazadas.iter().any(|known| known == name) {
                reading.rechazadas.push(name.to_owned());
            }
            continue;
        }
        if DROPPED_NAMESPACES.contains(&namespace)
            || (namespace == "parody" && name == "original")
            || (namespace == "other" && !KEPT_OTHER.contains(&name) && !extra.contains_key(name))
        {
            continue;
        }
        let guide = extra.get(name).map(String::as_str).or_else(|| {
            GUIDES
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, guide)| *guide)
        });
        if guide == Some("-") {
            continue;
        }
        let shown = match namespace {
            "female" => format!("{name} (ella)"),
            "male" => format!("{name} (él)"),
            "parody" => format!("parodia de {name}"),
            "character" => format!("personaje: {name}"),
            _ => name.to_owned(),
        };
        if reading.etiquetas.contains(&shown) {
            continue;
        }
        if let Some(guide) = guide {
            let line = format!("{name}: {guide}");
            if !reading.guia.contains(&line) {
                reading.guia.push(line);
            }
        }
        reading.etiquetas.push(shown);
    }
    reading
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|tag| (*tag).to_owned()).collect()
    }

    #[test]
    fn upload_tags_go_and_story_tags_come_with_a_guide() {
        let reading = read_with(
            &tags(&[
                "language:english",
                "artist:choma",
                "parody:original",
                "other:scanmark",
                "other:mosaic censorship",
                "female:big breasts",
                "female:netorare",
                "male:netorare",
                "female:mind break",
            ]),
            &BTreeMap::new(),
            &[],
        );
        assert_eq!(
            reading.etiquetas,
            [
                "big breasts (ella)",
                "netorare (ella)",
                "netorare (él)",
                "mind break (ella)"
            ]
        );
        assert_eq!(reading.guia.len(), 2);
        assert!(reading.guia[0].starts_with("netorare: infidelidad"));
        assert!(reading.menores.is_empty());
    }

    #[test]
    fn the_users_file_adds_and_drops_guides() {
        let extra = BTreeMap::from([
            ("big breasts".to_owned(), "-".to_owned()),
            (
                "tankoubon".to_owned(),
                "recopilación: varias historias".to_owned(),
            ),
        ]);
        let reading = read_with(
            &tags(&["female:big breasts", "other:tankoubon"]),
            &extra,
            &[],
        );
        assert_eq!(reading.etiquetas, ["tankoubon"]);
        assert_eq!(reading.guia, ["tankoubon: recopilación: varias historias"]);
    }
}
