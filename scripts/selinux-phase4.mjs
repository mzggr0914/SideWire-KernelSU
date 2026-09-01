import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
s=s.replace('                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 { return Err(std::io::Error::last_os_error()); }\n','');
s=s.replace('    let mut command = Command::new(&request.program);\n    command\n        .args(&request.args)', '    let mut command = Command::new("/system/bin/sh");\n    command\n        .arg("-c")\n        .arg("exec \\\"$0\\\" \\\"$@\\\"")\n        .arg(&request.program)\n        .args(&request.args)');
fs.writeFileSync(p,s);
