import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
const a=s.indexOf('#[cfg(target_os = "android")]\nfn open_pty(');
const b=s.indexOf('\nasync fn handle_pty',a);
if(a<0||b<0) throw new Error('open_pty block not found');
const block=fs.readFileSync('scripts/v051-openpty-block.txt','utf8').trimEnd()+'\n';
s=s.slice(0,a)+block+s.slice(b+1);
fs.writeFileSync(p,s);
