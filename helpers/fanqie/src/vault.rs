use serde_json::Value;
use windows_sys::Win32::Security::Credentials::*;

fn target() -> Result<Vec<u16>, &'static str> {
    let scope = std::env::var("LAZYBUDDY_CREDENTIAL_SCOPE").unwrap_or_default();
    if scope.len() > 48
        || !scope
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("invalid_credential_scope");
    }
    Ok(format!("LazyBuddy/Reader/{scope}/fanqie/v1")
        .encode_utf16()
        .chain(Some(0))
        .collect())
}
pub fn read() -> Result<Value, &'static str> {
    let target = target()?;
    let mut pointer = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) } == 0 {
        return if std::io::Error::last_os_error().raw_os_error() == Some(1168) {
            Ok(serde_json::json!({}))
        } else {
            Err("credential_read_failed")
        };
    }
    let result = unsafe {
        let entry = &*pointer;
        if entry.CredentialBlobSize == 0 || entry.CredentialBlobSize > 2560 {
            Err("invalid_saved_identity")
        } else {
            serde_json::from_slice(std::slice::from_raw_parts(
                entry.CredentialBlob,
                entry.CredentialBlobSize as usize,
            ))
            .map_err(|_| "invalid_saved_identity")
        }
    };
    unsafe {
        CredFree(pointer.cast());
    }
    let value = result?;
    validate(&value)?;
    Ok(value)
}
fn validate(value: &Value) -> Result<(), &'static str> {
    let object = value.as_object().ok_or("invalid_saved_identity")?;
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "reading_profile" | "reading_device" | "reading_v1_key"
        )
    }) {
        return Err("invalid_saved_identity");
    }
    if let Some(device) = object.get("reading_device") {
        for key in ["device_id", "install_id"] {
            if !device[key]
                .as_str()
                .is_some_and(|s| super::valid_id(s) && s.bytes().any(|b| b != b'0'))
            {
                return Err("invalid_saved_identity");
            }
        }
    }
    if let Some(key) = object.get("reading_v1_key") {
        if !key["key"].as_str().is_some_and(|s| {
            s.len() >= 32 && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_hexdigit())
        }) || key["device_id"] != value["reading_device"]["device_id"]
        {
            return Err("invalid_saved_identity");
        }
    }
    Ok(())
}
pub fn save(value: &Value) -> Result<(), &'static str> {
    validate(value)?;
    let mut target = target()?;
    let mut bytes = serde_json::to_vec(value).map_err(|_| "credential_save_failed")?;
    if bytes.len() > 2560 {
        return Err("credential_too_large");
    }
    let entry = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    let written = unsafe { CredWriteW(&entry, 0) };
    bytes.fill(0);
    if written == 0 {
        Err("credential_save_failed")
    } else {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrupt_state_cannot_be_saved_as_a_new_identity() {
        assert!(validate(&serde_json::json!({})).is_ok());
        for value in [
            serde_json::json!([]),
            serde_json::json!({"reading_device":{"device_id":"0","install_id":"1"}}),
            serde_json::json!({"reading_v1_key":{"key":"bad","device_id":"1"}}),
        ] {
            assert!(validate(&value).is_err());
        }
    }
}
