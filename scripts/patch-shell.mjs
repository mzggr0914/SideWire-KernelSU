import fs from 'node:fs';
const path = 'apps/sidewire/src/main.rs';
let s = fs.readFileSync(path, 'utf8');
const replace = (a, b) => {
  if (!s.includes(a)) throw new Error(`missing patch anchor: ${a.slice(0, 60)}`);
  s = s.replace(a, b);
};
replace(`        program: String,\n        args: Vec<String>,\n    },\n}`, `        program: String,\n        args: Vec<String>,\n        cwd: Option<String>,\n    },\n}`);
replace(`            program,\n            args,\n        } => match resolve_device`, `            program,\n            args,\n            cwd,\n        } => match resolve_device`);
replace(`remote_exec(&session, program, args).await`, `remote_exec(&session, program, args, cwd).await`);
replace(`    args: Vec<String>,\n) -> Result<(String, String, Option<i32>)> {`, `    args: Vec<String>,\n    cwd: Option<String>,\n) -> Result<(String, String, Option<i32>)> {`);
replace(`        cwd: None,\n    };\n    write_frame(&mut *stream`, `        cwd,\n    };\n    write_frame(&mut *stream`);
replace(`        program,\n        args,\n    };\n    match request_control`, `        program,\n        args,\n        cwd: None,\n    };\n    match request_control`);
fs.writeFileSync(path, s);
