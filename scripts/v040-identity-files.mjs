import fs from 'node:fs';
const proto='crates/sidewire-protocol/src/lib.rs';
let p=fs.readFileSync(proto,'utf8');
p=p.replace('pub const VERSION: u16 = 1;','pub const VERSION: u16 = 2;');
p=p.replace('pub struct FilePushRequest {\n    pub path: String,\n}','pub struct FilePushRequest {\n    pub path: String,\n    pub identity: ExecIdentity,\n}');
p=p.replace('pub struct FilePullRequest {\n    pub path: String,\n}','pub struct FilePullRequest {\n    pub path: String,\n    pub identity: ExecIdentity,\n}');
fs.writeFileSync(proto,p);
