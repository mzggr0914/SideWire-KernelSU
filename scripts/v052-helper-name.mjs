import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('let (master, resize, slave_name) = open_pty(request.cols.max(1), request.rows.max(1))?;', 'let (master, resize, slave_name) = open_pty_master(request.cols.max(1), request.rows.max(1))?;');
fs.writeFileSync(p,s);
