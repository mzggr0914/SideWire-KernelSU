import fs from 'node:fs';
const p='apps/sidewire/src/main.rs';
let s=fs.readFileSync(p,'utf8');
const a=s.indexOf('async fn shell_exec(');
const b=s.indexOf('\nasync fn run_shell(',a);
if(a>=0&&b>=0) s=s.slice(0,a)+s.slice(b+1);
fs.writeFileSync(p,s);
