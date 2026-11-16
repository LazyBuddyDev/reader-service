use std::io::{self, BufRead, Read, Write};
fn main() {
    let Some(root) = std::env::var_os("LAZYBUDDY_DATA_DIR") else {
        return;
    };
    let mut engine = reader_service::Engine::open(root.into());
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut line = Vec::new();
        match (&mut input).take(65537).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.len() > 65536 {
            break;
        }
        let Ok(request) = serde_json::from_slice(&line) else {
            break;
        };
        let reply = match &mut engine {
            Ok(engine) => reader_service::handle(engine, request),
            Err(code) => {
                serde_json::json!({"version":1,"id":request["id"],"ok":false,"error":{"code":code}})
            }
        };
        if serde_json::to_writer(&mut output, &reply).is_err()
            || output
                .write_all(b"\n")
                .and_then(|_| output.flush())
                .is_err()
        {
            break;
        }
    }
}
