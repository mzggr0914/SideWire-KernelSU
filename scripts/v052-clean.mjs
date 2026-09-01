import fs from 'node:fs';
const p='apps/sidewire/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('    let mut saw_output = false;\n    let mut sent_input = false;\n\n','');
fs.writeFileSync(p,s);
