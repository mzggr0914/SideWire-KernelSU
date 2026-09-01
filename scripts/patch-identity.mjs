import fs from 'node:fs';
const root='C:/Users/Administrator/Documents/Dev/Rust/SideWire-KernelSU';
const p=`${root}/crates/sidewire-protocol/src/lib.rs`;
let s=fs.readFileSync(p,'utf8');
s=s.replace('#[derive(Debug, Serialize, Deserialize)]\npub struct ExecRequest {', '#[derive(Debug, Clone, Copy, Serialize, Deserialize)]\npub enum ExecIdentity {\n    Root,\n    Shell,\n}\n\n#[derive(Debug, Serialize, Deserialize)]\npub struct ExecRequest {');
s=s.replace('    pub cwd: Option<String>,\n}', '    pub cwd: Option<String>,\n    pub identity: ExecIdentity,\n}');
fs.writeFileSync(p,s);
