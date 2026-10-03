//! curl's `-w/--write-out` template: `%{variable}`, `%%`, and the escapes
//! `\n`, `\r`, `\t`, `\\`.

/// The values a template can name.
pub struct Facts<'a> {
    pub http_code: u16,
    pub url_effective: &'a str,
    pub num_redirects: u32,
    pub size_download: u64,
    pub content_type: &'a str,
}

/// Expand `template`. An unknown variable expands to nothing, as in curl.
pub fn expand(template: &str, facts: &Facts) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("%{") {
            if let Some(end) = after.find('}') {
                out.push_str(&variable(&after[..end], facts));
                rest = &after[end + 1..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix("%%") {
            out.push('%');
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('\\') {
            if let Some(e) = after.chars().next() {
                let mapped = match e {
                    'n' => Some('\n'),
                    'r' => Some('\r'),
                    't' => Some('\t'),
                    '\\' => Some('\\'),
                    _ => None,
                };
                if let Some(m) = mapped {
                    out.push(m);
                    rest = &after[1..];
                    continue;
                }
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn variable(name: &str, facts: &Facts) -> String {
    match name {
        "http_code" | "response_code" => format!("{:03}", facts.http_code),
        "url_effective" | "url" => facts.url_effective.to_string(),
        "num_redirects" => facts.num_redirects.to_string(),
        "size_download" => facts.size_download.to_string(),
        "content_type" => facts.content_type.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACTS: Facts = Facts {
        http_code: 200,
        url_effective: "https://h/x",
        num_redirects: 2,
        size_download: 1234,
        content_type: "text/html",
    };

    #[test]
    fn expands_variables_and_escapes() {
        assert_eq!(expand("%{http_code}", &FACTS), "200");
        assert_eq!(expand("%{http_code}\\n", &FACTS), "200\n");
        assert_eq!(
            expand("%{response_code} %{url_effective} %{num_redirects} %{size_download} %{content_type}", &FACTS),
            "200 https://h/x 2 1234 text/html"
        );
        assert_eq!(expand("100%% %{nope}|%{open", &FACTS), "100% |%{open");
        assert_eq!(expand("\\q\\t", &FACTS), "\\q\t");
        let zero = Facts {
            http_code: 0,
            ..FACTS
        };
        assert_eq!(expand("%{http_code}", &zero), "000");
    }
}
