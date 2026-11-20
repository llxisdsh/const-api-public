use crate::image_cli::{LocalAccess, model_catalog, public_media_download};
use crate::media_request::bounded_response;
use crate::protocol::stream::SseDecoder;
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use reqwest::{RequestBuilder, Response};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

const HELP: &str = "media models\nmedia video-create < request.json\nmedia video-status JOB_ID\nmedia video-save JOB_ID FILE\nmedia speech FILE < request.json\nCreate and speech accept one UTF-8 API request object on stdin. Status and save reuse the same video job.";

pub(crate) fn run(args: Vec<OsString>) -> ExitCode {
    if args.is_empty() || (args.len() == 1 && (args[0] == "--help" || args[0] == "-h")) {
        let _ = writeln!(std::io::stdout().lock(), "{HELP}");
        return ExitCode::SUCCESS;
    }
    let result = (|| -> Result<Value> {
        let access = LocalAccess::read()?;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(execute(&access, &args, std::io::stdin().lock()))
    })();
    match result {
        Ok(result) => {
            let _ = writeln!(std::io::stdout().lock(), "{result}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = writeln!(std::io::stderr().lock(), "media: {error:#}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn discovery(catalog: &Value) -> Result<Value> {
    let mut video = std::collections::BTreeSet::new();
    let mut speech = std::collections::BTreeSet::new();
    let mut speech_sse = std::collections::BTreeSet::new();
    let mut pricing = serde_json::Map::new();
    let mut native = std::collections::BTreeMap::<&str, std::collections::BTreeSet<&str>>::new();
    let mut outputs = std::collections::BTreeMap::<&str, std::collections::BTreeSet<&str>>::new();
    for (id, entry) in
        crate::image_models::visible_catalog_entries(catalog).context("invalid model list")?
    {
        let metadata = &entry["const_api"];
        for kind in crate::model_discovery::media_outputs(metadata) {
            outputs.entry(kind).or_default().insert(id);
        }
        for op in metadata["native_media_operations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if crate::media_models::is_native_operation(op) {
                native.entry(op).or_default().insert(id);
            }
        }
        let ops = crate::media_models::MediaOperations::from_entry(entry);
        if ops.empty() {
            continue;
        }
        if ops.video {
            video.insert(id);
        }
        if ops.speech {
            speech.insert(id);
            if entry
                .pointer("/const_api/speech_stream_format")
                .and_then(Value::as_str)
                == Some("sse")
            {
                speech_sse.insert(id);
            }
        }
        if let Some(price) = entry
            .pointer("/const_api/platform_media_pricing")
            .filter(|p| p.is_object())
        {
            pricing.insert(id.into(), price.clone());
        }
    }
    Ok(
        json!({"video": video, "speech": speech, "speech_sse": speech_sse, "platform_pricing": pricing,
            "native_operations": native, "declared_outputs": outputs}),
    )
}

fn json_input(input: impl Read, required: &[&str]) -> Result<Value> {
    let mut bytes = Vec::new();
    let limit = crate::MODEL_MESSAGE_MAX_BYTES as u64;
    input.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "request exceeds the gateway message limit"
    );
    let body: Value = serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .context("expected a UTF-8 JSON request object on stdin")?;
    for key in required {
        ensure!(
            body[key].as_str().is_some_and(|s| !s.trim().is_empty()),
            "missing {key} in the request"
        );
    }
    Ok(body)
}

async fn send(request: RequestBuilder) -> Result<Response> {
    let response = request
        .send()
        .await
        .context("cannot reach the local CONST API")?;
    ensure!(
        response.status().is_success(),
        "CONST returned HTTP {}. See the request log before retrying",
        response.status()
    );
    Ok(response)
}

async fn json_response(request: RequestBuilder) -> Result<Value> {
    serde_json::from_slice(&bounded_response(send(request).await?).await?)
        .context("invalid video job response")
}

fn job_url(api: &str, id: &std::ffi::OsStr) -> Result<reqwest::Url> {
    let id = id.to_str().context("invalid video job ID")?;
    ensure!(
        !id.trim().is_empty()
            && !matches!(id, "." | "..")
            && !id.contains(['/', '\\', '?', '#', '%'])
            && !id.chars().any(char::is_control),
        "expected a single video job ID"
    );
    let mut url = reqwest::Url::parse(&format!("{api}/videos"))?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid local endpoint"))?
        .push(id);
    Ok(url)
}

fn video_status<'a>(job: &'a Value, id: &std::ffi::OsStr) -> Result<&'a str> {
    for field in ["id", "request_id"] {
        if let Some(returned) = job[field].as_str().filter(|s| !s.is_empty()) {
            ensure!(
                Some(returned) == id.to_str(),
                "response belongs to a different video job"
            );
        }
    }
    let status = job["status"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("video job has no status")?;
    if matches!(status, "done" | "completed")
        && job
            .pointer("/video/respect_moderation")
            .and_then(Value::as_bool)
            == Some(false)
    {
        return Ok("failed");
    }
    Ok(status)
}

fn output_file(path: &Path) -> Result<tempfile::NamedTempFile> {
    ensure!(
        !path.try_exists()?,
        "output already exists; choose a new filename"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tempfile::Builder::new()
        .prefix("const-media-")
        .suffix(
            &path
                .extension()
                .map_or(String::new(), |e| format!(".{}", e.to_string_lossy())),
        )
        .tempfile_in(parent)
        .context("cannot write to the output directory")
}

fn save(file: tempfile::NamedTempFile, path: &Path) -> Result<Value> {
    ensure!(
        file.as_file().metadata()?.len() > 0,
        "no media bytes received"
    );
    let saved = match file.persist_noclobber(path) {
        Ok(_) => path.to_owned(),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            error.file.keep()?.1
        }
        Err(error) => return Err(error.into()),
    };
    Ok(json!({"file": saved}))
}

async fn write_body(
    mut response: Response,
    file: &mut tempfile::NamedTempFile,
    speech: bool,
) -> Result<()> {
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if speech && mime == "text/event-stream" {
        let mut decoder = SseDecoder::default();
        loop {
            let chunk = response.chunk().await?;
            let frames = match &chunk {
                Some(bytes) => decoder.push(bytes)?,
                None => decoder.finish()?,
            };
            for frame in frames {
                if frame.data == "[DONE]" {
                    bail!("speech ended without speech.audio.done");
                }
                let data: Value =
                    serde_json::from_str(&frame.data).context("invalid speech event")?;
                match data["type"].as_str().or(frame.event.as_deref()) {
                    Some("speech.audio.delta") => {
                        let audio = data["audio"]
                            .as_str()
                            .context("speech delta is missing audio")?;
                        file.write_all(&base64::engine::general_purpose::STANDARD.decode(audio)?)?;
                    }
                    Some("speech.audio.done") => return Ok(()),
                    Some("error" | "speech.audio.error" | "speech.audio.failed") => {
                        bail!("speech stream failed")
                    }
                    _ => {}
                }
            }
            ensure!(
                chunk.is_some(),
                "speech stream ended before speech.audio.done"
            );
        }
    }
    let prefix = if speech { "audio/" } else { "video/" };
    ensure!(
        mime.starts_with(prefix) || mime == "application/octet-stream",
        "unexpected media content type"
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        file.write_all(&chunk)?;
    }
    Ok(())
}

async fn execute(access: &LocalAccess, args: &[OsString], input: impl Read) -> Result<Value> {
    if let [cmd] = args
        && cmd == "models"
    {
        return discovery(&model_catalog(access).await?);
    }
    let (client, api) = access.endpoint()?;
    let authorize = |req: RequestBuilder| req.bearer_auth(&access.api_key);
    match args {
        [cmd] if cmd == "video-create" => {
            let body = json_input(input, &["model", "prompt"])?;
            let job =
                json_response(authorize(client.post(format!("{api}/videos"))).json(&body)).await?;
            let id = job["id"]
                .as_str()
                .or_else(|| job["request_id"].as_str())
                .filter(|id| !id.trim().is_empty())
                .context(
                    "video submission returned no ID; check the request log before resubmitting",
                )?;
            Ok(json!({"id": id, "status": job["status"]}))
        }
        [cmd, id] if cmd == "video-status" => {
            let job = json_response(authorize(client.get(job_url(&api, id)?))).await?;
            Ok(
                json!({"id": id.to_str(), "status": video_status(&job, id)?, "progress": job["progress"]}),
            )
        }
        [cmd, id, path] if cmd == "video-save" => {
            let path = Path::new(path);
            let mut file = output_file(path)?;
            let mut url = job_url(&api, id)?;
            let job = json_response(authorize(client.get(url.clone()))).await?;
            ensure!(
                matches!(video_status(&job, id)?, "done" | "completed"),
                "video job is not complete"
            );
            let response = match job
                .pointer("/video/url")
                .and_then(Value::as_str)
                .or_else(|| job["url"].as_str())
            {
                Some(link) => public_media_download(link).await?,
                None => {
                    url.path_segments_mut()
                        .map_err(|_| anyhow::anyhow!("invalid local endpoint"))?
                        .push("content");
                    send(authorize(client.get(url))).await?
                }
            };
            write_body(response, &mut file, false).await?;
            save(file, path)
        }
        [cmd, path] if cmd == "speech" => {
            let path = Path::new(path);
            let mut body = json_input(input, &["model", "input", "voice"])?;
            let format = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            ensure!(
                matches!(
                    format.as_str(),
                    "mp3" | "wav" | "flac" | "opus" | "aac" | "pcm"
                ),
                "choose an output file with a supported audio extension"
            );
            ensure!(
                body.get("response_format")
                    .is_none_or(|v| v.as_str() == Some(&format)),
                "response_format must match the output file extension"
            );
            body["response_format"] = format.into();
            let mut file = output_file(path)?;
            let response =
                send(authorize(client.post(format!("{api}/audio/speech"))).json(&body)).await?;
            write_body(response, &mut file, true).await?;
            save(file, path)
        }
        _ => bail!("unknown media command or arguments. See media --help"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_models::{SPEECH, VIDEO};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use warp::Filter;

    #[tokio::test]
    async fn discovers_and_saves_a_single_video_job_and_both_speech_formats() {
        let posts = Arc::new(AtomicUsize::new(0));
        let requests = posts.clone();
        let route = warp::header::exact("authorization", "Bearer fixture")
            .and(warp::method()).and(warp::path::full()).and(warp::body::bytes())
            .map(move |method, path: warp::path::FullPath, bytes: bytes::Bytes| {
                let (mime, result) = match (method, path.as_str()) {
                    (warp::http::Method::GET, "/v1/models") => ("application/json", json!({"data":[
                        {"id":"vision-chat", "const_api":{"video_output":true,"audio_output":true}},
                        {"id":"video-test", "const_api":{"media_operations":[VIDEO]}},
                        {"id":"voice-test", "const_api":{"media_operations":[SPEECH],"speech_stream_format":"sse"}}
                    ]}).to_string()),
                    (warp::http::Method::POST, "/v1/videos") => {
                        requests.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["model"], "video-test");
                        ("application/json", json!({"request_id":"job_1"}).to_string())
                    }
                    (warp::http::Method::GET, "/v1/videos/job_1") => ("application/json", json!({"status":"completed"}).to_string()),
                    (warp::http::Method::GET, "/v1/videos/blocked") => ("application/json", json!({"status":"done","request_id":"blocked","video":{"respect_moderation":false}}).to_string()),
                    (warp::http::Method::GET, "/v1/videos/wrong") => ("application/json", json!({"status":"completed","id":"someone_else"}).to_string()),
                    (warp::http::Method::GET, "/v1/videos/job_1/content") => ("video/mp4", "fixture video".into()),
                    (warp::http::Method::POST, "/v1/audio/speech") => {
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(body["response_format"], "mp3");
                        if body["stream_format"] != "sse" {
                            ("audio/mpeg", "fixture speech".into())
                        } else {
                            let mut body_text = String::from("data: {\"type\":\"speech.audio.delta\",\"audio\":\"b25l\"}\n\nevent: speech.audio.delta\ndata: {\"audio\":\"dHdv\"}\n\n");
                            if body["input"] != "cut" { body_text.push_str("data: {\"type\":\"speech.audio.done\"}\n\n"); }
                            ("text/event-stream; charset=utf-8", body_text)
                        }
                    }
                    _ => panic!("unexpected fixture route"),
                };
                warp::reply::with_header(result, "content-type", mime)
            });
        let (listen, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let server = tokio::spawn(server);
        let access = LocalAccess {
            listen,
            api_key: "fixture".into(),
        };
        let folder = tempfile::tempdir().unwrap();
        let list = execute(&access, &["models".into()], &b""[..])
            .await
            .unwrap();
        assert_eq!(list["video"], json!(["video-test"]));
        assert_eq!(list["speech"], json!(["voice-test"]));
        assert_eq!(list["speech_sse"], list["speech"]);
        let job = execute(
            &access,
            &["video-create".into()],
            &br#"{"model":"video-test","prompt":"example"}"#[..],
        )
        .await
        .unwrap();
        let id = OsString::from(job["id"].as_str().unwrap());
        let status = execute(&access, &["video-status".into(), id.clone()], &b""[..])
            .await
            .unwrap();
        assert_eq!(status["status"], "completed");
        let video = folder.path().join("clip.mp4");
        execute(
            &access,
            &["video-save".into(), id, video.as_os_str().into()],
            &b""[..],
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(video).unwrap(), b"fixture video");
        for id in ["blocked", "wrong"] {
            let status = execute(&access, &["video-status".into(), id.into()], &b""[..]).await;
            if id == "blocked" {
                assert_eq!(status.unwrap()["status"], "failed");
            } else {
                assert!(status.is_err());
            }
            let out = folder.path().join(format!("{id}.mp4"));
            let saved = execute(
                &access,
                &["video-save".into(), id.into(), out.as_os_str().into()],
                &b""[..],
            )
            .await;
            assert!(saved.is_err() && !out.exists());
        }
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        for (input, stream, expected) in [
            ("binary", "audio", "fixture speech"),
            ("stream", "sse", "onetwo"),
            ("cut", "sse", ""),
        ] {
            let file = folder.path().join(format!("{input}.mp3"));
            let body =
                json!({"model":"voice-test","input":input,"voice":"sample","stream_format":stream})
                    .to_string();
            let outcome = execute(
                &access,
                &["speech".into(), file.as_os_str().into()],
                body.as_bytes(),
            )
            .await;
            if expected.is_empty() {
                assert!(outcome.is_err() && !file.exists());
            } else {
                outcome.unwrap();
                assert_eq!(std::fs::read_to_string(file).unwrap(), expected);
            }
        }
        assert!(job_url("http://127.0.0.1/v1", "../models".as_ref()).is_err());
        server.abort();
    }
}
