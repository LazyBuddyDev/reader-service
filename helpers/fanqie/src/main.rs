mod vault;
use md5::{Digest, Md5};
use mlua::{Function, Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{self, BufRead, Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
const MAX: u64 = 4 * 1024 * 1024;
fn error(code: &'static str) -> mlua::Error {
    mlua::Error::runtime(code)
}
fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 24 && s.bytes().all(|b| b.is_ascii_digit())
}
fn endpoint(url: &str, method: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && uri
            .authority()
            .is_some_and(|a| a.port_u16().is_none() && !a.as_str().contains('@'))
        && match (uri.host(), uri.path(), method) {
            (Some("log.snssdk.com"), "/service/2/device_register/", "POST") => true,
            (Some("api5-normal-sinfonlinec.fqnovel.com"), "/reading/crypt/registerkey", "POST") => {
                true
            }
            (
                Some("api5-normal-sinfonlinec.fqnovel.com"),
                "/reading/reader/batch_full/v",
                "GET",
            ) => true,
            _ => false,
        }
}
struct Protocol {
    lua: Lua,
    seen: Arc<Mutex<HashSet<String>>>,
    failure: Arc<Mutex<Option<&'static str>>>,
}
impl Protocol {
    fn runtime() -> mlua::Result<Lua> {
        // Only the embedded, pinned SDK runs here; the pipe never accepts Lua or URLs.
        let lua = unsafe { Lua::unsafe_new() };
        lua.set_memory_limit(64 * 1024 * 1024)?;
        let preload: Table = lua.globals().get::<Table>("package")?.get("preload")?;
        for (name, source) in [
            (
                "fanqie.reading",
                include_str!("../vendor/fanqie/reading.lua"),
            ),
            (
                "fanqie.reading.aes",
                include_str!("../vendor/fanqie/reading/aes.lua"),
            ),
            (
                "fanqie.reading.crypto",
                include_str!("../vendor/fanqie/reading/crypto.lua"),
            ),
            (
                "fanqie.reading.device",
                include_str!("../vendor/fanqie/reading/device.lua"),
            ),
            (
                "fanqie.reading.sign",
                include_str!("../vendor/fanqie/reading/sign.lua"),
            ),
            (
                "fanqie.reading.sm3",
                include_str!("../vendor/fanqie/reading/sm3.lua"),
            ),
            (
                "fanqie.reading.text",
                include_str!("../vendor/fanqie/reading/text.lua"),
            ),
        ] {
            preload.set(
                name,
                lua.create_function(move |lua, ()| lua.load(source).eval::<Table>())?,
            )?;
        }
        lua.globals().set(
            "native_md5",
            lua.create_function(|_, s: mlua::LuaString| {
                Ok(format!("{:x}", Md5::digest(s.as_bytes())))
            })?,
        )?;
        lua.globals().set(
            "native_random",
            lua.create_function(|lua, n: usize| {
                if n > 256 {
                    return Err(error("invalid_random_size"));
                }
                let mut bytes = vec![0u8; n];
                if unsafe {
                    windows_sys::Win32::Security::Cryptography::BCryptGenRandom(
                        std::ptr::null_mut(),
                        bytes.as_mut_ptr(),
                        n as u32,
                        windows_sys::Win32::Security::Cryptography::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                    )
                } < 0
                {
                    return Err(error("random_failed"));
                }
                lua.create_string(bytes)
            })?,
        )?;
        lua.load(r#"package.preload['ffi/sha2']=function() return {md5=function(s) return native_md5(s) end} end
        local crypto=require('fanqie.reading.crypto'); crypto.urandom=native_random
        -- No external module or Lua file may be loaded from the working directory.
        package.path=''; package.cpath=''
        package.loaders={package.loaders[1]}
        io={open=function() return nil end}; os.execute=nil; os.exit=nil; os.remove=nil; os.rename=nil
        print=function() end
    "#).exec()?;
        let crypto: Table = lua.load("return require('fanqie.reading.crypto')").eval()?;
        crypto.set(
            "maybe_decompress",
            lua.create_function(|lua, (data, status): (mlua::LuaString, Option<i64>)| {
                let bytes = data.as_bytes();
                let mut output = Vec::new();
                let reader: Option<Box<dyn Read + '_>> = if bytes.starts_with(&[31, 139]) {
                    Some(Box::new(flate2::read::GzDecoder::new(&bytes[..])))
                } else if bytes.first() == Some(&0x78) {
                    Some(Box::new(flate2::read::ZlibDecoder::new(&bytes[..])))
                } else if status.unwrap_or(0) > 0 {
                    Some(Box::new(flate2::read::DeflateDecoder::new(&bytes[..])))
                } else {
                    None
                };
                if let Some(r) = reader {
                    r.take(MAX + 1)
                        .read_to_end(&mut output)
                        .map_err(|_| error("source_decode_failed"))?;
                } else {
                    output.extend_from_slice(&bytes);
                }
                if output.len() as u64 > MAX {
                    return Err(error("source_response_too_large"));
                }
                lua.create_string(output)
            })?,
        )?;
        Ok(lua)
    }
    fn new() -> mlua::Result<Self> {
        let lua = Self::runtime()?;
        let saved = vault::read().map_err(error)?;
        if !saved.is_object() {
            return Err(error("invalid_saved_identity"));
        }
        lua.globals().set("saved", lua.to_value(&saved)?)?;
        lua.globals().set(
            "save_identity",
            lua.create_function(|lua, v: LuaValue| {
                let value: Value = lua.from_value(v)?;
                vault::save(&value).map_err(error)
            })?,
        )?;
        lua.load(r#"settings={values=saved,get=function(self,k,d) local v=self.values[k]; if v==nil then return d end return v end,
        set=function(self,k,v) self.values[k]=v end,flush=function(self) save_identity(self.values) end}
    "#).exec()?;
        lua.load(r#"local device=require('fanqie.reading.device'); local original=device.newLocalProfile
            device.newLocalProfile=function()
                local profile=settings:get('reading_profile')
                if not profile then profile=original(); settings:set('reading_profile',profile); settings:flush() end
                local copy={}; for k,v in pairs(profile) do copy[k]=v end; return copy
            end
        "#).exec()?;
        let agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .build(),
            )
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .new_agent();
        let seen = Arc::new(Mutex::new(HashSet::<String>::new()));
        let visits = seen.clone();
        let failure = Arc::new(Mutex::new(None));
        let last = failure.clone();
        let client = lua.create_table()?;
        client.set(
            "json_encode",
            lua.create_function(|lua, (_, v): (Table, LuaValue)| {
                let value: Value = lua.from_value(v)?;
                serde_json::to_string(&value).map_err(|_| error("source_encode_failed"))
            })?,
        )?;
        client.set(
            "json_decode",
            lua.create_function(|lua, (_, bytes): (Table, mlua::LuaString)| {
                let value: Value = serde_json::from_slice(&bytes.as_bytes())
                    .map_err(|_| error("invalid_source_response"))?;
                lua.to_value(&value)
            })?,
        )?;
        client.set(
            "request",
            lua.create_function(move |lua, (_, options): (Table, Table)| {
                let url: String = options.get("url")?;
                let method: String = options.get("method")?;
                if url.len() > 16384 || !endpoint(&url, &method) {
                    return Err(error("source_access_denied"));
                }
                let path = url.parse::<ureq::http::Uri>().unwrap().path().to_owned();
                if !visits.lock().unwrap().insert(path) {
                    *last.lock().unwrap() = Some("source_retry_stopped");
                    return Err(error("source_retry_stopped"));
                }
                let mut request = ureq::http::Request::builder()
                    .method(method.as_str())
                    .uri(url);
                let headers: Table = options.get("headers")?;
                for pair in headers.pairs::<String, String>() {
                    let (name, value) = pair?;
                    request = request.header(name, value);
                }
                let body: Option<mlua::LuaString> = options.get("body")?;
                let bytes = body.map(|s| s.as_bytes().to_vec()).unwrap_or_default();
                let request = request
                    .body(bytes)
                    .map_err(|_| error("invalid_source_request"))?;
                let result = (|| -> Result<Vec<u8>, &'static str> {
                    let mut response = agent.run(request).map_err(|e| match e {
                        ureq::Error::StatusCode(401 | 403) => "source_access_denied",
                        ureq::Error::StatusCode(429) => "source_rate_limited",
                        ureq::Error::Timeout(_) => "source_timeout",
                        _ => "source_network_failed",
                    })?;
                    if response.status().is_redirection() {
                        return Err("source_access_denied");
                    }
                    if response.headers().contains_key("bdturing-verify")
                        || response.headers().contains_key("x-vc-bdturing-parameters")
                    {
                        return Err("source_verification_required");
                    }
                    let bytes = response
                        .body_mut()
                        .with_config()
                        .limit(MAX)
                        .read_to_vec()
                        .map_err(|e| match e {
                            ureq::Error::BodyExceedsLimit(_) => "source_response_too_large",
                            ureq::Error::Timeout(_) => "source_timeout",
                            _ => "source_network_failed",
                        })?;
                    if bytes.is_empty() {
                        return Err("source_empty_response");
                    }
                    Ok(bytes)
                })();
                match result {
                    Ok(bytes) => Ok((lua.create_string(bytes)?, 200)),
                    Err(code) => {
                        *last.lock().unwrap() = Some(code);
                        Err(error(code))
                    }
                }
            })?,
        )?;
        lua.globals().set("client", client)?;
        Ok(Self { lua, seen, failure })
    }
    fn fetch(&self, book: &str, chapter: &str) -> Result<Value, &'static str> {
        if !valid_id(book) || !valid_id(chapter) {
            return Err("invalid_source_book");
        }
        if let Some(code) = *self.failure.lock().unwrap() {
            if matches!(
                code,
                "source_access_denied" | "source_verification_required" | "source_empty_response"
            ) {
                return Err(code);
            }
        }
        self.seen.lock().unwrap().clear();
        *self.failure.lock().unwrap() = None;
        let reading: Table = self
            .lua
            .load("return require('fanqie.reading')")
            .eval()
            .map_err(|_| "source_runtime_failed")?;
        let f: Function = reading.get("fetch").map_err(|_| "source_runtime_failed")?;
        let result = f.call::<(Option<Table>, Option<String>)>((
            self.lua.globals().get::<Table>("client").unwrap(),
            self.lua.globals().get::<Table>("settings").unwrap(),
            book,
            chapter,
        ));
        let code = (*self.failure.lock().unwrap()).unwrap_or("source_decode_failed");
        match result {
            Ok((Some(row), _)) => {
                let content: String = row.get("content").map_err(|_| "source_decode_failed")?;
                if content.is_empty() || content.len() as u64 > MAX {
                    return Err("invalid_source_response");
                }
                Ok(json!({"content":content}))
            }
            Err(e) => Err(classify(&e, code)),
            _ => Err(code),
        }
    }
}
fn classify(e: &mlua::Error, default: &'static str) -> &'static str {
    let message = e.to_string();
    [
        "credential_save_failed",
        "credential_read_failed",
        "invalid_saved_identity",
        "invalid_credential_scope",
    ]
    .into_iter()
    .find(|s| message.contains(s))
    .unwrap_or(default)
}
fn main() {
    let mut protocol = None;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        if (&mut input)
            .take(4097)
            .read_until(b'\n', &mut bytes)
            .unwrap_or(0)
            == 0
            || bytes.len() > 4096
        {
            break;
        }
        let Ok(request) = serde_json::from_slice::<Value>(&bytes) else {
            break;
        };
        let result = if request["method"] == "chapter"
            && request["book_id"].as_str().is_some_and(valid_id)
            && request["chapter_id"].as_str().is_some_and(valid_id)
        {
            if protocol.is_none() {
                protocol = Some(Protocol::new());
            }
            match protocol.as_ref().unwrap() {
                Ok(p) => p.fetch(
                    request["book_id"].as_str().unwrap(),
                    request["chapter_id"].as_str().unwrap(),
                ),
                Err(e) => Err(classify(e, "source_runtime_failed")),
            }
        } else {
            Err("invalid_arguments")
        };
        let reply = match result {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(code) => json!({"ok":false,"error":code}),
        };
        if serde_json::to_writer(&mut output, &reply).is_err()
            || output.write_all(b"\n").is_err()
            || output.flush().is_err()
        {
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restricted_endpoints_and_ids() {
        assert!(endpoint(
            "https://log.snssdk.com/service/2/device_register/?a=1",
            "POST"
        ));
        for url in [
            "http://log.snssdk.com/service/2/device_register/",
            "https://evil.invalid/reading/reader/batch_full/v",
            "https://u@log.snssdk.com/service/2/device_register/",
            "https://log.snssdk.com:443/service/2/device_register/",
            "https://log.snssdk.com/other",
        ] {
            assert!(!endpoint(url, "POST"));
        }
        assert!(!valid_id("../1"));
        assert!(valid_id("6883749008234250760"));
    }
    #[test]
    fn embedded_sdk_vectors_and_windows_runtime() {
        let lua = Protocol::runtime().unwrap();
        assert!(
            lua.load("return require('ffi')~=nil")
                .eval::<bool>()
                .unwrap()
        );
        lua.load(r#"local aes=require('fanqie.reading.aes'); local sm3=require('fanqie.reading.sm3')
        assert(aes.to_hex(aes.cbc_encrypt(aes.from_hex('6bc1bee22e409f96e93d7e117393172a'),aes.from_hex('2b7e151628aed2a6abf7158809cf4f3c'),aes.from_hex('000102030405060708090a0b0c0d0e0f'),false))=='7649abac8119b246cee98e9b12e9197d')
        assert(sm3.hex('abc')=='66c7f0f462eeedd9d1f2d46bdc10e4e24167c4875cf2f7a2297da02b8f4ba8e0')
        local sign=require('fanqie.reading.sign'); local headers=sign.sign_headers('aid=1967','123',{khronos=1700000000})
        assert(headers['X-Argus'] and headers['X-Ladon'] and headers['X-Helios'])
        "#).exec().unwrap();
    }
}
