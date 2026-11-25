//! Comcigan protocol, ported from leegeunhyeok/comcigan-parser (MIT).
//!
//! Every function that touches the *shape* of the site's responses lives here,
//! so that when Comcigan changes something, this is the only file to fix.

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use boa_engine::{Context, Source};
use regex::Regex;
use scraper::{Html, Node, Selector};

pub const HOST: &str = "http://컴시간학생.kr";

/// Everything learned from the one-time "frame" page.
#[derive(Debug, Clone)]
pub struct Session {
    pub base_url: String,      // scheme://host[:port]
    pub extract_code: String,  // e.g. "/36179?"
    pub sc_data: Vec<String>,  // sc_data('xxx', ..., n) arguments
    pub page_source: String,   // decoded frame page (contains the decoder JS)
}

#[derive(Debug, Clone, Default)]
pub struct Cell {
    pub subject: String,
    pub teacher: String,
}

// ---------- decoding helpers ----------

/// Decode bytes as UTF-8, falling back to EUC-KR.
pub fn decode_auto(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::EUC_KR.decode(bytes).0.into_owned(),
    }
}

pub fn decode_euc_kr(bytes: &[u8]) -> String {
    encoding_rs::EUC_KR.decode(bytes).0.into_owned()
}

/// Keyword -> "%b0%a1..." (EUC-KR bytes, lowercase hex, same as the original).
pub fn euc_kr_percent(keyword: &str) -> String {
    let (bytes, _, _) = encoding_rs::EUC_KR.encode(keyword);
    bytes.iter().map(|b| format!("%{:x}", b)).collect()
}

/// Cut everything after the last '}' (the server appends garbage).
pub fn trim_json(s: &str) -> &str {
    match s.rfind('}') {
        Some(i) => &s[..=i],
        None => s,
    }
}

// ---------- step 1: the frame page ----------

/// First request: the landing page contains a <frame src="..."> pointing at the real app.
pub fn extract_frame_src(landing_html: &str) -> Result<String> {
    let body = landing_html.to_lowercase().replace('\'', "\"");
    let frame = Regex::new(r#"<frame [^>]*src="([^"]*)"[^>]*>"#)?
        .captures(&body)
        .ok_or_else(|| anyhow!("landing page has no <frame src=...>"))?;
    Ok(frame[1].to_string())
}

/// Second request (EUC-KR page): pull out the route code and sc_data args.
pub fn parse_frame(frame_url: &str, source: String) -> Result<Session> {
    let i_ra = source.find("school_ra(sc)").with_context(|| format!("`school_ra(sc)` not found in frame page\n{}", diagnose(&source)))?;
    let i_sc = source.find("sc_data('").with_context(|| format!("`sc_data('` not found in frame page\n{}", diagnose(&source)))?;

    let ra: String = source[i_ra..].chars().take(50).collect::<String>().replacen(' ', "", 1);
    let extract_code = Regex::new(r"url:'.(.*?)'")?
        .captures(&ra)
        .context("route code (url:'...') not found after school_ra(sc)")?[1]
        .to_string();

    let sc: String = source[i_sc..].chars().take(30).collect::<String>().replacen(' ', "", 1);
    let args = Regex::new(r"\(.*?\)")?.find(&sc).context("sc_data(...) arguments not found")?;
    let sc_data: Vec<String> = args
        .as_str()
        .replace(['(', ')', '\''], "")
        .split(',')
        .map(|a| a.trim().to_owned())
        .collect();
    if sc_data.len() < 3 {
        bail!("sc_data has {} args, expected >= 3: {:?}", sc_data.len(), sc_data);
    }

    let u = url::Url::parse(frame_url)?;
    Ok(Session {
        base_url: u.origin().ascii_serialization(),
        extract_code,
        sc_data,
        page_source: source,
    })
}


/// Printed when the frame page no longer looks like the original parser expects:
/// shows size, head, external script/frame links and where the key tokens occur.
pub fn diagnose(source: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let chars: Vec<char> = source.chars().collect();
    let _ = writeln!(out, "--- 진단: frame 페이지 {}자 ---", chars.len());
    let head: String = chars.iter().take(300).collect();
    let _ = writeln!(out, "[처음 300자]\n{}\n", head.replace('\r', ""));

    if let Ok(re) = Regex::new(r#"(?i)<(?:i?frame|script)[^>]*\b(?:src|href)\s*=\s*["']?([^"' >]+)"#) {
        for c in re.captures_iter(source).take(15) {
            let _ = writeln!(out, "[링크] {}", &c[1]);
        }
    }
    for token in ["school_ra", "sc_data", "url:", "function 자료", "자료", "base64", "ajax", "fetch(", "XMLHttp"] {
        let hits: Vec<usize> = source.match_indices(token).map(|(i, _)| i).take(3).collect();
        let _ = writeln!(out, "[토큰 '{token}'] {}곳", source.matches(token).count());
        for i in hits {
            let start = source[..i].char_indices().rev().nth(40).map(|(b, _)| b).unwrap_or(0);
            let end = source[i..].char_indices().nth(120).map(|(b, _)| i + b).unwrap_or(source.len());
            let _ = writeln!(out, "    …{}…", source[start..end].replace(['\r', '\n'], " "));
        }
    }
    out
}

// ---------- step 2: school search ----------

pub fn search_url(s: &Session, keyword: &str) -> String {
    format!("{}{}{}", s.base_url, s.extract_code, euc_kr_percent(keyword))
}

#[derive(Debug, Clone)]
pub struct School {
    pub region: String,
    pub name: String,
    pub code: i64,
}

pub fn parse_search(body: &str) -> Result<Vec<School>> {
    let v: serde_json::Value = serde_json::from_str(trim_json(body)).context("search JSON")?;
    let rows = v["학교검색"].as_array().context("`학교검색` missing in search response")?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(School {
                region: r.get(1)?.as_str()?.to_owned(),
                name: r.get(2)?.as_str()?.to_owned(),
                code: r.get(3)?.as_i64()?,
            })
        })
        .collect())
}

// ---------- step 3: timetable ----------

pub fn timetable_url(s: &Session, school_code: i64) -> String {
    let payload = format!("{}{}_0_{}", s.sc_data[0], school_code, s.sc_data[2]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
    let route = s.extract_code.split('?').next().unwrap_or("");
    format!("{}{}?{}", s.base_url, route, b64)
}

/// The decoder is JavaScript embedded in the site's page: collect all of it.
pub fn extract_scripts(page: &str) -> Result<String> {
    let start = Regex::new(r"(?mi)<script language(.*?)>")?
        .find(page)
        .context("no <script language...> tag in frame page")?
        .as_str()
        .to_owned();
    let re = Regex::new(&format!("(?si){}(.*?)</script>", regex::escape(&start)))?;
    let script: String = re.captures_iter(page).map(|c| c[1].to_string()).collect::<Vec<_>>().join("\n");
    if script.trim().is_empty() {
        bail!("decoder script is empty");
    }
    Ok(script)
}

/// Name of the page's `function 자료NN(...)` decoder.
pub fn decoder_name(script: &str) -> Result<String> {
    let m = Regex::new(r"function\s+(자료[^(\s]*)")?
        .captures(script)
        .context("`function 자료...` decoder not found in page script")?;
    Ok(m[1].to_string())
}

/// Run the site's own decoder in an embedded JS engine; returns an HTML table.
pub fn render_class_html(script: &str, func: &str, json: &str, grade: u32, class: u32) -> Result<String> {
    let program = format!("{script}\n\n{func}({json},{grade},{class})");
    let mut ctx = Context::default();
    let value = ctx
        .eval(Source::from_bytes(program.as_bytes()))
        .map_err(|e| anyhow!("JS decoder failed: {e}"))?;
    let s = value
        .to_string(&mut ctx)
        .map_err(|e| anyhow!("JS result not a string: {e}"))?;
    Ok(s.to_std_string_escaped())
}

/// HTML table -> cells[weekday 0..5][period 0..], same layout as the original parser.
pub fn parse_table(html: &str) -> Vec<Vec<Cell>> {
    let doc = Html::parse_fragment(html);
    let tr = Selector::parse("tr").unwrap();
    let td = Selector::parse("td").unwrap();
    let mut days: Vec<Vec<Cell>> = vec![Vec::new(); 5];

    for (time_idx, row) in doc.select(&tr).enumerate() {
        if time_idx <= 1 {
            continue; // two header rows
        }
        for (wd_idx, cell) in row.select(&td).enumerate() {
            if wd_idx == 0 || wd_idx == 6 {
                continue; // period label column / trailing column
            }
            let text_of = |n: Option<ego_tree::NodeRef<Node>>| -> String {
                n.map(|n| match n.value() {
                    Node::Text(t) => t.text.to_string(),
                    _ => scraper::ElementRef::wrap(n)
                        .map(|e| e.text().collect::<String>())
                        .unwrap_or_default(),
                })
                .unwrap_or_default()
                .trim()
                .to_owned()
            };
            let mut kids = cell.children();
            let first = kids.next();
            let last = cell.children().last();
            days[wd_idx - 1].push(Cell { subject: text_of(first), teacher: text_of(last) });
        }
    }
    days
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_matches_euc_kr() {
        // '가' = B0 A1 in EUC-KR
        assert_eq!(euc_kr_percent("가"), "%b0%a1");
    }

    #[test]
    fn trims_trailing_garbage() {
        assert_eq!(trim_json("{\"a\":1}\u{0}\u{0}xx"), "{\"a\":1}");
    }

    #[test]
    fn frame_and_session() {
        let landing = "<HTML><FRAME src='http://comci.example:4082/st' name=a></HTML>";
        assert_eq!(extract_frame_src(landing).unwrap(), "http://comci.example:4082/st");

        let page = "x school_ra(sc) { $.ajax({ url:'./36179?' }) } y sc_data('73629_', x, 2) z".to_string();
        let s = parse_frame("http://comci.example:4082/st", page).unwrap();
        assert_eq!(s.base_url, "http://comci.example:4082");
        assert_eq!(s.extract_code, "/36179?");
        assert_eq!(s.sc_data[0], "73629_");
        assert_eq!(s.sc_data[2], "2");
        assert!(search_url(&s, "가").ends_with("/36179?%b0%a1"));
        // payload: "73629_" + code + "_0_" + "2"
        let t = timetable_url(&s, 13209);
        assert!(t.starts_with("http://comci.example:4082/36179?"));
    }

    #[test]
    fn search_response() {
        let body = r#"{"학교검색":[[1,"제주","대정고등학교",12345],[2,"서울","대정고",999]]}garbage"#;
        let v = parse_search(body).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].region.as_str(), v[0].name.as_str(), v[0].code), ("제주", "대정고등학교", 12345));
    }

    /// Stand-in for the site's decoder: proves script extraction + JS eval + table parsing chain.
    #[test]
    fn js_decoder_roundtrip() {
        let page = "<html><script language='javascript'>\nfunction 자료42(d,g,c){\
            var h='<table><tr><td>a</td></tr><tr><td>b</td></tr>';\
            for(var p=0;p<2;p++){h+='<tr><td>'+(p+1)+'</td>';\
              for(var w=0;w<5;w++){h+='<td>'+d['x'][w]+'<br>T'+g+c+'</td>';}\
              h+='<td></td></tr>';}\
            return h+'</table>';}\n</script></html>";
        let script = extract_scripts(page).unwrap();
        let name = decoder_name(&script).unwrap();
        assert_eq!(name, "자료42");
        let html = render_class_html(&script, &name, r#"{"x":["국어","수학","영어","과학","체육"]}"#, 1, 3).unwrap();
        let days = parse_table(&html);
        assert_eq!(days[0].len(), 2);
        assert_eq!(days[1][0].subject, "수학");
        assert_eq!(days[4][1].teacher, "T13");
    }
}
