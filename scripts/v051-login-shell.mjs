import fs from 'node:fs';
const pc='apps/sidewire/src/main.rs';
let s=fs.readFileSync(pc,'utf8');
s=s.replace(`        "/system/bin/sh".into(),
        vec!["-i".into()],`, `        "/system/bin/sh".into(),
        Vec::new(),`);
fs.writeFileSync(pc,s);
const dp='apps/sidewired/src/main.rs';
let d=fs.readFileSync(dp,'utf8');
const marker='        let mut command = Command::new(&request.program);\n';
if(!d.includes(marker)) throw new Error('PTY command marker missing');
d=d.replace(marker, marker+'        if request.program == "/system/bin/sh" && request.args.is_empty() {\n            use std::os::unix::process::CommandExt;\n            command.as_std_mut().arg0("-sh");\n        }\n');
fs.writeFileSync(dp,d);
