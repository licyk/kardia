use crate::Request;

struct Media {
    kind: String,
    subtype: String,
    params: Vec<(String, String)>,
    quality: f64,
    order: usize,
}

impl Request {
    pub fn accepted_types(&self) -> Vec<String> {
        let mut media = accepted(self);
        media.retain(|m| m.quality > 0.0);
        media.sort_by(|a, b| b.quality.total_cmp(&a.quality).then(a.order.cmp(&b.order)));
        media.into_iter().map(|m| format!("{}/{}", m.kind, m.subtype)).collect()
    }

    pub fn accepts<'a>(&self, types: &'a [&str]) -> Option<&'a str> {
        if self.headers.get("accept").is_none_or(|header| header.is_empty()) {
            return types.first().copied();
        }
        let accepted = accepted(self);
        let mut matches = Vec::new();
        for (index, candidate) in types.iter().enumerate() {
            let mime = if candidate.contains('/') {
                (*candidate).to_owned()
            } else {
                let Some(mime) = crate::mime::lookup(candidate) else { continue };
                mime.to_owned()
            };
            let Some(media) = parse(&mime, index) else { continue };
            let mut best = None;
            for spec in &accepted {
                let Some(specificity) = specificity(&media, spec) else { continue };
                let priority = (specificity, spec.quality, spec.order);
                if best.is_none_or(|old| priority > old) {
                    best = Some(priority);
                }
            }
            if let Some((specificity, quality, order)) = best.filter(|(_, q, _)| *q > 0.0) {
                matches.push((index, specificity, quality, order));
            }
        }
        matches.sort_by(|a, b| b.2.total_cmp(&a.2).then(b.1.cmp(&a.1)).then(a.3.cmp(&b.3)).then(a.0.cmp(&b.0)));
        matches.first().map(|m| types[m.0])
    }
}

fn accepted(req: &Request) -> Vec<Media> {
    let header = req.headers.get("accept").and_then(|v| v.to_str().ok()).unwrap_or("*/*");
    split(header, ',').into_iter().enumerate().filter_map(|(i, s)| parse(s, i)).collect()
}

fn split(value: &str, separator: char) -> Vec<&str> {
    let mut quoted = false;
    value
        .split(|ch| {
            if ch == '"' {
                quoted = !quoted;
            }
            ch == separator && !quoted
        })
        .collect()
}

fn parse(value: &str, order: usize) -> Option<Media> {
    let parts = split(value, ';');
    let (kind, subtype) = parts.first()?.trim().split_once('/')?;
    if kind.is_empty() || subtype.is_empty() || kind.chars().chain(subtype.chars()).any(char::is_whitespace) {
        return None;
    }
    let mut media = Media { kind: kind.into(), subtype: subtype.into(), params: Vec::new(), quality: 1.0, order };
    for part in &parts[1..] {
        let (key, value) = part.trim().split_once('=').unwrap_or((part.trim(), ""));
        let key = key.to_ascii_lowercase();
        let value = value.trim_matches('"');
        if key == "q" {
            media.quality = value.parse().unwrap_or(0.0);
            break;
        }
        media.params.push((key, value.into()));
    }
    Some(media)
}

fn specificity(candidate: &Media, spec: &Media) -> Option<u8> {
    let mut score = 0;
    if spec.kind.eq_ignore_ascii_case(&candidate.kind) {
        score |= 4;
    } else if spec.kind != "*" {
        return None;
    }
    if spec.subtype.eq_ignore_ascii_case(&candidate.subtype) {
        score |= 2;
    } else if spec.subtype != "*" {
        return None;
    }
    for (key, value) in &spec.params {
        let candidate = candidate.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or("");
        if value != "*" && !value.eq_ignore_ascii_case(candidate) {
            return None;
        }
    }
    if !spec.params.is_empty() {
        score |= 1;
    }
    Some(score)
}
