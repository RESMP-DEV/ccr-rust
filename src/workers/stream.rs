use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;
const READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Default, Serialize)]
pub struct ScanStats {
    pub next_cursor: u64,
    pub file_bytes: u64,
    pub malformed_lines: u64,
    pub oversized_lines: u64,
    pub partial_line: bool,
}

pub fn scan(
    path: &Path,
    after: u64,
    mut visit: impl FnMut(u64, &Value) -> bool,
) -> Result<ScanStats> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open worker stream {}", path.display()))?;
    let file_bytes = file
        .metadata()
        .with_context(|| format!("failed to inspect worker stream {}", path.display()))?
        .len();

    if after > file_bytes {
        bail!(
            "cursor {} exceeds worker stream snapshot size {}",
            after,
            file_bytes
        );
    }
    if after > 0 {
        let mut terminator = [0_u8];
        file.seek(SeekFrom::Start(after - 1))?;
        file.read_exact(&mut terminator)?;
        if terminator[0] != b'\n' {
            bail!("cursor {} is not immediately after LF", after);
        }
        file.seek(SeekFrom::Start(after))?;
    }

    let mut stats = ScanStats {
        next_cursor: after,
        file_bytes,
        ..ScanStats::default()
    };
    let mut reader = BufReader::with_capacity(READ_CHUNK_BYTES, file.take(file_bytes - after));
    let mut position = after;
    let mut record = Vec::new();
    let mut record_oversized = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if position < file_bytes {
                bail!("worker stream was truncated during inspection; restart from cursor 0");
            }
            stats.partial_line = position > stats.next_cursor;
            return Ok(stats);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        let consumed = content_len + usize::from(newline.is_some());
        if !record_oversized {
            if record.len() + content_len > MAX_RECORD_BYTES {
                record_oversized = true;
                record.clear();
            } else {
                record.extend_from_slice(&available[..content_len]);
            }
        }
        reader.consume(consumed);
        position += consumed as u64;
        if newline.is_none() {
            continue;
        }
        stats.next_cursor = position;
        if record_oversized {
            stats.oversized_lines += 1;
        } else {
            match serde_json::from_slice::<Value>(&record) {
                Ok(value) if value.is_object() => {
                    if !visit(position, &value) {
                        return Ok(stats);
                    }
                }
                _ => {
                    stats.malformed_lines += 1;
                }
            }
        }
        record.clear();
        record_oversized = false;
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;
