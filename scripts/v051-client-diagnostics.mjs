import fs from 'node:fs';
const p='apps/sidewire/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace(`enum ConsoleEvent {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}`, `enum ConsoleEvent {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Error(String),
}`);
s=s.replace(`                Err(_) => break,`, `                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!("terminal event poll failed: {error}")));
                    break;
                },`);
s=s.replace(`                Err(_) => break,
            }
        }
    });`, `                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!("terminal event read failed: {error}")));
                    break;
                },
            }
        }
    });`);
fs.writeFileSync(p,s);
