use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use std::{
    env, fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

struct Api {
    base: String,
    token: String,
    http: Client,
}
impl Api {
    fn new() -> Result<Self, String> {
        Ok(Self {
            base: env::var("NARADA_MEDIA_API_URL")
                .map_err(|_| "NARADA_MEDIA_API_URL is required")?
                .trim_end_matches('/')
                .into(),
            token: env::var("NARADA_MEDIA_API_TOKEN")
                .map_err(|_| "NARADA_MEDIA_API_TOKEN is required")?,
            http: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(err)?,
        })
    }
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let url = format!("{}{}", self.base, path);
        let mut r = match method {
            "GET" => self.http.get(url),
            "POST" => self.http.post(url),
            _ => return Err("unsupported method".into()),
        }
        .bearer_auth(&self.token);
        if let Some(v) = body {
            r = r.json(&v);
        }
        let response = r.send().map_err(err)?;
        let status = response.status();
        let text = response.text().map_err(err)?;
        if !status.is_success() {
            return Err(format!("API {status}: {text}"));
        }
        serde_json::from_str(&text).map_err(err)
    }
    fn submit(&self, operation: &str, mut args: Map<String, Value>) -> Result<Value, String> {
        args.insert("operation".into(), operation.into());
        self.request("POST", "/v1/jobs", Some(Value::Object(args)))
    }
    fn wait(&self, id: &str) -> Result<Value, String> {
        loop {
            let v = self.request("GET", &format!("/v1/jobs/{id}"), None)?;
            match v["status"].as_str() {
                Some("succeeded" | "failed" | "canceled") => return Ok(v),
                _ => thread::sleep(Duration::from_secs(2)),
            }
        }
    }
    fn fetch(&self, job: &str, artifact: &str, output: &Path) -> Result<Value, String> {
        let signed = self.request(
            "POST",
            &format!("/v1/jobs/{job}/artifacts/{artifact}/url"),
            None,
        )?;
        let url = signed["url"].as_str().ok_or("missing signed URL")?;
        let bytes = self
            .http
            .get(url)
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .bytes()
            .map_err(err)?;
        fs::create_dir_all(output).map_err(err)?;
        let name = signed["filename"].as_str().unwrap_or(artifact);
        let target = safe_output(output, name)?;
        fs::write(&target, &bytes).map_err(err)?;
        Ok(json!({"path":target,"bytes":bytes.len()}))
    }
}
fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}
fn safe_output(root: &Path, name: &str) -> Result<PathBuf, String> {
    let name = Path::new(name).file_name().ok_or("invalid filename")?;
    Ok(root.join(name))
}
fn get_str<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{k} is required"))
}

fn tool_call(api: &Api, name: &str, args: Value) -> Result<Value, String> {
    let mut map = args.as_object().cloned().unwrap_or_default();
    match name {
        "media_capabilities" => api.request("GET", "/v1/capabilities", None),
        "youtube_inspect" | "x_inspect" => {
            let platform = if name.starts_with("youtube") {
                "youtube"
            } else {
                "x"
            };
            api.request(
                "POST",
                &format!("/v1/{platform}/inspect"),
                Some(json!({"url":get_str(&Value::Object(map),"url")?})),
            )
        }
        "media_job_get" => api.request(
            "GET",
            &format!("/v1/jobs/{}", get_str(&Value::Object(map), "job_id")?),
            None,
        ),
        "media_job_cancel" => api.request(
            "POST",
            &format!(
                "/v1/jobs/{}/cancel",
                get_str(&Value::Object(map), "job_id")?
            ),
            None,
        ),
        "media_artifact_fetch" => {
            let v = Value::Object(map);
            api.fetch(
                get_str(&v, "job_id")?,
                get_str(&v, "artifact_id")?,
                Path::new(v.get("output").and_then(Value::as_str).unwrap_or(".")),
            )
        }
        _ => {
            let operation = match name {
                "youtube_download_video" => "youtube.video.download",
                "youtube_download_audio" => "youtube.audio.download",
                "youtube_clip" => "youtube.clip",
                "youtube_transcript" => "youtube.transcript",
                "youtube_thumbnail" => "youtube.thumbnail",
                "x_download_media" => "x.media.download",
                "x_clip_video" => "x.video.clip",
                _ => return Err(format!("unknown tool: {name}")),
            };
            let wait = map
                .remove("wait")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            map.remove("output");
            let job = api.submit(operation, map)?;
            if wait {
                api.wait(get_str(&job, "job_id")?)
            } else {
                Ok(job)
            }
        }
    }
}
fn tools() -> Value {
    let specs = [
        ("media_capabilities", false),
        ("youtube_inspect", true),
        ("youtube_download_video", true),
        ("youtube_download_audio", true),
        ("youtube_clip", true),
        ("youtube_transcript", true),
        ("youtube_thumbnail", true),
        ("x_inspect", true),
        ("x_download_media", true),
        ("x_clip_video", true),
        ("media_job_get", false),
        ("media_job_cancel", false),
        ("media_artifact_fetch", false),
    ];
    Value::Array(specs.into_iter().map(|(name,url)|json!({"name":name,"description":format!("Narada media operation: {name}"),"inputSchema":{"type":"object","properties":{"url":{"type":"string"},"job_id":{"type":"string"},"artifact_id":{"type":"string"},"start_seconds":{"type":"number"},"end_seconds":{"type":"number"},"duration_seconds":{"type":"number"},"quality":{"type":"string"},"audio_format":{"type":"string"},"transcript_format":{"type":"string"},"language":{"type":"string"},"wait":{"type":"boolean"},"output":{"type":"string"}},"required":if url{json!(["url"])}else{json!([])}}})).collect())
}

fn mcp() -> Result<(), String> {
    let api = Api::new()?;
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(err)? == 0 {
            break;
        }
        let body = if line.to_ascii_lowercase().starts_with("content-length:") {
            let n: usize = line
                .split(':')
                .nth(1)
                .ok_or("bad header")?
                .trim()
                .parse()
                .map_err(err)?;
            loop {
                line.clear();
                reader.read_line(&mut line).map_err(err)?;
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            let mut b = vec![0; n];
            reader.read_exact(&mut b).map_err(err)?;
            String::from_utf8(b).map_err(err)?
        } else {
            line
        };
        let req: Value = serde_json::from_str(body.trim()).map_err(err)?;
        if req.get("id").is_none() {
            continue;
        }
        let id = req["id"].clone();
        let result=match req["method"].as_str(){Some("initialize")=>Ok(json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"narada-media","version":"0.1.0"}})),Some("tools/list")=>Ok(json!({"tools":tools()})),Some("tools/call")=>tool_call(&api,get_str(&req["params"],"name")?,req["params"].get("arguments").cloned().unwrap_or(json!({}))).map(|v|json!({"content":[{"type":"text","text":serde_json::to_string_pretty(&v).unwrap()}],"structuredContent":v})),_=>Err("method not found".into())};
        let response = match result {
            Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e}}),
        };
        let bytes = serde_json::to_vec(&response).map_err(err)?;
        write!(stdout, "Content-Length: {}\r\n\r\n", bytes.len()).map_err(err)?;
        stdout.write_all(&bytes).map_err(err)?;
        stdout.flush().map_err(err)?
    }
    Ok(())
}

fn cli(args: &[String]) -> Result<Value, String> {
    let api = Api::new()?;
    if args.len() < 2 {
        return Err("usage: narada-media <youtube|x|job|capabilities> ...".into());
    }
    if args[1] == "capabilities" {
        return api.request("GET", "/v1/capabilities", None);
    }
    if args[1] == "job" {
        let id = args.get(3).ok_or("job id required")?;
        return if args.get(2).map(String::as_str) == Some("cancel") {
            api.request("POST", &format!("/v1/jobs/{id}/cancel"), None)
        } else {
            api.request("GET", &format!("/v1/jobs/{id}"), None)
        };
    }
    let platform = &args[1];
    let action = args.get(2).ok_or("action required")?;
    let url = args.get(3).ok_or("url required")?;
    if action == "inspect" {
        return api.request(
            "POST",
            &format!("/v1/{platform}/inspect"),
            Some(json!({"url":url})),
        );
    }
    let operation = match (platform.as_str(), action.as_str()) {
        ("youtube", "video") => "youtube.video.download",
        ("youtube", "audio") => "youtube.audio.download",
        ("youtube", "clip") => "youtube.clip",
        ("youtube", "transcript") => "youtube.transcript",
        ("youtube", "thumbnail") => "youtube.thumbnail",
        ("x", "download") => "x.media.download",
        ("x", "clip") => "x.video.clip",
        _ => return Err("unknown platform/action".into()),
    };
    let mut m = Map::new();
    m.insert("url".into(), url.clone().into());
    let mut wait = false;
    let mut output: Option<PathBuf> = None;
    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--wait" => wait = true,
            "--output" => {
                i += 1;
                output = Some(PathBuf::from(args.get(i).ok_or("missing value")?));
                wait = true;
            }
            "--start" | "--end" | "--duration" => {
                let key = match args[i].as_str() {
                    "--start" => "start_seconds",
                    "--end" => "end_seconds",
                    _ => "duration_seconds",
                };
                i += 1;
                m.insert(
                    key.into(),
                    args.get(i)
                        .ok_or("missing value")?
                        .parse::<f64>()
                        .map_err(err)?
                        .into(),
                );
            }
            "--quality" | "--format" | "--language" => {
                let key = match args[i].as_str() {
                    "--quality" => "quality",
                    "--language" => "language",
                    _ => {
                        if action == "audio" {
                            "audio_format"
                        } else {
                            "transcript_format"
                        }
                    }
                };
                i += 1;
                m.insert(
                    key.into(),
                    args.get(i).ok_or("missing value")?.clone().into(),
                );
            }
            x => return Err(format!("unknown option: {x}")),
        }
        i += 1
    }
    let job = api.submit(operation, m)?;
    if wait {
        let completed = api.wait(get_str(&job, "job_id")?)?;
        if let Some(directory) = output {
            let id = get_str(&completed, "job_id")?;
            let downloads = completed["artifacts"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|artifact| api.fetch(id, get_str(artifact, "artifact_id")?, &directory))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({"job": completed, "downloads": downloads}))
        } else {
            Ok(completed)
        }
    } else {
        Ok(job)
    }
}
fn main() {
    let args: Vec<String> = env::args().collect();
    let result = if args.get(1).map(String::as_str) == Some("mcp") {
        mcp().map(|_| json!(null))
    } else {
        cli(&args)
    };
    match result {
        Ok(v) => {
            if !v.is_null() {
                println!("{}", serde_json::to_string_pretty(&v).unwrap())
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_strips_directories() {
        assert_eq!(
            safe_output(Path::new("out"), "../evil.mp4").unwrap(),
            Path::new("out").join("evil.mp4")
        );
    }
    #[test]
    fn catalog_has_expected_tools() {
        assert!(tools()
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["name"] == "youtube_clip"));
    }
}
