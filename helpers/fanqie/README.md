# Fanqie chapter helper

Independent GPL-3.0-only program. The MIT reader service invokes it over private stdin/stdout; the helper does not depend on the host GUI, workspace paths, Python or external Lua scripts.

The unchanged Lua reading SDK is vendored from hesan1232/fanqie.koplugin, commit 9ba6f8151a24815be1c5a6917750704031defa96. Its original copyright notices, GPL license and third-party notices are in vendor/. Those notices describe the upstream plugin; this standalone helper does not embed KOReader. The Rust integration in this directory is distributed under GPL-3.0-only as well.

Build on Windows x64 with Rust and the Visual Studio C++ build tools:

```powershell
cargo test --locked
cargo build --release --locked
```

LuaJIT is compiled from the exact crates.io source recorded in Cargo.lock. The module package includes this directory's corresponding source and the dependencies' license texts. Dependencies are fetched by Cargo from their locked sources; no private SDK or secret build configuration is required.

The only input is `{method:"chapter",book_id:<numeric string>,chapter_id:<numeric string>}`. Reply is `{ok:true,result:{content:<source HTML>}}` or `{ok:false,error:<stable code>}`. There is no URL, file, eval, device replacement or member activation command. Input is limited to 4 KiB; responses and decompression are bounded. Errors never contain the upstream response, URL, device identifiers or keys. Only fixed official HTTPS endpoints are used, redirects are refused, and a stage is never retried automatically.

One locally generated profile and its registered identity/key are saved in Windows Credential Manager. `LAZYBUDDY_CREDENTIAL_SCOPE` supports isolated QA instances and is inherited from the service; never use production scope for tests. The helper does not provide credential export/reset or read account cookies. Chapter text is returned through the private pipe and is not saved to disk. SDK compatibility layers supply Windows OS randomness, standard MD5, inert JSON and bounded decompression. The reader owns the helper's Windows Job and cancels it on request cancellation/timeout or service exit.

The published mlua-sys 0.13.0 crate omits the repository license file. licenses/mlua-sys-0.13.0/LICENSE is the unmodified upstream MIT text from its recorded VCS commit a186d461c7772aed4cb0a9fa1d1899121c6450e7. The package also retains the LuaJIT runtime copyright separately from the luajit-src build crate license.
