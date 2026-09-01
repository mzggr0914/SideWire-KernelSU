import fs from 'node:fs';
let s=fs.readFileSync('Cargo.toml','utf8');
s=s.replace('version = "0.5.0"','version = "0.5.1"');
fs.writeFileSync('Cargo.toml',s);
fs.writeFileSync('module/module.prop',`id=sidewire\nname=SideWire\nversion=0.5.1\nversionCode=8\nauthor=SideWire contributors\ndescription=Native Android bridge with real PTY shell, root/shell execution, streaming logcat, file transfer, app tools, and TCP forwarding.\n`);
let c=fs.readFileSync('module/customize.sh','utf8');
c=c.replace(/SideWire 0\.5\.0/g,'SideWire 0.5.1');
fs.writeFileSync('module/customize.sh',c);
