import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
const old=`            FrameKind::PtyOpen => {
                handle_pty(&mut stream, request.stream_id, &request.payload).await?
            }`;
const repl=`            FrameKind::PtyOpen => {
                if let Err(error) = handle_pty(&mut stream, request.stream_id, &request.payload).await {
                    write_frame(
                        &mut stream,
                        &raw_frame(FrameKind::Error, request.stream_id, error.to_string().into_bytes()),
                    ).await?;
                }
            }`;
if(!s.includes(old)) throw new Error('PTY serve arm not found');
s=s.replace(old,repl);
fs.writeFileSync(p,s);
