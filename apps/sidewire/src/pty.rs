use super::*;

enum ConsoleEvent {
    Input(Vec<u8>),
    Resize {
        cols: u16,
        rows: u16,
    },
    #[cfg(windows)]
    Complete {
        line: String,
        cursor: usize,
        reply: std::sync::mpsc::SyncSender<Option<PtyCompleteResult>>,
    },
    Error(String),
    #[cfg(windows)]
    Eof,
}

#[cfg(windows)]
#[derive(Default)]
struct LineDisplayState {
    prompt_tail: std::sync::Mutex<String>,
}

struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[cfg(windows)]
fn update_prompt_tail(state: &LineDisplayState, payload: &[u8]) {
    let text = String::from_utf8_lossy(payload);
    let last_break = text
        .char_indices()
        .filter(|(_, character)| matches!(character, '\r' | '\n'))
        .map(|(index, character)| index + character.len_utf8())
        .next_back();
    let mut tail = state
        .prompt_tail
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(start) = last_break {
        tail.clear();
        tail.push_str(&text[start..]);
    } else {
        tail.push_str(&text);
    }
    if tail.len() > 512 {
        let mut start = tail.len() - 512;
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        *tail = tail[start..].to_owned();
    }
}

#[cfg(any(windows, test))]
fn format_completion_candidates(
    candidates: &[String],
    candidate_count: u32,
    terminal_width: u16,
) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let max_chars = candidates
        .iter()
        .map(|candidate| candidate.chars().count())
        .max()
        .unwrap_or(1);
    let column_width = (max_chars + 2).max(2);
    let columns = ((terminal_width as usize).max(1) / column_width).max(1);
    let rows = candidates.len().div_ceil(columns);
    let mut output = String::new();
    for row in 0..rows {
        for column in 0..columns {
            let index = column * rows + row;
            let Some(candidate) = candidates.get(index) else {
                continue;
            };
            output.push_str(candidate);
            if column + 1 < columns && index + rows < candidates.len() {
                let padding = column_width.saturating_sub(candidate.chars().count());
                output.extend(std::iter::repeat_n(' ', padding));
            }
        }
        if row + 1 < rows {
            output.push_str("\r\n");
        }
    }
    let shown = candidates.len() as u32;
    if candidate_count > shown {
        output.push_str(&format!("\r\n... {} more", candidate_count - shown));
    }
    output
}

#[cfg(not(windows))]
fn key_bytes(key: crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mut bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if c.is_ascii() {
                vec![(c.to_ascii_lowercase() as u8) & 0x1f]
            } else {
                return None;
            }
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(5) => b"\x1b[15~".to_vec(),
        KeyCode::F(6) => b"\x1b[17~".to_vec(),
        KeyCode::F(7) => b"\x1b[18~".to_vec(),
        KeyCode::F(8) => b"\x1b[19~".to_vec(),
        KeyCode::F(9) => b"\x1b[20~".to_vec(),
        KeyCode::F(10) => b"\x1b[21~".to_vec(),
        KeyCode::F(11) => b"\x1b[23~".to_vec(),
        KeyCode::F(12) => b"\x1b[24~".to_vec(),
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(not(windows))]
fn spawn_console_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    tokio::task::spawn_blocking(move || {
        use crossterm::event::{Event, poll, read};
        while !stop.load(Ordering::Relaxed) {
            match poll(std::time::Duration::from_millis(50)) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!(
                        "terminal event poll failed: {error}"
                    )));
                    break;
                }
            }
            match read() {
                Ok(Event::Key(key)) => {
                    if let Some(bytes) = key_bytes(key)
                        && tx.send(ConsoleEvent::Input(bytes)).is_err()
                    {
                        break;
                    }
                }
                Ok(Event::Resize(cols, rows)) => {
                    if tx.send(ConsoleEvent::Resize { cols, rows }).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = tx.send(ConsoleEvent::Error(format!(
                        "terminal event read failed: {error}"
                    )));
                    break;
                }
            }
        }
    });
}

#[cfg(windows)]
fn spawn_console_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    let input_stop = stop.clone();
    let input_tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut buffer = [0u8; 1024];
        while !input_stop.load(Ordering::Relaxed) {
            match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if input_tx
                        .send(ConsoleEvent::Input(buffer[..read].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ =
                        input_tx.send(ConsoleEvent::Error(format!("stdin read failed: {error}")));
                    break;
                }
            }
        }
    });

    tokio::task::spawn_blocking(move || {
        let mut last = crossterm::terminal::size().unwrap_or((80, 24));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(size) = crossterm::terminal::size()
                && size != last
            {
                last = size;
                if tx
                    .send(ConsoleEvent::Resize {
                        cols: size.0,
                        rows: size.1,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    });
}

pub(super) async fn run_pty_client(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
) -> Result<()> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let request = ControlRequest::Pty {
        device,
        program,
        args,
        run_as,
        cols,
        rows,
        term: "xterm-256color".into(),
        echo: true,
    };
    let mut stream = TcpStream::connect(control)
        .await
        .with_context(|| format!("connect to SideWire server control {control}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("SideWire server closed PTY control connection");
    }
    match serde_json::from_str::<ControlResponse>(line.trim_end())? {
        ControlResponse::Ok { .. } => {}
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected PTY server response"),
    }

    crossterm::terminal::enable_raw_mode().context("enable terminal raw mode")?;
    let raw_guard = RawModeGuard;
    let stop = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_console_reader(stop.clone(), event_tx);
    let stream_id = 0x5054_5901;
    let mut stdout = io::stdout();
    let (mut net_read, mut net_write) = tokio::io::split(reader);
    let input_loop = async {
        const RAW_BATCH_BYTES: usize = 4 * 1024;
        let mut pending = None;
        loop {
            let event = match pending.take() {
                Some(event) => event,
                None => event_rx
                    .recv()
                    .await
                    .context("terminal input reader stopped")?,
            };
            match event {
                ConsoleEvent::Input(mut bytes) => {
                    while bytes.len() < RAW_BATCH_BYTES {
                        match event_rx.try_recv() {
                            Ok(ConsoleEvent::Input(more)) => bytes.extend_from_slice(&more),
                            Ok(other) => {
                                pending = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    write_raw_frame(&mut net_write, FrameKind::PtyInput, stream_id, &bytes).await?;
                }
                ConsoleEvent::Resize { cols, rows } => {
                    let resize = frame(FrameKind::PtyResize, stream_id, &PtyResize { cols, rows })?;
                    write_frame(&mut net_write, &resize).await?;
                }
                #[cfg(windows)]
                ConsoleEvent::Complete { reply, .. } => {
                    let _ = reply.send(None);
                }
                ConsoleEvent::Error(message) => {
                    let _ =
                        write_raw_frame(&mut net_write, FrameKind::PtyClose, stream_id, &[]).await;
                    bail!(message);
                }
                #[cfg(windows)]
                ConsoleEvent::Eof => {
                    write_raw_frame(&mut net_write, FrameKind::PtyClose, stream_id, &[]).await?;
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
    };

    let output_loop = async {
        let mut saw_output = false;
        loop {
            let remote = read_frame(&mut net_read).await?;
            match remote.kind {
                FrameKind::PtyOutput => {
                    saw_output = true;
                    stdout.write_all(&remote.payload)?;
                    stdout.flush()?;
                }
                FrameKind::PtyExit => {
                    let exit: PtyExit = decode(&remote.payload)?;
                    if !saw_output || exit.code.unwrap_or(0) != 0 {
                        bail!("remote PTY exited (code {:?})", exit.code);
                    }
                    return Ok::<(), anyhow::Error>(());
                }
                FrameKind::Error => {
                    bail!(
                        "remote PTY error: {}",
                        String::from_utf8_lossy(&remote.payload)
                    );
                }
                kind => bail!("unexpected PTY frame {kind:?}"),
            }
        }
    };

    tokio::pin!(input_loop);
    tokio::pin!(output_loop);
    let result = tokio::select! {
        result = &mut input_loop => result,
        result = &mut output_loop => result,
    };
    stop.store(true, Ordering::Relaxed);
    drop(raw_guard);
    result
}

#[cfg(windows)]
fn spawn_standard_line_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    let input_stop = stop.clone();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut line = String::new();
        while !input_stop.load(Ordering::Relaxed) {
            line.clear();
            match std::io::BufRead::read_line(&mut input, &mut line) {
                Ok(0) => {
                    let _ = input_tx.send(ConsoleEvent::Eof);
                    break;
                }
                Ok(_) => {
                    let bytes = normalize_console_line(line.as_bytes());
                    if input_tx.send(ConsoleEvent::Input(bytes)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = input_tx.send(ConsoleEvent::Error(format!(
                        "stdin line read failed: {error}"
                    )));
                    break;
                }
            }
        }
    });
    spawn_line_resize_reader(stop, tx);
}

#[cfg(windows)]
fn spawn_line_resize_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
) {
    std::thread::spawn(move || {
        let mut last = crossterm::terminal::size().unwrap_or((80, 24));
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Ok(size) = crossterm::terminal::size()
                && size != last
            {
                last = size;
                if tx
                    .send(ConsoleEvent::Resize {
                        cols: size.0,
                        rows: size.1,
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    });
}

#[cfg(windows)]
#[repr(C)]
struct ConsoleReadConsoleControl {
    n_length: u32,
    n_initial_chars: u32,
    ctrl_wakeup_mask: u32,
    control_key_state: u32,
}

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    #[link_name = "GetStdHandle"]
    fn get_std_handle(std_handle: u32) -> *mut std::ffi::c_void;
    #[link_name = "GetConsoleMode"]
    fn get_console_mode(console: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    #[link_name = "ReadConsoleW"]
    fn read_console_w(
        console: *mut std::ffi::c_void,
        buffer: *mut std::ffi::c_void,
        chars_to_read: u32,
        chars_read: *mut u32,
        control: *mut ConsoleReadConsoleControl,
    ) -> i32;
    #[link_name = "WriteConsoleW"]
    fn write_console_w(
        console: *mut std::ffi::c_void,
        buffer: *const std::ffi::c_void,
        chars_to_write: u32,
        chars_written: *mut u32,
        reserved: *mut std::ffi::c_void,
    ) -> i32;
}

#[cfg(windows)]
fn windows_console_handle(kind: u32) -> Option<*mut std::ffi::c_void> {
    let handle = unsafe { get_std_handle(kind) };
    let mut mode = 0u32;
    (unsafe { get_console_mode(handle, &mut mode) } != 0).then_some(handle)
}

#[cfg(windows)]
fn write_windows_console(text: &str) -> std::io::Result<()> {
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    let wide: Vec<u16> = text.encode_utf16().collect();
    if wide.is_empty() {
        return Ok(());
    }
    let handle = windows_console_handle(STD_OUTPUT_HANDLE)
        .ok_or_else(|| std::io::Error::other("stdout is not a Windows console"))?;
    let mut written = 0u32;
    let ok = unsafe {
        write_console_w(
            handle,
            wide.as_ptr().cast(),
            wide.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn spawn_line_reader(
    stop: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<ConsoleEvent>,
    display: Arc<LineDisplayState>,
) {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const TAB: u16 = 0x09;
    const ERROR_OPERATION_ABORTED: i32 = 995;
    const BUFFER_CHARS: usize = 8192;

    if windows_console_handle(STD_INPUT_HANDLE).is_none() {
        spawn_standard_line_reader(stop, tx);
        return;
    }
    if windows_console_handle(STD_OUTPUT_HANDLE).is_none() {
        spawn_standard_line_reader(stop, tx);
        return;
    }

    let input_stop = stop.clone();
    let input_tx = tx.clone();
    std::thread::spawn(move || {
        let Some(input_handle) = windows_console_handle(STD_INPUT_HANDLE) else {
            let _ = input_tx.send(ConsoleEvent::Error(
                "stdin stopped being a Windows console".into(),
            ));
            return;
        };
        let mut buffer = vec![0u16; BUFFER_CHARS];
        let mut keep = 0usize;
        let mut last_completion: Option<(String, usize)> = None;
        while !input_stop.load(Ordering::Relaxed) {
            if keep >= buffer.len() - 1 {
                let _ = input_tx.send(ConsoleEvent::Error("console input line is too long".into()));
                break;
            }
            let mut control = ConsoleReadConsoleControl {
                n_length: std::mem::size_of::<ConsoleReadConsoleControl>() as u32,
                n_initial_chars: keep as u32,
                ctrl_wakeup_mask: 1u32 << TAB,
                control_key_state: 0,
            };
            let mut count = 0u32;
            let ok = unsafe {
                read_console_w(
                    input_handle,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut count,
                    &mut control,
                )
            };
            if ok == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED) {
                    continue;
                }
                let _ = input_tx.send(ConsoleEvent::Error(format!("ReadConsoleW failed: {error}")));
                break;
            }
            let count = count as usize;
            if count == 0 {
                continue;
            }

            if let Some(tab_index) = buffer[..count]
                .iter()
                .position(|character| *character == TAB)
            {
                let prefix = String::from_utf16_lossy(&buffer[..tab_index]);
                let suffix = String::from_utf16_lossy(&buffer[tab_index + 1..count]);
                let mut line = prefix.clone();
                line.push_str(&suffix);

                if tab_index + 1 != count {
                    last_completion = None;
                    let wide: Vec<u16> = line.encode_utf16().collect();
                    if wide.len() >= buffer.len() {
                        let _ = input_tx
                            .send(ConsoleEvent::Error("console input line is too long".into()));
                        break;
                    }
                    buffer[..wide.len()].copy_from_slice(&wide);
                    keep = wide.len();
                    continue;
                }

                let requested_cursor = prefix.len();
                let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
                if input_tx
                    .send(ConsoleEvent::Complete {
                        line: line.clone(),
                        cursor: requested_cursor,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    break;
                }
                let completed = match reply_rx.recv() {
                    Ok(Some(completed)) => completed,
                    Ok(None) | Err(_) => break,
                };
                let completed_cursor = (completed.cursor as usize).min(completed.line.len());
                let show_candidates = completed.candidate_count > 1
                    && last_completion
                        .as_ref()
                        .is_some_and(|(last_line, last_cursor)| {
                            last_line == &line && *last_cursor == requested_cursor
                        });
                if show_candidates {
                    let width = crossterm::terminal::size().map(|size| size.0).unwrap_or(80);
                    let listing = format_completion_candidates(
                        &completed.candidates,
                        completed.candidate_count,
                        width,
                    );
                    let prompt = display
                        .prompt_tail
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                    let redraw = format!("\r\n{listing}\r\n{prompt}{}", completed.line);
                    if let Err(error) = write_windows_console(&redraw) {
                        let _ = input_tx.send(ConsoleEvent::Error(format!(
                            "write completion candidates to console failed: {error}"
                        )));
                        break;
                    }
                } else {
                    let new_prefix = completed
                        .line
                        .get(..completed_cursor)
                        .unwrap_or(&completed.line);
                    if let Some(addition) = new_prefix.strip_prefix(&prefix)
                        && let Err(error) = write_windows_console(addition)
                    {
                        let _ = input_tx.send(ConsoleEvent::Error(format!(
                            "write completion to console failed: {error}"
                        )));
                        break;
                    }
                }
                last_completion = (completed.candidate_count > 1)
                    .then(|| (completed.line.clone(), completed_cursor));

                let wide: Vec<u16> = completed.line.encode_utf16().collect();
                if wide.len() >= buffer.len() {
                    let _ = input_tx.send(ConsoleEvent::Error(
                        "completed console line is too long".into(),
                    ));
                    break;
                }
                buffer[..wide.len()].copy_from_slice(&wide);
                keep = wide.len();
                continue;
            }

            last_completion = None;
            let line = String::from_utf16_lossy(&buffer[..count]);
            if input_tx
                .send(ConsoleEvent::Input(normalize_console_line(line.as_bytes())))
                .is_err()
            {
                break;
            }
            keep = 0;
        }
    });

    spawn_line_resize_reader(stop, tx);
}

/// Convert the host console's line ending to one PTY newline.
///
/// Windows cooked console input ends submitted lines with CRLF. Forwarding
/// both bytes to an Android PTY with ICRNL enabled turns them into two newline
/// characters, so mksh executes an extra empty command and prints two prompts.
#[cfg(any(windows, test))]
fn normalize_console_line(line: &[u8]) -> Vec<u8> {
    let Some(without_lf) = line.strip_suffix(b"\n") else {
        return line.to_vec();
    };
    let content = without_lf.strip_suffix(b"\r").unwrap_or(without_lf);
    let mut normalized = Vec::with_capacity(content.len() + 1);
    normalized.extend_from_slice(content);
    normalized.push(b'\n');
    normalized
}

async fn open_pty_control(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
    echo: bool,
) -> Result<BufReader<TcpStream>> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let request = ControlRequest::Pty {
        device,
        program,
        args,
        run_as,
        cols,
        rows,
        term: "xterm-256color".into(),
        echo,
    };
    let mut stream = TcpStream::connect(control)
        .await
        .with_context(|| format!("connect to SideWire server control {control}"))?;
    stream.set_nodelay(true).context("enable TCP_NODELAY")?;
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        bail!("SideWire server closed PTY control connection");
    }
    match serde_json::from_str::<ControlResponse>(line.trim_end())? {
        ControlResponse::Ok { .. } => Ok(reader),
        ControlResponse::Error { message } => bail!(message),
        _ => bail!("unexpected PTY server response"),
    }
}

#[cfg(windows)]
async fn run_pty_line_client(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    program: String,
    args: Vec<String>,
) -> Result<()> {
    let reader = open_pty_control(control, device, run_as, program, args, false).await?;
    let stream_id = 0x5054_5901;
    let stop = Arc::new(AtomicBool::new(false));
    let display = Arc::new(LineDisplayState::default());
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_line_reader(stop.clone(), event_tx, display.clone());
    let (mut net_read, mut net_write) = tokio::io::split(reader);
    let (remote_tx, mut remote_rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let incoming = match read_frame(&mut net_read).await {
                Ok(frame) => Ok(frame),
                Err(error) => Err(error.to_string()),
            };
            let failed = incoming.is_err();
            if remote_tx.send(incoming).await.is_err() || failed {
                break;
            }
        }
    });
    let mut stdout = io::stdout();
    let mut completion_reply: Option<std::sync::mpsc::SyncSender<Option<PtyCompleteResult>>> = None;
    let result = loop {
        tokio::select! {
            event = event_rx.recv() => match event {
                Some(ConsoleEvent::Input(bytes)) => {
                    write_raw_frame(&mut net_write, FrameKind::PtyInput, stream_id, &bytes).await?;
                }
                Some(ConsoleEvent::Resize { cols, rows }) => {
                    let resize = frame(FrameKind::PtyResize, stream_id, &PtyResize { cols, rows })?;
                    write_frame(&mut net_write, &resize).await?;
                }
                Some(ConsoleEvent::Complete { line, cursor, reply }) => {
                    if completion_reply.is_some() {
                        let _ = reply.send(None);
                        continue;
                    }
                    let request = PtyCompleteRequest {
                        line,
                        cursor: cursor as u32,
                    };
                    write_frame(
                        &mut net_write,
                        &frame(FrameKind::PtyComplete, stream_id, &request)?,
                    )
                    .await?;
                    completion_reply = Some(reply);
                }
                Some(ConsoleEvent::Error(message)) => break Err(anyhow::anyhow!(message)),
                Some(ConsoleEvent::Eof) | None => {
                    let _ = write_raw_frame(&mut net_write, FrameKind::PtyClose, stream_id, &[]).await;
                    break Ok(());
                }
            },
            signal = tokio::signal::ctrl_c() => {
                signal.context("wait for Ctrl+C")?;
                write_raw_frame(&mut net_write, FrameKind::PtyInput, stream_id, &[0x03]).await?;
            }
            remote = remote_rx.recv() => match remote {
                Some(Ok(frame)) => match frame.kind {
                    FrameKind::PtyOutput => {
                        update_prompt_tail(&display, &frame.payload);
                        stdout.write_all(&frame.payload)?;
                        stdout.flush()?;
                    }
                    FrameKind::PtyCompleteResult => {
                        let completion: PtyCompleteResult = decode(&frame.payload)?;
                        if let Some(reply) = completion_reply.take() {
                            let _ = reply.send(Some(completion));
                        }
                    }
                    FrameKind::PtyExit => break Ok(()),
                    FrameKind::Error => break Err(anyhow::anyhow!("remote PTY error: {}", String::from_utf8_lossy(&frame.payload))),
                    kind => break Err(anyhow::anyhow!("unexpected PTY frame {kind:?}")),
                },
                Some(Err(message)) => break Err(anyhow::anyhow!(message)),
                None => break Err(anyhow::anyhow!("PTY network reader stopped")),
            }
        }
    };
    if let Some(reply) = completion_reply.take() {
        let _ = reply.send(None);
    }
    stop.store(true, Ordering::Relaxed);
    result
}

async fn run_pty_probe(control: &str, device: Option<String>, run_as: RunAs) -> Result<()> {
    let mut stream = open_pty_control(
        control,
        device,
        run_as,
        "/system/bin/sh".into(),
        Vec::new(),
        false,
    )
    .await?;
    let stream_id = 0x5054_5901;
    let command = b"printf '__SIDEWIRE_PTY_OK__\\n'; id; id -Z; tty; exit\r".to_vec();
    write_frame(
        &mut stream,
        &raw_frame(FrameKind::PtyInput, stream_id, command),
    )
    .await?;
    let probe = async {
        let mut output = Vec::new();
        loop {
            let frame = read_frame(&mut stream).await?;
            match frame.kind {
                FrameKind::PtyOutput => {
                    io::stdout().write_all(&frame.payload)?;
                    io::stdout().flush()?;
                    output.extend_from_slice(&frame.payload);
                }
                FrameKind::PtyExit => break,
                FrameKind::Error => bail!(
                    "remote PTY error: {}",
                    String::from_utf8_lossy(&frame.payload)
                ),
                kind => bail!("unexpected PTY frame {kind:?}"),
            }
        }
        if !output
            .windows(b"__SIDEWIRE_PTY_OK__".len())
            .any(|w| w == b"__SIDEWIRE_PTY_OK__")
        {
            bail!("PTY probe completed without input marker");
        }
        Ok::<(), anyhow::Error>(())
    };
    match tokio::time::timeout(tokio::time::Duration::from_secs(6), probe).await {
        Ok(result) => result,
        Err(_) => bail!("PTY probe timed out: remote shell did not consume synthetic input"),
    }
}

pub(super) async fn run_shell(
    control: &str,
    device: Option<String>,
    run_as: RunAs,
    raw: bool,
    probe: bool,
) -> Result<()> {
    if probe {
        return run_pty_probe(control, device, run_as).await;
    }
    if raw {
        return run_pty_client(control, device, run_as, "/system/bin/sh".into(), Vec::new()).await;
    }
    #[cfg(not(windows))]
    {
        // Unix terminals already provide a good raw PTY experience, so keep the
        // remote shell's native editor/history/completion enabled by default.
        run_pty_client(control, device, run_as, "/system/bin/sh".into(), Vec::new()).await
    }
    #[cfg(windows)]
    {
        // The Windows console already echoes and edits cooked line input. Turn
        // off mksh's remote editor so the submitted command is not drawn a
        // second time; SideWire handles Tab completion locally in this mode.
        run_pty_line_client(
            control,
            device,
            run_as,
            "/system/bin/sh".into(),
            vec!["+o".into(), "emacs".into()],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{format_completion_candidates, normalize_console_line};

    #[test]
    fn normalizes_windows_console_crlf_to_one_pty_enter() {
        assert_eq!(normalize_console_line(b"ls\r\n"), b"ls\n");
        assert_eq!(normalize_console_line(b"\r\n"), b"\n");
    }

    #[test]
    fn normalizes_unix_console_lf_to_one_pty_enter() {
        assert_eq!(normalize_console_line(b"pwd\n"), b"pwd\n");
        assert_eq!(normalize_console_line(b"\n"), b"\n");
    }

    #[test]
    fn leaves_unterminated_input_unchanged() {
        assert_eq!(normalize_console_line(b"exit"), b"exit");
    }

    #[test]
    fn formats_completion_candidates_in_columns_and_reports_truncation() {
        let candidates = vec!["alpha".into(), "beta".into(), "gamma".into()];
        let output = format_completion_candidates(&candidates, 5, 16);
        assert!(output.contains("alpha"));
        assert!(output.contains("beta"));
        assert!(output.contains("gamma"));
        assert!(output.contains("... 2 more"));
    }
}
