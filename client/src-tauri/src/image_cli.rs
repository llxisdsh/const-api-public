use crate::media_request::bounded_response;
use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use reqwest::{Client, multipart};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::ExitCode,
};

const HELP: &str = concat!(
    env!("CARGO_PKG_NAME"),
    " image models\nList available Images API models by operation and marketplace reference rates when known.\n",
    "image --model MODEL --prompt TEXT --out FILE [--input FILE] [--size SIZE] [--quality QUALITY]\nGenerate an image, or edit an existing image with --input. Size and quality values depend on the chosen model. The command prints the saved file path. PNG, JPEG and WebP are supported."
);

struct Command {
    model: String,
    prompt: String,
    output: PathBuf,
    input: Option<PathBuf>,
    size: Option<String>,
    quality: Option<String>,
}

impl Command {
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self> {
        let (mut model, mut prompt, mut output, mut input) = (None, None, None, None);
        let (mut size, mut quality) = (None, None);
        while let Some(flag) = args.next() {
            let value = args.next().context("missing option value")?;
            let text = || {
                value
                    .to_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_owned)
                    .with_context(|| format!("{} requires non-empty text", flag.to_string_lossy()))
            };
            match flag.to_str() {
                Some("--model") if model.is_none() => model = Some(text()?),
                Some("--prompt") if prompt.is_none() => prompt = Some(text()?),
                Some("--out") if output.is_none() => output = Some(PathBuf::from(value)),
                Some("--input") if input.is_none() => input = Some(PathBuf::from(value)),
                Some("--size") if size.is_none() => size = Some(text()?),
                Some("--quality") if quality.is_none() => quality = Some(text()?),
                _ => bail!("unknown or repeated option; see image --help"),
            }
        }
        Ok(Self {
            model: model.context("--model is required")?,
            prompt: prompt.context("--prompt is required")?,
            output: output.context("--out is required")?,
            input,
            size,
            quality,
        })
    }
}

// Read just the two local access fields. A command must never start a second
// desktop instance, migrate a profile, or send its key to an external host.
#[derive(serde::Deserialize)]
pub(super) struct LocalAccess {
    pub(super) listen: SocketAddr,
    pub(super) api_key: String,
}

impl LocalAccess {
    pub(super) fn read() -> Result<Self> {
        Self::read_from(&crate::config::default_config_path())
    }

    pub(super) fn read_from(path: &Path) -> Result<Self> {
        let raw = std::fs::read(path).context("open CONST and enable local API access first")?;
        serde_json::from_slice(&raw).context("invalid CONST access configuration")
    }

    pub(super) fn endpoint(&self) -> Result<(Client, String)> {
        ensure!(!self.api_key.trim().is_empty(), "local API key is missing");
        let ip = match self.listen.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            ip => ip,
        };
        if !ip.is_loopback() {
            ensure!(
                if_addrs::get_if_addrs()?
                    .iter()
                    .any(|interface| interface.ip() == ip),
                "the configured listener address is not assigned to this device"
            );
        }
        let client = Client::builder()
            .no_proxy()
            .local_address((!ip.is_loopback()).then_some(ip))
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok((
            client,
            format!("http://{}/v1", SocketAddr::new(ip, self.listen.port())),
        ))
    }
}

#[derive(Default, serde::Serialize)]
struct AvailableModels {
    generate: Vec<String>,
    edit: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    platform_pricing: BTreeMap<String, serde_json::Value>,
}

pub(super) async fn model_catalog(access: &LocalAccess) -> Result<serde_json::Value> {
    let (client, root) = access.endpoint()?;
    let response = client
        .get(format!("{root}/models"))
        .header(crate::model_discovery::CATALOG_HEADER, "compact-v1")
        .bearer_auth(&access.api_key)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .context("cannot read the local model list")?
        .error_for_status()?;
    serde_json::from_slice(&bounded_response(response).await?).context("invalid local model list")
}

async fn available_models(access: &LocalAccess) -> Result<AvailableModels> {
    let catalog = model_catalog(access).await?;
    let mut result = AvailableModels::default();
    for model in
        crate::image_models::catalog_models(&catalog).context("model list has no data array")?
    {
        if model.operations.generate {
            result.generate.push(model.id.into());
        }
        if model.operations.edit {
            result.edit.push(model.id.into());
        }
        if let Some(pricing) = model.platform_pricing {
            result
                .platform_pricing
                .insert(model.id.into(), pricing.clone());
        }
    }
    result.generate.sort();
    result.generate.dedup();
    result.edit.sort();
    result.edit.dedup();
    Ok(result)
}

fn media_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

async fn execute(command: Command, access: LocalAccess) -> Result<()> {
    let (client, api) = access.endpoint()?;
    let extension = command
        .output
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let extension = if extension == "jpeg" {
        "jpg"
    } else {
        extension.as_str()
    };
    ensure!(
        matches!(extension, "png" | "jpg" | "webp"),
        "--out must use .png, .jpg or .webp"
    );
    let parent = command
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Check the destination before making a billable call. Persist atomically and
    // without replacing an existing subsequent-turn input or another process's output.
    ensure!(
        !command.output.try_exists()?,
        "output already exists; choose a new filename"
    );
    let mut file =
        tempfile::NamedTempFile::new_in(parent).context("cannot write to the output directory")?;
    let root = format!("{api}/images");
    let output_format = match extension {
        "jpg" => "jpeg",
        "webp" => "webp",
        _ => "png",
    };
    let options = [
        ("size", command.size),
        ("quality", command.quality),
        (
            "output_format",
            (extension != "png").then(|| output_format.to_owned()),
        ),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| (key, value)));
    let request = if let Some(input) = command.input {
        let metadata = std::fs::metadata(&input)?;
        ensure!(metadata.is_file(), "--input must be a regular image file");
        let limit = crate::MODEL_MESSAGE_MAX_BYTES;
        ensure!(
            metadata.len() <= limit as u64,
            "input exceeds the gateway message limit"
        );
        // The file may grow after metadata was read. Bound the actual read too.
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        std::fs::File::open(input)?
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= limit,
            "input exceeds the gateway message limit"
        );
        let (mime, ext) = media_type(&bytes).context("unsupported input image")?;
        let part = multipart::Part::bytes(bytes)
            .file_name(format!("input.{ext}"))
            .mime_str(mime)?;
        let mut form = multipart::Form::new()
            .text("model", command.model)
            .text("prompt", command.prompt)
            .part("image", part);
        for (key, value) in options {
            form = form.text(key, value);
        }
        client.post(format!("{root}/edits")).multipart(form)
    } else {
        let mut body = serde_json::json!({"model":command.model, "prompt":command.prompt});
        for (key, value) in options {
            body[key] = value.into();
        }
        client.post(format!("{root}/generations")).json(&body)
    };
    let response = request
        .header(crate::supplier::IMAGE_RESPONSE_FORMAT_HEADER, "b64_json")
        .bearer_auth(access.api_key)
        .send()
        .await
        .context("cannot reach local CONST API")?;
    ensure!(
        response.status().is_success(),
        "CONST returned HTTP {}; see the local request log for details",
        response.status()
    );
    let raw = bounded_response(response).await?;
    #[derive(serde::Deserialize)]
    struct Images<'a> {
        #[serde(borrow)]
        data: Vec<Image<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Image<'a> {
        #[serde(borrow)]
        b64_json: Option<std::borrow::Cow<'a, str>>,
        #[serde(borrow)]
        url: Option<std::borrow::Cow<'a, str>>,
    }
    let images: Images = serde_json::from_slice(&raw).context("invalid Images API response")?;
    let image = images
        .data
        .first()
        .context("the Images API returned no result")?;
    let bytes = if let Some(encoded) = image.b64_json.as_deref().filter(|s| !s.is_empty()) {
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("invalid image encoding")?
    } else {
        let url = image
            .url
            .as_deref()
            .context("the Images API returned neither image bytes nor a URL")?;
        download_result(url)
            .await
            .context("image generated, but its download failed; do not repeat generation")?
    };
    let (_, actual) =
        media_type(&bytes).context("the API did not return a supported image file")?;
    let output = if actual == extension {
        command.output.clone()
    } else {
        command.output.with_extension(actual)
    };
    file.write_all(&bytes)?;
    let saved = match file.persist_noclobber(&output) {
        Ok(_) => output,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            // A provider can choose its own codec or a concurrent writer can
            // claim the name. Keep the already-generated result without loss.
            let mut unique = tempfile::Builder::new()
                .prefix("const-image-")
                .suffix(&format!(".{actual}"))
                .tempfile_in(parent)?;
            unique.write_all(&bytes)?;
            unique.keep()?.1
        }
        Err(error) => return Err(error.into()),
    };
    // The image is already saved. A closed output pipe must not turn that
    // successful operation into a failure that an agent might retry.
    let _ = writeln!(std::io::stdout().lock(), "{}", saved.display());
    Ok(())
}

// URLs in a supplier response are untrusted. Resolve once, reject special-use
// networks and local interfaces, then pin those same addresses for TLS.
fn public_image_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a != 0
                && a < 224
                && !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && !(a == 100 && (64..128).contains(&b))
                && !(a == 192 && ((b == 0 && c == 0) || (b == 88 && c == 99)))
                && !(a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            let [first, second, ..] = ip.segments();
            (0x2000..0x4000).contains(&first)
                && first != 0x2002
                && !(first == 0x2001 && (second < 0x200 || second == 0xdb8))
                && !(first == 0x3fff && second < 0x1000)
        }
    }
}

async fn download_result(raw_url: &str) -> Result<Vec<u8>> {
    bounded_response(public_media_download(raw_url).await?).await
}

pub(super) async fn public_media_download(raw_url: &str) -> Result<reqwest::Response> {
    let url = reqwest::Url::parse(raw_url).context("invalid media download URL")?;
    ensure!(
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443),
        "expected a public HTTPS media URL"
    );
    let host = url.domain().context("expected a public media hostname")?;
    let addresses: Vec<_> = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::lookup_host((host, 443)),
    )
    .await??
    .collect();
    let local = if_addrs::get_if_addrs()?;
    ensure!(
        !addresses.is_empty()
            && addresses
                .iter()
                .all(|a| public_image_address(a.ip()) && !local.iter().any(|i| i.ip() == a.ip())),
        "media URL resolves outside the public network"
    );
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, &addresses)
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    ensure!(
        response.status().is_success(),
        "media download returned HTTP {}",
        response.status()
    );
    Ok(response)
}

#[cfg(any(target_os = "linux", test))]
fn appimage_launcher(executable: PathBuf, bundle: Option<(OsString, OsString)>) -> PathBuf {
    if let Some((directory, image)) = bundle {
        let directory = Path::new(&directory);
        let image = PathBuf::from(image);
        // A tool can itself be launched from another application's AppImage.
        if directory.is_absolute()
            && executable.starts_with(directory.join("usr/bin"))
            && image.is_absolute()
        {
            return image;
        }
    }
    executable
}

fn invocation(models: Option<AvailableModels>) -> Result<serde_json::Value, String> {
    let executable = std::env::current_exe().map_err(|err| err.to_string())?;
    #[cfg(target_os = "linux")]
    let executable = appimage_launcher(
        executable,
        std::env::var_os("APPDIR").zip(std::env::var_os("APPIMAGE")),
    );
    let mut invocation = serde_json::json!({
        "executable": executable.to_str().ok_or("executable path is not valid UTF-8")?,
        "models": ["image", "models"],
        "generate": ["image", "--model", "MODEL", "--prompt", "PROMPT", "--out", "cover.png"],
        "edit": ["image", "--model", "MODEL", "--prompt", "EDIT_PROMPT", "--input", "cover.png", "--out", "cover-blue.png"],
    });
    if let Some(models) = models {
        if let [model] = models.generate.as_slice() {
            invocation["generate"][2] = model.as_str().into();
        }
        if let [model] = models.edit.as_slice() {
            invocation["edit"][2] = model.as_str().into();
        }
        invocation["available_models"] = serde_json::json!(models);
    }
    if cfg!(debug_assertions) {
        // The calling tool may have started outside the desktop's dev shell.
        // Bind only its child process to the profile that produced these instructions.
        let path = crate::config::default_config_path();
        invocation["env"] = serde_json::json!({
            "CONST_API_CLIENT_CONFIG_PATH": path.to_str().ok_or("configuration path is not valid UTF-8")?,
        });
    }
    Ok(invocation)
}

#[tauri::command]
pub(crate) async fn image_command_usage() -> Result<String, String> {
    let models = match LocalAccess::read() {
        Ok(access) => available_models(&access).await.ok(),
        Err(_) => None,
    };
    invocation(models).map(|value| value.to_string())
}

/// Installed instructions discover the current operation catalog at use time.
pub(crate) fn image_skill_invocation() -> Result<String, String> {
    let mut call = invocation(None)?;
    call["media_models"] = serde_json::json!(["media", "models"]);
    call["video_create"] = serde_json::json!(["media", "video-create"]);
    call["video_status"] = serde_json::json!(["media", "video-status", "JOB_ID"]);
    call["video_save"] = serde_json::json!(["media", "video-save", "JOB_ID", "clip.mp4"]);
    call["speech"] = serde_json::json!(["media", "speech", "speech.mp3"]);
    Ok(call.to_string())
}

pub(crate) fn run(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut args = args.peekable();
    if args.peek().is_none_or(|a| a == "--help" || a == "-h") {
        let _ = writeln!(std::io::stdout().lock(), "{HELP}");
        if let Ok(usage) = invocation(None) {
            let _ = writeln!(std::io::stdout().lock(), "{usage}");
        }
        return ExitCode::SUCCESS;
    }
    let result = (|| -> Result<()> {
        let models = args.next_if(|arg| arg == "models").is_some();
        let command = if models {
            ensure!(args.next().is_none(), "image models takes no options");
            None
        } else {
            Some(Command::parse(args)?)
        };
        let access = LocalAccess::read()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        match command {
            Some(command) => runtime.block_on(execute(command, access)),
            None => {
                let models = runtime.block_on(available_models(&access))?;
                writeln!(
                    std::io::stdout().lock(),
                    "{}",
                    serde_json::to_string(&models)?
                )?;
                Ok(())
            }
        }
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr().lock(), "image: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use warp::Filter;

    #[tokio::test]
    async fn models_lists_only_advertised_operations_and_copies_unique_choices() {
        let pricing = serde_json::json!({
            "currency":"USD","price_version_id":"fixture",
            "base_rates":[{"meter_kind":"image_output_unit","unit":"per_image","price_usd":0.08}],
            "offer_ratios":{(crate::image_models::GENERATE):{"min_ratio":0,"max_ratio":1.5}}
        });
        let advertised = pricing.clone();
        let route = warp::get().and(warp::path!("v1" / "models"))
            .and(warp::header::exact("authorization", "Bearer fixture"))
            .and(warp::header::exact(crate::model_discovery::CATALOG_HEADER, "compact-v1"))
            .map(move || warp::reply::json(&serde_json::json!({"data":[
                {"id":"dialogue","const_api":{"image_output":true}},
                {"id":"legacy"},{"id":"generate-only","const_api":{"image_operations":[crate::image_models::GENERATE]}},
                {"id":"both","const_api":{"image_operations":[crate::image_models::EDIT,crate::image_models::GENERATE],"platform_image_pricing":advertised}}
            ]})));
        let (listen, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let models = available_models(&LocalAccess {
            listen,
            api_key: "fixture".into(),
        })
        .await
        .unwrap();
        assert_eq!(models.generate, ["both", "generate-only"]);
        assert_eq!(models.edit, ["both"]);
        assert_eq!(
            models.platform_pricing,
            BTreeMap::from([("both".into(), pricing)])
        );
        let guide = invocation(Some(models)).unwrap();
        assert_eq!(guide["generate"][2], "MODEL");
        assert_eq!(guide["edit"][2], "both");
        assert_eq!(guide["models"], serde_json::json!(["image", "models"]));
        task.abort();
    }

    #[tokio::test]
    async fn result_download_rejects_non_public_destinations_before_fetch() {
        for url in [
            "http://example.com/x",
            "https://user@example.com/x",
            "https://example.com:8080/x",
            "https://localhost/x",
            "https://127.0.0.1/x",
        ] {
            assert!(download_result(url).await.is_err());
        }
        for ip in [
            "0.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "192.0.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "::ffff:127.0.0.1",
            "fd00::1",
            "2002:7f00:1::1",
        ] {
            assert!(!public_image_address(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_image_address("1.1.1.1".parse().unwrap()));
    }

    #[tokio::test]
    async fn provider_chosen_codec_preserves_paid_result_and_existing_files() {
        let route = warp::post()
            .and(warp::path!("v1" / "images" / "generations"))
            .and(warp::header::exact(
                crate::supplier::IMAGE_RESPONSE_FORMAT_HEADER,
                "b64_json",
            ))
            .and(warp::body::json::<serde_json::Value>())
            .map(|body| {
                assert_eq!(
                    body,
                    serde_json::json!({"model":"jpeg-model","prompt":"test"})
                );
                warp::reply::json(&serde_json::json!({"data":[{"b64_json":"/9j/"}]}))
            });
        let (listen, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let folder = tempfile::tempdir().unwrap();
        let command = || Command {
            model: "jpeg-model".into(),
            prompt: "test".into(),
            output: folder.path().join("out.png"),
            input: None,
            size: None,
            quality: None,
        };
        let access = || LocalAccess {
            listen,
            api_key: "fixture".into(),
        };
        execute(command(), access()).await.unwrap();
        assert_eq!(
            std::fs::read(folder.path().join("out.jpg")).unwrap(),
            [255, 216, 255]
        );
        std::fs::write(folder.path().join("out.jpg"), b"keep").unwrap();
        execute(command(), access()).await.unwrap();
        assert_eq!(
            std::fs::read(folder.path().join("out.jpg")).unwrap(),
            b"keep"
        );
        assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 2);
        task.abort();
    }

    #[test]
    fn published_invocation_uses_only_the_current_appimage_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join(".mount_CONST");
        let executable = directory.join("usr/bin/const-api-client");
        let image = temp.path().join("CONST API.AppImage");
        assert_eq!(
            appimage_launcher(
                executable.clone(),
                Some((directory.into(), image.clone().into()))
            ),
            image
        );
        assert_eq!(
            appimage_launcher(
                executable.clone(),
                Some((temp.path().join("other").into(), image.into()))
            ),
            executable
        );
    }

    #[tokio::test]
    async fn generate_then_edit_the_first_file_and_never_overwrite_a_result() {
        const RED: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";
        const BLUE: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYPgPAAEDAQAIicLsAAAAAElFTkSuQmCC";
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let route = warp::post().and(warp::path::full()).and(warp::header::<String>("authorization"))
            .and(warp::header::<String>("content-type")).and(warp::body::bytes())
            .map(move |path: warp::path::FullPath, auth, content_type: String, body: bytes::Bytes| {
                assert_eq!(auth, "Bearer fixture");
                observed.fetch_add(1, Ordering::SeqCst);
                let image = match path.as_str() {
                    "/v1/images/generations" => {
                        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        assert_eq!(body, serde_json::json!({"model":"image-fixture","prompt":"red","size":"1536x1024","quality":"high"}));
                        RED
                    }
                    "/v1/images/edits" => {
                        assert!(content_type.starts_with("multipart/form-data; boundary="));
                        let input = base64::engine::general_purpose::STANDARD.decode(RED).unwrap();
                        assert!(body.windows(input.len()).any(|w| w == input));
                        for value in ["name=\"model\"", "image-fixture", "name=\"prompt\"", "blue", "name=\"image\"", "name=\"size\"\r\n\r\n1536x1024", "name=\"quality\"\r\n\r\nhigh"] {
                            assert!(body.windows(value.len()).any(|w| w == value.as_bytes()));
                        }
                        BLUE
                    }
                    _ => panic!("unexpected image operation"),
                };
                warp::reply::json(&serde_json::json!({"data":[{"b64_json":image}],"usage":{"input_tokens":3,"output_tokens":5}}))
            });
        let (listen, server) = crate::bind_ephemeral!(route, ([127, 0, 0, 1], 0));
        let task = tokio::spawn(server);
        let access = || LocalAccess {
            listen,
            api_key: "fixture".into(),
        };
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("cover.png");
        let second = temp.path().join("cover-blue.png");
        let command = |prompt: &str, output: &Path, input: Option<PathBuf>| {
            let mut args: Vec<OsString> = [
                "--model",
                "image-fixture",
                "--prompt",
                prompt,
                "--size",
                "1536x1024",
                "--quality",
                "high",
                "--out",
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            args.push(output.into());
            if let Some(input) = input {
                args.push("--input".into());
                args.push(input.into_os_string());
            }
            Command::parse(args.into_iter()).unwrap()
        };
        let too_large = temp.path().join("oversized.png");
        std::fs::File::create(&too_large)
            .unwrap()
            .set_len(crate::MODEL_MESSAGE_MAX_BYTES as u64 + 1)
            .unwrap();
        for invalid in [too_large, temp.path().to_owned()] {
            assert!(
                execute(command("invalid", &first, Some(invalid)), access())
                    .await
                    .is_err()
            );
        }
        let unassigned = LocalAccess {
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), listen.port()),
            ..access()
        };
        let error = execute(command("invalid address", &first, None), unassigned)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not assigned to this device"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        execute(command("red", &first, None), access())
            .await
            .unwrap();
        execute(command("blue", &second, Some(first.clone())), access())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(&first).unwrap(),
            base64::engine::general_purpose::STANDARD
                .decode(RED)
                .unwrap()
        );
        assert_eq!(
            std::fs::read(&second).unwrap(),
            base64::engine::general_purpose::STANDARD
                .decode(BLUE)
                .unwrap()
        );
        assert!(
            execute(command("again", &second, None), access())
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        task.abort();
    }
}
