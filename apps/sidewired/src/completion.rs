#[cfg(target_os = "android")]
use anyhow::{Context, Result};
#[cfg(target_os = "android")]
use sidewire_protocol::{PtyCompleteRequest, PtyCompleteResult};
#[cfg(target_os = "android")]
use std::fs;

#[cfg(any(target_os = "android", test))]
fn completion_token_start(line: &str, cursor: usize) -> usize {
    let mut start = 0usize;
    let mut quote = None;
    for (index, character) in line[..cursor].char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => {
                quote = Some(character);
                if start == index {
                    start = index + character.len_utf8();
                }
            }
            None if character.is_whitespace() || "|;&()<>".contains(character) => {
                start = index + character.len_utf8();
            }
            None => {}
        }
    }
    start
}

#[cfg(any(target_os = "android", test))]
fn common_completion_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = first.clone();
    for value in &values[1..] {
        while !value.starts_with(&prefix) {
            if prefix.pop().is_none() {
                break;
            }
        }
        if prefix.is_empty() {
            break;
        }
    }
    prefix
}

#[cfg(target_os = "android")]
pub(super) fn complete_pty_path(
    pid: u32,
    request: &PtyCompleteRequest,
) -> Result<PtyCompleteResult> {
    let mut cursor = (request.cursor as usize).min(request.line.len());
    while cursor > 0 && !request.line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = completion_token_start(&request.line, cursor);
    let typed = &request.line[start..cursor];
    let split = typed.rfind('/').map(|index| index + 1).unwrap_or(0);
    let (directory_text, needle) = typed.split_at(split);

    let cwd = fs::read_link(format!("/proc/{pid}/cwd"))
        .with_context(|| format!("read PTY cwd for pid {pid}"))?;
    let search_dir = if directory_text.starts_with('/') {
        std::path::PathBuf::from(if directory_text.is_empty() {
            "/"
        } else {
            directory_text
        })
    } else if directory_text.is_empty() {
        cwd
    } else {
        cwd.join(directory_text)
    };

    let mut candidates = Vec::new();
    for entry in fs::read_dir(&search_dir)
        .with_context(|| format!("read completion directory {}", search_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(needle) {
            continue;
        }
        let mut candidate = format!("{directory_text}{name}");
        if entry
            .metadata()
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
        {
            candidate.push('/');
        }
        candidates.push(candidate);
    }
    candidates.sort();

    let replacement = match candidates.len() {
        0 => typed.to_owned(),
        1 => candidates.remove(0),
        _ => {
            let prefix = common_completion_prefix(&candidates);
            if prefix.len() > typed.len() {
                prefix
            } else {
                typed.to_owned()
            }
        }
    };

    let mut line = request.line.clone();
    line.replace_range(start..cursor, &replacement);
    let cursor = start + replacement.len();
    Ok(PtyCompleteResult {
        line,
        cursor: cursor as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::{common_completion_prefix, completion_token_start};

    #[test]
    fn finds_path_token_after_shell_separator() {
        let line = "echo ok && cd /sys/cl";
        assert_eq!(completion_token_start(line, line.len()), 14);
    }

    #[test]
    fn skips_opening_quote_in_path_token() {
        let line = "cat \"foo bar/ba";
        assert_eq!(completion_token_start(line, line.len()), 5);
    }

    #[test]
    fn finds_common_candidate_prefix() {
        let values = vec!["class/".into(), "classes.dex".into()];
        assert_eq!(common_completion_prefix(&values), "class");
    }
}
