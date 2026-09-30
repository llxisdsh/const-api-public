//! Trae Code's installed byteCrypto format (out-build/vs/base/common/byteCrypto.js).
//! Read the existing sign-in only; never refresh, replace, or export its credentials.
use super::*;
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use sha2::Sha512;

// The installed Trae/Trae CN/TraeWork llm-client crypto format, verified against
// a manually configured local-only probe. These are public format constants,
// not user credentials. The cloud API accepts plaintext but the native client
// later rejects it as "invalid ak", so never fall back to plaintext.
fn model_key_cipher() -> Result<ring::aead::LessSafeKey> {
    let mut key = *b"f4gh7jk9lkln0qs2tuxyjk9ln0qs2tu5";
    for (byte, mask) in key[..8].iter_mut().zip(b"qs2tu5vw") {
        *byte ^= mask;
    }
    ring::aead::UnboundKey::new(&ring::aead::AES_256_GCM, &key)
        .map(ring::aead::LessSafeKey::new)
        .map_err(|_| anyhow!("Trae credential cipher initialization failed"))
}

pub(super) fn encode_model_key(api_key: &str) -> Result<String> {
    use ring::rand::SecureRandom;
    let mut nonce = [0u8; 12];
    ring::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| anyhow!("Trae credential nonce generation failed"))?;
    let mut encrypted = api_key.trim().as_bytes().to_vec();
    model_key_cipher()?
        .seal_in_place_append_tag(
            ring::aead::Nonce::assume_unique_for_key(nonce),
            ring::aead::Aad::from(b"1741255278"),
            &mut encrypted,
        )
        .map_err(|_| anyhow!("Trae credential encryption failed"))?;
    Ok(BASE64.encode([nonce.as_slice(), &encrypted].concat()))
}

pub(super) fn decode_model_key(encoded: &str) -> Option<String> {
    let mut bytes = BASE64.decode(encoded).ok()?;
    if bytes.len() < 28 {
        return None;
    }
    let (nonce, encrypted) = bytes.split_at_mut(12);
    let clear = model_key_cipher()
        .ok()?
        .open_in_place(
            ring::aead::Nonce::try_assume_unique_for_key(nonce).ok()?,
            ring::aead::Aad::from(b"1741255278"),
            encrypted,
        )
        .ok()?;
    String::from_utf8(clear.to_vec()).ok()
}

pub(super) struct TraeSession {
    pub host: String,
    pub account: String,
    pub client: reqwest::Client,
}

fn login_error() -> anyhow::Error {
    anyhow!(crate::native_i18n::text(
        "请先打开并登录 Trae，再重试配置。",
        "Open Trae and sign in before configuring it.",
    ))
}

fn decode_auth(encoded: &str) -> Result<Value> {
    // These are format constants published in both installed Trae editions,
    // not account secrets. Unknown future formats must not be guessed.
    const SALTS: [[u8; 64]; 4] = [
        [
            191, 192, 216, 250, 122, 246, 220, 97, 31, 254, 98, 27, 8, 72, 71, 176, 135, 99, 96,
            18, 127, 101, 203, 104, 211, 102, 191, 125, 37, 72, 150, 156, 51, 229, 121, 35, 17,
            153, 141, 177, 110, 131, 150, 128, 172, 255, 254, 6, 18, 140, 55, 62, 236, 249, 135,
            64, 135, 12, 117, 4, 89, 149, 168, 209,
        ],
        [
            246, 204, 26, 232, 232, 70, 129, 109, 223, 146, 169, 242, 23, 241, 105, 145, 50, 196,
            165, 42, 254, 120, 3, 54, 244, 207, 209, 85, 53, 6, 138, 106, 175, 148, 31, 204, 186,
            186, 165, 182, 87, 142, 49, 10, 39, 110, 26, 154, 86, 56, 173, 125, 18, 64, 198, 225,
            99, 99, 83, 82, 191, 134, 76, 170,
        ],
        [
            82, 9, 106, 213, 48, 54, 165, 56, 191, 64, 163, 158, 129, 243, 215, 251, 124, 227, 57,
            130, 155, 47, 255, 135, 52, 142, 67, 68, 196, 222, 233, 203, 84, 123, 148, 50, 166,
            194, 35, 61, 238, 76, 149, 11, 66, 250, 195, 78, 8, 46, 161, 102, 40, 217, 36, 178,
            118, 91, 162, 73, 109, 139, 209, 37,
        ],
        [
            31, 221, 168, 51, 136, 7, 199, 49, 177, 18, 16, 89, 39, 128, 236, 95, 96, 81, 127, 169,
            25, 181, 74, 13, 45, 229, 122, 159, 147, 201, 156, 239, 160, 224, 59, 77, 174, 42, 245,
            176, 200, 235, 187, 60, 131, 83, 153, 97, 23, 43, 4, 126, 186, 119, 214, 38, 225, 105,
            20, 99, 85, 33, 12, 125,
        ],
    ];
    let encrypted = BASE64.decode(encoded).map_err(|_| login_error())?;
    if encrypted.len() < 54 {
        return Err(login_error());
    }
    let offset = match &encrypted[..6] {
        [116, 99, 5, 16, 0, 0] => 2,
        [18, 57, 32, 32, 2, 3] => 0,
        _ => {
            return Err(anyhow!(crate::native_i18n::text(
                "此 Trae 版本的登录存储格式暂不支持，未修改其配置。",
                "This Trae sign-in storage format is not supported. No configuration was changed.",
            )));
        }
    };
    let mut hash = Sha512::new();
    hash.update(Sha512::digest(&encrypted[6..38]));
    hash.update(std::array::from_fn::<_, 64, _>(|i| {
        SALTS[offset][i] ^ SALTS[offset + 1][i]
    }));
    let material = hash.finalize();
    let mut buffer = encrypted[38..].to_vec();
    let clear = cbc::Decryptor::<aes::Aes128>::new_from_slices(&material[..16], &material[16..32])
        .map_err(|_| login_error())?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| login_error())?;
    if clear.len() < 64 || clear[..64] != Sha512::digest(&clear[64..])[..] {
        return Err(login_error());
    }
    serde_json::from_slice(&clear[64..]).map_err(|_| login_error())
}

pub(super) fn data_root(tool: &str) -> Result<PathBuf> {
    let app = match tool {
        "trae" => "Trae",
        "trae-cn" => "Trae CN",
        // TraeWork 0.1.69 retains its original SOLO application/data identity.
        "trae-work" => "TRAE SOLO CN",
        _ => return Err(anyhow!("unknown Trae edition: {tool}")),
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let root = dirs::data_dir();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = dirs::config_dir();
    Ok(root
        .ok_or_else(|| anyhow!("user configuration directory is unavailable"))?
        .join(app))
}

fn trusted_host(host: &str) -> bool {
    matches!(
        host,
        "https://trae-api-cn.mchost.guru"
            | "https://core-normal.trae.ai"
            | "https://coresg-normal.trae.ai"
            | "https://coreva-normal.trae.ai"
            | "https://core-normal.traeapi.us"
            | "https://core-normal.traeapiusds.us"
    )
}

impl TraeSession {
    pub fn load(tool: &str) -> Result<Self> {
        let candidate = tool_launch_candidate_without_process_scan(tool)?.ok_or_else(|| {
            anyhow!(crate::native_i18n::text(
                "请先定位 Trae 程序。",
                "Locate the Trae app first."
            ))
        })?;
        let app_path = Path::new(&candidate.path);
        let product_path = app_path
            .ancestors()
            .take(5)
            .flat_map(|root| {
                [
                    root.join("resources/app/product.json"),
                    root.join("Contents/Resources/app/product.json"),
                ]
            })
            .find(|path| path.is_file())
            .ok_or_else(|| anyhow!("Trae product.json was not found beside the selected app"))?;
        let product: Value = serde_json::from_slice(&fs::read(product_path)?)?;
        let storage_path = data_root(tool)?.join("User/globalStorage/storage.json");
        let storage: Value =
            serde_json::from_slice(&fs::read(storage_path).map_err(|_| login_error())?)
                .map_err(|_| login_error())?;
        let provider = product
            .pointer("/iCubeApp/nativeAppConfig/authProviderId")
            .and_then(Value::as_str)
            .ok_or_else(login_error)?;
        let encoded = storage
            .get(format!("iCubeAuthInfo://{provider}"))
            .and_then(Value::as_str)
            .ok_or_else(login_error)?;
        Self::from_values(&product, &decode_auth(encoded)?)
    }

    fn from_values(product: &Value, auth: &Value) -> Result<Self> {
        let region = auth
            .pointer("/userRegion/_aiRegion")
            .or_else(|| auth.pointer("/account/uidRegion"))
            .or_else(|| auth.get("aiRegion"))
            .and_then(Value::as_str)
            .unwrap_or("normal");
        let endpoints = product
            .pointer("/bootConfig/agent/trae")
            .ok_or_else(login_error)?;
        let host = endpoints
            .get(region)
            .or_else(|| endpoints.get("normal"))
            .and_then(Value::as_str)
            .filter(|host| trusted_host(host))
            .ok_or_else(|| anyhow!("Unrecognized Trae model service; credentials were not sent"))?;
        let app_id = product
            .pointer("/bootConfig/agent/appId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(login_error)?;
        let token = auth
            .get("token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(login_error)?;
        let user_id = auth
            .get("userId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(login_error)?;
        let mut headers = reqwest::header::HeaderMap::new();
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Cloud-IDE-JWT {token}"))
                .map_err(|_| login_error())?;
        authorization.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
        headers.insert("X-App-Id", reqwest::header::HeaderValue::from_str(app_id)?);
        let client = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .build()?;
        Ok(Self {
            host: host.to_string(),
            account: sha256_hex(format!("{host}\n{user_id}").as_bytes()),
            client,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_credentials_match_native_format_and_never_use_plaintext() {
        // Independent AES-GCM fixture with fake data, nonce = [7; 12].
        let fixture = "BwcHBwcHBwcHBwcHdm16aEQxAFARVuW0G2MMS/M2Giipn6qlBTaOZ7AAdsGCSQ==";
        assert_eq!(
            decode_model_key(fixture).as_deref(),
            Some("const-api-test-key")
        );
        let first = encode_model_key(" const-api-test-key ").unwrap();
        let second = encode_model_key("const-api-test-key").unwrap();
        assert_ne!(first, second, "each encryption needs a fresh nonce");
        assert_eq!(
            decode_model_key(&first).as_deref(),
            Some("const-api-test-key")
        );
        assert_eq!(
            decode_model_key(&second).as_deref(),
            Some("const-api-test-key")
        );
        assert!(decode_model_key("const-api-test-key").is_none());
        let mut corrupt = BASE64.decode(fixture).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(decode_model_key(&BASE64.encode(corrupt)).is_none());
    }

    #[test]
    fn both_installed_auth_formats_decode_without_real_credentials() {
        // Generated with the installed public byteCrypto encoder and fake data.
        for fixture in [
            "dGMFEAAABwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwenv1gG5z40DyPUMc2xXpSECQfsL3hJ/mOvNKu5gqgqnKC6qm6GBNvz1DWMgtmkcOgsQnMkZG+ak50f0aK82Z6x4GRTeCnbCztZSayvwsmcjwxSqT5sFcaf+V5G6CcFL9dGHX3cow6I09MR5efW+iFF",
            "EjkgIAIDBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwf0Z4ZHd2ex6x429lpW8XvMEHPqnY3eKlQom9U8HudTSznmt2fbOdMy9Ul45eB5DrqTieCgeLdxDcBT8+xaQWE626Jh4oh6kM8OPqidRuufrbCXWyJk0GcYV0S8F54YkqDplzQ8ToxydMSxZwSK3JiZ",
        ] {
            assert_eq!(
                decode_auth(fixture).unwrap(),
                json!({"userId":"fake-account","token":"fake-token"})
            );
            let mut broken = BASE64.decode(fixture).unwrap();
            *broken.last_mut().unwrap() ^= 1;
            assert!(decode_auth(&BASE64.encode(broken)).is_err());
        }
    }

    #[test]
    fn account_identity_is_region_bound_but_not_access_token_bound() {
        let product = json!({"bootConfig":{"agent":{"appId":"mock-app","trae":{"normal":"https://core-normal.trae.ai","SG":"https://coresg-normal.trae.ai"}}}});
        let auth =
            json!({"userId":"mock-account","token":"token-a","userRegion":{"_aiRegion":"SG"}});
        let first = TraeSession::from_values(&product, &auth).unwrap();
        assert_eq!(first.host, "https://coresg-normal.trae.ai");
        let mut changed = auth.clone();
        changed["token"] = json!("token-b");
        assert_eq!(
            first.account,
            TraeSession::from_values(&product, &changed)
                .unwrap()
                .account
        );
        changed["userId"] = json!("another-account");
        assert_ne!(
            first.account,
            TraeSession::from_values(&product, &changed)
                .unwrap()
                .account
        );
        let mut untrusted = product;
        untrusted["bootConfig"]["agent"]["trae"]["SG"] = json!("https://example.invalid");
        assert!(TraeSession::from_values(&untrusted, &auth).is_err());
    }

    #[test]
    fn unknown_auth_formats_and_credential_hosts_are_rejected() {
        assert!(decode_auth("").is_err());
        assert!(decode_auth(&BASE64.encode([0u8; 100])).is_err());
        assert!(!trusted_host("https://core-normal.trae.ai.attacker.test"));
        assert!(!trusted_host("http://core-normal.trae.ai"));
        assert!(trusted_host("https://coresg-normal.trae.ai"));
    }
}
