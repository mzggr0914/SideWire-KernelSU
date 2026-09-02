use std::io::{self, Write};

#[derive(Clone, Copy)]
enum OutputTarget {
    Stdout,
    Stderr,
}

pub(super) struct StreamOutput {
    target: OutputTarget,
    #[cfg(windows)]
    console: bool,
    #[cfg(windows)]
    decoder: Utf8ConsoleDecoder,
}

impl StreamOutput {
    pub(super) fn stdout() -> Self {
        Self::new(OutputTarget::Stdout)
    }

    pub(super) fn stderr() -> Self {
        Self::new(OutputTarget::Stderr)
    }

    fn new(target: OutputTarget) -> Self {
        Self {
            target,
            #[cfg(windows)]
            console: windows_console_handle(target).is_some(),
            #[cfg(windows)]
            decoder: Utf8ConsoleDecoder::default(),
        }
    }

    pub(super) fn write_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(windows)]
        if self.console {
            let target = self.target;
            return self
                .decoder
                .push(bytes, |text| write_windows_console(target, text));
        }
        self.write_raw(bytes)
    }

    pub(super) fn finish(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        if self.console {
            let target = self.target;
            return self
                .decoder
                .finish(|text| write_windows_console(target, text));
        }
        self.flush_raw()
    }

    fn write_raw(&self, bytes: &[u8]) -> io::Result<()> {
        match self.target {
            OutputTarget::Stdout => {
                let mut output = io::stdout().lock();
                output.write_all(bytes)?;
                output.flush()
            }
            OutputTarget::Stderr => {
                let mut output = io::stderr().lock();
                output.write_all(bytes)?;
                output.flush()
            }
        }
    }

    fn flush_raw(&self) -> io::Result<()> {
        match self.target {
            OutputTarget::Stdout => io::stdout().lock().flush(),
            OutputTarget::Stderr => io::stderr().lock().flush(),
        }
    }
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct Utf8ConsoleDecoder {
    pending: Vec<u8>,
}

#[cfg(any(windows, test))]
impl Utf8ConsoleDecoder {
    fn push<F>(&mut self, bytes: &[u8], mut emit: F) -> io::Result<()>
    where
        F: FnMut(&str) -> io::Result<()>,
    {
        self.pending.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    if !text.is_empty() {
                        emit(text)?;
                    }
                    self.pending.clear();
                    return Ok(());
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        let text = std::str::from_utf8(&self.pending[..valid_up_to])
                            .expect("validated UTF-8 prefix");
                        emit(text)?;
                        self.pending.drain(..valid_up_to);
                    }
                    match error.error_len() {
                        Some(length) => {
                            emit("�")?;
                            self.pending.drain(..length);
                        }
                        None => return Ok(()),
                    }
                }
            }
        }
    }

    fn finish<F>(&mut self, mut emit: F) -> io::Result<()>
    where
        F: FnMut(&str) -> io::Result<()>,
    {
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&self.pending).into_owned();
            emit(&text)?;
            self.pending.clear();
        }
        Ok(())
    }
}

#[cfg(windows)]
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
#[cfg(windows)]
const STD_ERROR_HANDLE: u32 = -12i32 as u32;

#[cfg(windows)]
#[link(name = "Kernel32")]
unsafe extern "system" {
    #[link_name = "GetStdHandle"]
    fn get_std_handle(std_handle: u32) -> *mut std::ffi::c_void;
    #[link_name = "GetConsoleMode"]
    fn get_console_mode(console: *mut std::ffi::c_void, mode: *mut u32) -> i32;
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
fn windows_console_handle(target: OutputTarget) -> Option<*mut std::ffi::c_void> {
    let kind = match target {
        OutputTarget::Stdout => STD_OUTPUT_HANDLE,
        OutputTarget::Stderr => STD_ERROR_HANDLE,
    };
    let handle = unsafe { get_std_handle(kind) };
    let mut mode = 0u32;
    (unsafe { get_console_mode(handle, &mut mode) } != 0).then_some(handle)
}

#[cfg(windows)]
fn write_windows_console(target: OutputTarget, text: &str) -> io::Result<()> {
    let handle = windows_console_handle(target)
        .ok_or_else(|| io::Error::other("output is not a Windows console"))?;
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut offset = 0usize;
    while offset < wide.len() {
        let count = (wide.len() - offset).min(u32::MAX as usize) as u32;
        let mut written = 0u32;
        let ok = unsafe {
            write_console_w(
                handle,
                wide[offset..].as_ptr().cast(),
                count,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "WriteConsoleW wrote zero chars",
            ));
        }
        offset += written as usize;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_utf8_split_across_chunks() {
        let mut decoder = Utf8ConsoleDecoder::default();
        let mut output = String::new();
        decoder
            .push(b"ab\xe2\x82", |text| {
                output.push_str(text);
                Ok(())
            })
            .unwrap();
        decoder
            .push(b"\xaccd", |text| {
                output.push_str(text);
                Ok(())
            })
            .unwrap();
        assert_eq!(output, "ab€cd");
    }

    #[test]
    fn replaces_invalid_console_bytes_and_continues() {
        let mut decoder = Utf8ConsoleDecoder::default();
        let mut output = String::new();
        decoder
            .push(b"a\xffb", |text| {
                output.push_str(text);
                Ok(())
            })
            .unwrap();
        assert_eq!(output, "a�b");
    }

    #[test]
    fn finishes_incomplete_utf8_lossily() {
        let mut decoder = Utf8ConsoleDecoder::default();
        let mut output = String::new();
        decoder
            .push(b"a\xe2\x82", |text| {
                output.push_str(text);
                Ok(())
            })
            .unwrap();
        decoder
            .finish(|text| {
                output.push_str(text);
                Ok(())
            })
            .unwrap();
        assert_eq!(output, "a�");
    }
}
