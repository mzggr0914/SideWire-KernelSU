import fs from "node:fs";

const cargoPath = "apps/sidewire/Cargo.toml";
let cargo = fs.readFileSync(cargoPath, "utf8");
cargo = cargo.replace(/\n\[target\.'cfg\(windows\)'\.dependencies\]\nwindows-sys[^\n]*\n?/g, "\n");
fs.writeFileSync(cargoPath, cargo);

const srcPath = "apps/sidewire/src/main.rs";
let src = fs.readFileSync(srcPath, "utf8");
src = src.replace("    time::Duration,\n", "");
src = src.replace("fn key_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {", "#[cfg(not(windows))]\nfn key_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {");
const start = src.indexOf("fn spawn_console_reader(\n");
const end = src.indexOf("async fn run_pty_client(\n", start);
if (start < 0 || end < 0) throw new Error("console reader markers not found");
const replacement = String.raw`#[cfg(not(windows))]
fn spawn_console_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    tokio::task::spawn_blocking(move || {
        use crossterm::event::{Event, poll, read};
        while !stop.load(Ordering::Relaxed) {
            match poll(std::time::Duration::from_millis(50)) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!("terminal event poll failed: {error}")));
                    break;
                }
            }
            match read() {
                Ok(Event::Key(key)) => {
                    if let Some(bytes) = key_bytes(key) {
                        if tx.send(ConsoleEvent::Input(bytes)).is_err() { break; }
                    }
                }
                Ok(Event::Resize(cols, rows)) => {
                    if tx.send(ConsoleEvent::Resize { cols, rows }).is_err() { break; }
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!("terminal event read failed: {error}")));
                    break;
                }
            }
        }
    });
}

#[cfg(windows)]
fn spawn_console_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    let input_stop = stop.clone();
    let input_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut byte = [0u8; 1];
        while !input_stop.load(Ordering::Relaxed) {
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    if input_tx.send(ConsoleEvent::Input(vec![byte[0]])).is_err() { break; }
                }
                Err(error) => {
                    let _ = input_tx.send(ConsoleEvent::Error(format!("stdin read failed: {error}")));
                    break;
                }
            }
        }
    });

    tokio::task::spawn_blocking(move || {
        let mut last = crossterm::terminal::size().unwrap_or((80, 24));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(size) = crossterm::terminal::size() {
                if size != last {
                    last = size;
                    if tx.send(ConsoleEvent::Resize { cols: size.0, rows: size.1 }).is_err() { break; }
                }
            }
        }
    });
}
`;
src = src.slice(0, start) + replacement + "\n" + src.slice(end);
fs.writeFileSync(srcPath, src);
