//! P0 spike: does the Comcigan pipeline still work, end to end, in Rust?
//!
//!   cargo run --release -- 대정고등학교 [학년] [반] [--code N]
//!
//! Raw responses are saved to ./dump/ so a failure can be turned into a test fixture.

mod comcigan;

use anyhow::{Context as _, Result, bail};
use comcigan::*;
use std::{fs, time::Duration};

fn get(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    eprintln!("GET {url}");
    let resp = client.get(url).send().with_context(|| format!("request failed: {url}"))?;
    let status = resp.status();
    let bytes = resp.bytes()?.to_vec();
    if !status.is_success() {
        bail!("HTTP {status} for {url}");
    }
    Ok(bytes)
}

fn dump(name: &str, bytes: &[u8]) {
    let _ = fs::create_dir_all("dump");
    let _ = fs::write(format!("dump/{name}"), bytes);
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut forced_code: Option<i64> = None;
    if let Some(i) = args.iter().position(|a| a == "--code") {
        forced_code = Some(args.get(i + 1).context("--code needs a value")?.parse()?);
        args.drain(i..=i + 1);
    }
    let keyword = args.first().cloned().unwrap_or_else(|| "대정고등학교".into());
    let grade: u32 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(1);
    let class: u32 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1);

    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build()?;

    // 1) landing page -> frame url
    let landing = get(&client, HOST)?;
    dump("1_landing.html", &landing);
    let frame_url = extract_frame_src(&decode_auto(&landing))?;
    println!("[1] frame: {frame_url}");

    // 2) frame page (EUC-KR) -> route code + sc_data
    let frame = get(&client, &frame_url)?;
    dump("2_frame.html", &frame);
    let session = parse_frame(&frame_url, decode_euc_kr(&frame))?;
    println!("[2] base={} route={} sc_data={:?}", session.base_url, session.extract_code, session.sc_data);

    // 3) school search
    let code = match forced_code {
        Some(c) => c,
        None => {
            let body = get(&client, &search_url(&session, &keyword))?;
            dump("3_search.json", &body);
            let schools = parse_search(&decode_auto(&body))?;
            if schools.is_empty() {
                bail!("검색된 학교가 없습니다: {keyword}");
            }
            println!("[3] search results for '{keyword}':");
            for s in &schools {
                println!("    code={:<8} {} {}", s.code, s.region, s.name);
            }
            // exact name match first, else the first hit
            let pick = schools.iter().find(|s| s.name == keyword).unwrap_or(&schools[0]);
            println!("    -> using {} {} ({})", pick.region, pick.name, pick.code);
            pick.code
        }
    };

    // 4) timetable JSON
    let body = get(&client, &timetable_url(&session, code))?;
    dump("4_timetable.json", &body);
    let json_text = decode_auto(&body);
    let json = trim_json(&json_text);
    let parsed: serde_json::Value = serde_json::from_str(json).context("timetable JSON")?;
    println!("[4] timetable JSON ok, top-level keys: {}", parsed.as_object().map(|o| o.len()).unwrap_or(0));

    // 5) run the page's own decoder in an embedded JS engine
    let script = extract_scripts(&session.page_source)?;
    let func = decoder_name(&script)?;
    println!("[5] decoder = {func} ({} bytes of script)", script.len());
    let html = render_class_html(&script, &func, json, grade, class)?;
    dump("5_class.html", html.as_bytes());

    // 6) table -> cells
    let days = parse_table(&html);
    let periods = days.iter().map(Vec::len).max().unwrap_or(0);
    println!("\n== {grade}학년 {class}반 ==");
    if let Some(times) = parsed["일과시간"].as_array() {
        println!("일과시간: {}", times.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join("  "));
    }
    print!("{:>3}", "");
    for d in ["월", "화", "수", "목", "금"] {
        print!(" | {:<14}", d);
    }
    println!();
    for p in 0..periods {
        print!("{:>3}", p + 1);
        for d in &days {
            let c = d.get(p).cloned().unwrap_or_default();
            let txt = if c.subject.is_empty() { "-".to_string() } else { format!("{}({})", c.subject, c.teacher) };
            print!(" | {:<14}", txt);
        }
        println!();
    }
    println!("\n원본 응답은 ./dump/ 에 저장되었습니다.");
    Ok(())
}
