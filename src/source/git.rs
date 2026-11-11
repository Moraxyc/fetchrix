//! Ref advertisement of the Git smart HTTP protocol.

use super::SourceError;

pub struct Ref {
    pub oid: String,
    pub name: String,
}

pub fn refs_url(repo: &str) -> String {
    let repo = repo.trim_end_matches('/');
    let suffix = if repo.to_ascii_lowercase().ends_with(".git") {
        ""
    } else {
        ".git"
    };
    format!("{repo}{suffix}/info/refs?service=git-upload-pack")
}

/// Parses a protocol v0 ref advertisement made of pkt-lines.
pub fn parse_refs(mut body: &[u8]) -> Result<Vec<Ref>, SourceError> {
    let bad = |what: &str| SourceError::Decode(format!("malformed ref advertisement: {what}"));
    let mut refs = Vec::new();
    while !body.is_empty() {
        let len = body
            .get(..4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| usize::from_str_radix(h, 16).ok())
            .ok_or_else(|| bad("invalid length"))?;
        if len == 0 {
            body = &body[4..];
            continue;
        }
        let line = body.get(4..len).ok_or_else(|| bad("truncated line"))?;
        body = &body[len..];
        if line.starts_with(b"#") {
            continue;
        }
        let line = line.split(|&b| b == 0).next().unwrap_or_default();
        let line = std::str::from_utf8(line).map_err(|_| bad("non-UTF-8 ref"))?;
        let (oid, name) = line
            .trim_end()
            .split_once(' ')
            .ok_or_else(|| bad("missing ref name"))?;
        refs.push(Ref {
            oid: oid.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(refs)
}
