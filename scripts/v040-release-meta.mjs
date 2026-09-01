import fs from 'node:fs';
let c=fs.readFileSync('Cargo.toml','utf8');
c=c.replace('version = "0.3.1"','version = "0.4.0"');
fs.writeFileSync('Cargo.toml',c);
fs.writeFileSync('module/module.prop','id=sidewire\nname=SideWire\nversion=0.4.0\nversionCode=6\nauthor=SideWire contributors\ndescription=Native Android bridge with shell/root execution, push/pull, app tools, screenshots, logs, and TCP forwarding.\n');
let sh=fs.readFileSync('module/customize.sh','utf8');
sh=sh.replace(/SideWire 0\.3\.1/g,'SideWire 0.4.0');
fs.writeFileSync('module/customize.sh',sh);
