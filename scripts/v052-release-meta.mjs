import fs from 'node:fs';
let s=fs.readFileSync('Cargo.toml','utf8').replace('version = "0.5.1"','version = "0.5.2"');
fs.writeFileSync('Cargo.toml',s);
let p=fs.readFileSync('module/module.prop','utf8');
p=p.replace('version=0.5.1','version=0.5.2').replace('versionCode=8','versionCode=9');
fs.writeFileSync('module/module.prop',p);
let c=fs.readFileSync('module/customize.sh','utf8').replace(/SideWire 0\.5\.1/g,'SideWire 0.5.2');
fs.writeFileSync('module/customize.sh',c);
