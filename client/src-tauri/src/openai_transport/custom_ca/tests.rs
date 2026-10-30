use super::*;

#[test]
fn ca_selection_matches_codex_precedence_and_empty_values() {
    let select = |a: &str, b: &str| selected_bundle(Some(a.into()), Some(b.into()));
    assert_eq!(
        select("codex.pem", "ssl.pem"),
        Some(("CODEX_CA_CERTIFICATE", "codex.pem".into()))
    );
    assert_eq!(
        select("", "ssl.pem"),
        Some(("SSL_CERT_FILE", "ssl.pem".into()))
    );
    assert_eq!(select("", ""), None);
    assert_eq!(selected_bundle(None, None), None);
}

#[test]
fn ca_bundle_accepts_all_certificates_and_openssl_labels() {
    let one = rcgen::generate_simple_self_signed(vec!["one.test".into()]).unwrap();
    let two = rcgen::generate_simple_self_signed(vec!["two.test".into()]).unwrap();
    let bundle = format!("# certificate bundle\n{}{}", one.cert.pem(), two.cert.pem());
    let roots = parse_bundle(bundle.as_bytes()).unwrap();
    assert_eq!(roots, vec![one.cert.der().clone(), two.cert.der().clone()]);
    let trusted = bundle.replace("CERTIFICATE", "TRUSTED CERTIFICATE");
    assert_eq!(parse_bundle(trusted.as_bytes()).unwrap(), roots);
    assert!(build_config(None).is_ok());
}

#[test]
fn ca_configuration_errors_are_explicit_and_do_not_dump_pem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ca.pem");
    let error = build_config(Some(("CODEX_CA_CERTIFICATE", path.clone()))).unwrap_err();
    assert!(format!("{error:#}").contains("CODEX_CA_CERTIFICATE"));
    for invalid in [
        "",
        "not a certificate",
        "-----BEGIN CERTIFICATE-----\nsensitive-invalid-material\n-----END CERTIFICATE-----",
    ] {
        std::fs::write(&path, invalid).unwrap();
        let error = build_config(Some(("SSL_CERT_FILE", path.clone()))).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("SSL_CERT_FILE"));
        assert!(!message.contains("sensitive-invalid-material"));
    }
}
