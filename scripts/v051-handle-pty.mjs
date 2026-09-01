import fs from 'node:fs';
const p='apps/sidewired/src/main.rs';
let s=fs.readFileSync(p,'utf8');
const old=`        let (master, resize, stdin, stdout, stderr) =
            open_pty(request.cols.max(1), request.rows.max(1))?;
        let mut command = Command::new(&request.program);
        command
            .args(&request.args)
            .env("TERM", &request.term)
            .env("COLORTERM", "truecolor")
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        apply_identity(&mut command, request.identity)?;`;
if(!s.includes(old)) throw new Error('old PTY child setup not found');
const replacement=`        let (master, resize, slave_name) =
            open_pty_master(request.cols.max(1), request.rows.max(1))?;
        let mut command = Command::new(&request.program);
        command
            .args(&request.args)
            .env("TERM", &request.term)
            .env("COLORTERM", "truecolor")
            .env("SHELL", "/system/bin/sh")
            .env("TMPDIR", "/data/local/tmp")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match request.identity {
            ExecIdentity::Shell => {
                command.env("HOME", "/data/local/tmp").env("USER", "shell").env("LOGNAME", "shell");
            }
            ExecIdentity::Root => {
                command.env("HOME", "/data/adb").env("USER", "root").env("LOGNAME", "root");
            }
        }
        let identity = request.identity;
        let child_slave = slave_name.clone();
        unsafe {
            command.pre_exec(move || configure_pty_child(&child_slave, identity));
        }`;
s=s.replace(old,replacement);
fs.writeFileSync(p,s);
