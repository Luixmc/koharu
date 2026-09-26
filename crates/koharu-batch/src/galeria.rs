//! Tags of an e-hentai gallery, to start a work's notes from.
//!
//! The gallery API needs the gallery's token as well as its number. Given only
//! the number, the token is read from the site's own search for `gid:<number>`,
//! and failing that from the listing that starts right after it.

use anyhow::{Context as _, Result};

use crate::obra::{UserNotes, Work};

const SITE: &str = "https://e-hentai.org";
const API: &str = "https://api.e-hentai.org/api.php";

/// What the gallery says about the work.
#[derive(Debug, Default, serde::Serialize)]
pub struct Gallery {
    pub galeria: u64,
    pub titulo: String,
    pub titulo_original: String,
    pub etiquetas: Vec<String>,
}

/// Reads a gallery number and, when present, its token from a number, a
/// `number/token` pair or a gallery link (e-hentai or exhentai).
pub fn parse(input: &str) -> Option<(u64, Option<String>)> {
    let input = input.trim();
    let path = match input.find("/g/") {
        Some(start) => &input[start + 3..],
        None => input,
    };
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let gid = parts.next()?.parse().ok()?;
    let token = parts
        .next()
        .filter(|token| !token.is_empty() && token.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned);
    Some((gid, token))
}

/// The token that follows `/g/<gid>/` in a page of the site, if listed there.
fn token_in(page: &str, gid: u64) -> Option<String> {
    let marker = format!("/g/{gid}/");
    page.match_indices(&marker).find_map(|(at, _)| {
        let token: String = page[at + marker.len()..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        (!token.is_empty()).then_some(token)
    })
}

async fn find_token(client: &reqwest::Client, gid: u64) -> Result<String> {
    let searches = [
        format!("{SITE}/?f_search=gid%3A{gid}"),
        format!("{SITE}/?next={}", gid + 1),
    ];
    for url in &searches {
        let page = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("request to {url} failed"))?
            .text()
            .await?;
        if let Some(token) = token_in(&page, gid) {
            return Ok(token);
        }
    }
    anyhow::bail!(
        "gallery {gid} is not listed on e-hentai (removed, or only on exhentai); paste its full link instead"
    )
}

pub async fn fetch(input: &str) -> Result<Gallery> {
    let (gid, token) =
        parse(input).with_context(|| format!("`{input}` is not a gallery number or link"))?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("khr/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let token = match token {
        Some(token) => token,
        None => find_token(&client, gid).await?,
    };
    let reply: serde_json::Value = client
        .post(API)
        .json(&serde_json::json!({
            "method": "gdata",
            "gidlist": [[gid, token]],
            "namespace": 1,
        }))
        .send()
        .await
        .context("request to the e-hentai API failed")?
        .error_for_status()?
        .json()
        .await
        .context("malformed reply from the e-hentai API")?;
    let data = &reply["gmetadata"][0];
    if let Some(error) = data["error"].as_str() {
        anyhow::bail!("e-hentai: {error}");
    }
    let text = |key: &str| data[key].as_str().unwrap_or_default().trim().to_owned();
    Ok(Gallery {
        galeria: gid,
        titulo: text("title"),
        titulo_original: text("title_jpn"),
        etiquetas: data["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tag| tag.as_str())
            .map(str::to_owned)
            .collect(),
    })
}

/// Adds the gallery's tags to what the user wrote about the work, and its
/// title when there is no description yet.
pub fn merge(notes: &mut UserNotes, gallery: &Gallery) {
    for tag in &gallery.etiquetas {
        if !notes.etiquetas.iter().any(|known| known.trim() == tag) {
            notes.etiquetas.push(tag.clone());
        }
    }
    if notes.descripcion.trim().is_empty() {
        let title = if gallery.titulo_original.is_empty() {
            &gallery.titulo
        } else {
            &gallery.titulo_original
        };
        if !title.is_empty() {
            notes.descripcion = format!("Título: {title}");
        }
    }
}

pub async fn run(input: &str, project: Option<&std::path::Path>) -> Result<()> {
    let gallery = fetch(input).await?;
    eprintln!("gallery {}: {} tag(s)", gallery.galeria, gallery.etiquetas.len());
    if let Some(project) = project {
        let work = Work::of(project);
        let mut notes = work.user_notes().unwrap_or_default();
        merge(&mut notes, &gallery);
        work.write_user_notes(&notes)?;
        eprintln!("added to {}", work.user_notes_path().display());
    }
    println!("{}", serde_json::to_string(&gallery)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_pairs_and_links_are_read() {
        assert_eq!(parse("618395"), Some((618395, None)));
        assert_eq!(parse(" 618395/0439fa3666 "), Some((618395, Some("0439fa3666".to_owned()))));
        assert_eq!(
            parse("https://e-hentai.org/g/618395/0439fa3666/"),
            Some((618395, Some("0439fa3666".to_owned())))
        );
        assert_eq!(parse("https://exhentai.org/g/618395/"), Some((618395, None)));
        assert_eq!(parse("hola"), None);
    }

    #[test]
    fn the_token_is_taken_from_the_gallery_link() {
        let page = r#"<a href="https://e-hentai.org/g/618394/76604b00ee/"></a>
                      <a href="https://e-hentai.org/g/618395/0439fa3666/"></a>"#;
        assert_eq!(token_in(page, 618395).as_deref(), Some("0439fa3666"));
        assert_eq!(token_in(page, 1), None);
    }

    #[test]
    fn merging_keeps_the_users_text() {
        let gallery = Gallery {
            galeria: 1,
            titulo: "Title".to_owned(),
            titulo_original: "題名".to_owned(),
            etiquetas: vec!["female:milf".to_owned(), "romance".to_owned()],
        };
        let mut notes = UserNotes {
            etiquetas: vec!["romance".to_owned()],
            descripcion: String::new(),
        };
        merge(&mut notes, &gallery);
        assert_eq!(notes.etiquetas, ["romance", "female:milf"]);
        assert_eq!(notes.descripcion, "Título: 題名");

        notes.descripcion = "Mía".to_owned();
        merge(&mut notes, &gallery);
        assert_eq!(notes.descripcion, "Mía");
    }
}
