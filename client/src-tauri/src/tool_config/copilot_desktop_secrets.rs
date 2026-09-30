use anyhow::{Context, Result, anyhow};

// Copilot App 1.1.23 uses these same service/account names on Windows and macOS.
// Only our provider IDs are passed here; never enumerate the user's credentials.
const SERVICE: &str = "github-copilot-app";

pub(super) trait Secrets {
    fn read(&mut self, provider: &str) -> Result<Option<String>>;
    fn write(&mut self, provider: &str, value: Option<&str>) -> Result<()>;
}

pub(super) struct SystemSecrets;

impl Secrets for SystemSecrets {
    fn read(&mut self, provider: &str) -> Result<Option<String>> {
        read(&format!("byok:{provider}:apiKey"))
    }

    fn write(&mut self, provider: &str, value: Option<&str>) -> Result<()> {
        write(&format!("byok:{provider}:apiKey"), value)
    }
}

#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
fn read(account: &str) -> Result<Option<String>> {
    use windows_sys::Win32::{
        Foundation::{ERROR_NOT_FOUND, GetLastError},
        Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW},
    };
    let target = wide(&format!("{account}.{SERVICE}"));
    let mut record: *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut record) } == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_NOT_FOUND {
            return Ok(None);
        }
        return Err(std::io::Error::from_raw_os_error(code as i32))
            .context("read Copilot desktop credential");
    }
    let result = (|| {
        let record = unsafe { record.as_ref() }
            .context("Copilot desktop credential store returned no record")?;
        let len = record.CredentialBlobSize as usize;
        if len == 0 {
            return Ok(Some(String::new()));
        }
        if record.CredentialBlob.is_null() || len > 65_536 || !len.is_multiple_of(2) {
            return Err(anyhow!("invalid Copilot desktop credential encoding"));
        }
        let bytes = unsafe { std::slice::from_raw_parts(record.CredentialBlob, len) };
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&words)
            .map(Some)
            .context("decode Copilot desktop credential")
    })();
    unsafe { CredFree(record.cast()) };
    result
}

#[cfg(target_os = "windows")]
fn write(account: &str, value: Option<&str>) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{ERROR_NOT_FOUND, GetLastError},
        Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredWriteW,
        },
    };
    let mut target = wide(&format!("{account}.{SERVICE}"));
    let Some(value) = value else {
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_NOT_FOUND {
                return Err(std::io::Error::from_raw_os_error(code as i32))
                    .context("remove Copilot desktop credential");
            }
        }
        return Ok(());
    };
    // windows-native-keyring-store stores passwords as UTF-16LE, without a NUL.
    let mut blob = value
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut account = wide(account);
    let record = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        UserName: account.as_mut_ptr(),
        CredentialBlobSize: blob
            .len()
            .try_into()
            .context("Copilot credential too large")?,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..CREDENTIALW::default()
    };
    if unsafe { CredWriteW(&record, 0) } == 0 {
        return Err(std::io::Error::last_os_error()).context("write Copilot desktop credential");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn read(account: &str) -> Result<Option<String>> {
    match security_framework::passwords::get_generic_password(SERVICE, account) {
        Ok(value) => String::from_utf8(value)
            .map(Some)
            .context("decode Copilot desktop credential"),
        Err(error) if error.code() == security_framework_sys::base::errSecItemNotFound => Ok(None),
        Err(error) => Err(error).context("read Copilot desktop credential"),
    }
}

#[cfg(target_os = "macos")]
fn write(account: &str, value: Option<&str>) -> Result<()> {
    use security_framework::passwords::{delete_generic_password, set_generic_password};
    let result = match value {
        Some(value) => set_generic_password(SERVICE, account, value.as_bytes()),
        None => delete_generic_password(SERVICE, account),
    };
    match result {
        Ok(()) => Ok(()),
        Err(error)
            if value.is_none()
                && error.code() == security_framework_sys::base::errSecItemNotFound =>
        {
            Ok(())
        }
        Err(error) => Err(error).context("update Copilot desktop credential"),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn read(_account: &str) -> Result<Option<String>> {
    Err(anyhow!(
        "Copilot desktop configuration requires Windows or macOS ({SERVICE})"
    ))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn write(_account: &str, _value: Option<&str>) -> Result<()> {
    Err(anyhow!(
        "Copilot desktop configuration requires Windows or macOS ({SERVICE})"
    ))
}
