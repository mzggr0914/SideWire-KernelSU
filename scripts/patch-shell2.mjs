import fs from 'node:fs';
const path='apps/sidewire/src/main.rs';
let s=fs.readFileSync(path,'utf8');
const start=s.indexOf('async fn run_shell(');
if(start<0) throw new Error('run_shell not found');
const replacement=`async fn shell_exec(control: &str, device: Option<String>, cwd: Option<String>, command: &str) -> Result<ControlResponse> {
    let request = ControlRequest::Exec {
        device,
        program: "/system/bin/sh".into(),
        args: vec!["-c".into(), command.into()],
        cwd,
    };
    request_control(control, &request).await
}

async fn run_shell(control: &str, device: Option<String>) -> Result<()> {
    let mut cwd = "/".to_string();
    let root = match shell_exec(control, device.clone(), None, "id -u").await? {
        ControlResponse::Exec { stdout, code, .. } => code == Some(0) && stdout.trim() == "0",
        _ => false,
    };
    let host = device.clone().unwrap_or_else(|| "device".into());
    println!("SideWire shell. Type 'exit' to quit.");
    loop {
        print!("{}@{}:{}{} ", if root { "root" } else { "shell" }, host, cwd, if root { "#" } else { "$" });
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 { return Ok(()); }
        let command = line.trim();
        if command.is_empty() { continue; }
        if command.eq_ignore_ascii_case("exit") || command.eq_ignore_ascii_case("quit") { return Ok(()); }
        if command == "su" && root {
            println!("already root (uid=0)");
            continue;
        }
        if let Some(rest) = command.strip_prefix("cd") {
            let target = rest.trim();
            let target = if target.is_empty() { "/" } else { target };
            match shell_exec(control, device.clone(), Some(cwd.clone()), &format!("cd -- {:?} && pwd", target)).await? {
                ControlResponse::Exec { stdout, stderr, code } if code == Some(0) => {
                    cwd = stdout.trim().to_string();
                    if !stderr.is_empty() { eprint!("{stderr}"); }
                }
                ControlResponse::Exec { stderr, code, .. } => {
                    eprint!("{stderr}");
                    if stderr.is_empty() { eprintln!("cd failed: {:?}", code); }
                }
                ControlResponse::Error { message } => eprintln!("error: {message}"),
                _ => eprintln!("error: unexpected server response"),
            }
            continue;
        }
        match shell_exec(control, device.clone(), Some(cwd.clone()), command).await? {
            ControlResponse::Exec { stdout, stderr, code } => {
                print!("{stdout}");
                eprint!("{stderr}");
                if code.unwrap_or(1) != 0 { eprintln!("[exit {:?}]", code); }
            }
            ControlResponse::Error { message } => eprintln!("error: {message}"),
            _ => eprintln!("error: unexpected server response"),
        }
    }
}
`;
s=s.slice(0,start)+replacement;
fs.writeFileSync(path,s);
