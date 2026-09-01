#[cfg(target_os = "android")]
use anyhow::{Context, Result};
#[cfg(target_os = "android")]
use sidewire_protocol::{PtyCompleteRequest, PtyCompleteResult};
#[cfg(target_os = "android")]
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[cfg(target_os = "android")]
#[derive(Clone)]
struct CachedDirEntry {
    name: String,
    is_dir: bool,
}

#[cfg(target_os = "android")]
struct CachedDirectory {
    loaded_at: Instant,
    entries: Arc<Vec<CachedDirEntry>>,
}

#[cfg(target_os = "android")]
static DIRECTORY_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedDirectory>>> = OnceLock::new();
#[cfg(target_os = "android")]
const DIRECTORY_CACHE_TTL: Duration = Duration::from_millis(750);
#[cfg(target_os = "android")]
const DIRECTORY_CACHE_LIMIT: usize = 64;

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
fn shrink_common_prefix(prefix: &mut String, value: &str) {
    while !value.starts_with(prefix.as_str()) {
        if prefix.pop().is_none() {
            break;
        }
    }
}

#[cfg(test)]
fn common_completion_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = first.clone();
    for value in &values[1..] {
        shrink_common_prefix(&mut prefix, value);
        if prefix.is_empty() {
            break;
        }
    }
    prefix
}

#[cfg(target_os = "android")]
fn completion_entries(path: &Path) -> Result<Arc<Vec<CachedDirEntry>>> {
    let cache = DIRECTORY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        let cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cache.get(path)
            && cached.loaded_at.elapsed() <= DIRECTORY_CACHE_TTL
        {
            return Ok(cached.entries.clone());
        }
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path)
        .with_context(|| format!("read completion directory {}", path.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let is_dir = file_type.is_dir()
            || (file_type.is_symlink()
                && entry
                    .metadata()
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false));
        entries.push(CachedDirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }

    let entries = Arc::new(entries);
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= DIRECTORY_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(
        path.to_path_buf(),
        CachedDirectory {
            loaded_at: Instant::now(),
            entries: entries.clone(),
        },
    );
    Ok(entries)
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

    let mut matches = 0usize;
    let mut first = None;
    let mut prefix = String::new();
    let entries = completion_entries(&search_dir)?;
    for entry in entries.iter() {
        if !entry.name.starts_with(needle) {
            continue;
        }
        let mut candidate = format!("{directory_text}{}", entry.name);
        if entry.is_dir {
            candidate.push('/');
        }

        matches += 1;
        if matches == 1 {
            prefix.clone_from(&candidate);
            first = Some(candidate);
        } else {
            shrink_common_prefix(&mut prefix, &candidate);
        }
    }

    let replacement = match matches {
        0 => typed.to_owned(),
        1 => first.unwrap_or_else(|| typed.to_owned()),
        _ if prefix.len() > typed.len() => prefix,
        _ => typed.to_owned(),
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
