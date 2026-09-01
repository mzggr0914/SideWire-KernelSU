import fs from 'node:fs';
const p='apps/sidewire/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('    let mut stdout = io::stdout();\n\n    let result: Result<()> = loop {','    let mut stdout = io::stdout();\n    let mut saw_output = false;\n    let mut sent_input = false;\n\n    let result: Result<()> = loop {');
const old=`                let Some(event) = event else { break Ok(()); };
                let outgoing = match event {
                    ConsoleEvent::Input(bytes) => raw_frame(FrameKind::PtyInput, stream_id, bytes),
                    ConsoleEvent::Resize { cols, rows } => frame(
                        FrameKind::PtyResize,
                        stream_id,
                        &PtyResize { cols, rows },
                    )?,
                };
                write_frame(reader.get_mut(), &outgoing).await?;`;
const repl=`                let Some(event) = event else {
                    break Err(anyhow::anyhow!("terminal input reader stopped unexpectedly"));
                };
                let outgoing = match event {
                    ConsoleEvent::Input(bytes) => {
                        sent_input = true;
                        raw_frame(FrameKind::PtyInput, stream_id, bytes)
                    }
                    ConsoleEvent::Resize { cols, rows } => frame(
                        FrameKind::PtyResize,
                        stream_id,
                        &PtyResize { cols, rows },
                    )?,
                    ConsoleEvent::Error(message) => break Err(anyhow::anyhow!(message)),
                };
                write_frame(reader.get_mut(), &outgoing).await?;`;
if(!s.includes(old)) throw new Error('PTY client event block not found');
s=s.replace(old,repl);
fs.writeFileSync(p,s);
s=fs.readFileSync(p,'utf8');
const oldRemote=`                    FrameKind::PtyOutput => {
                        stdout.write_all(&remote.payload)?;
                        stdout.flush()?;
                    }
                    FrameKind::PtyExit => {
                        let _: PtyExit = decode(&remote.payload)?;
                        break Ok(());
                    }`;
const replRemote=`                    FrameKind::PtyOutput => {
                        saw_output = true;
                        stdout.write_all(&remote.payload)?;
                        stdout.flush()?;
                    }
                    FrameKind::PtyExit => {
                        let exit: PtyExit = decode(&remote.payload)?;
                        if !saw_output && !sent_input {
                            break Err(anyhow::anyhow!(
                                "remote PTY exited immediately (code {:?})",
                                exit.code
                            ));
                        }
                        break Ok(());
                    }`;
if(!s.includes(oldRemote)) throw new Error('PTY client remote block not found');
s=s.replace(oldRemote,replRemote);
fs.writeFileSync(p,s);
