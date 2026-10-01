//! Test harness for mux: an independent reference terminal, a generator of
//! realistic terminal output, and a rig that drives the real `mux` binary
//! through pseudo-terminals.

pub mod generate;
pub mod reference;
pub mod rig;
pub mod screen;

/// Shrinks `tokens` to a smaller list that still makes `fails` true, by
/// removing chunks and then single tokens until nothing more can go.
pub fn shrink(mut tokens: Vec<Vec<u8>>, fails: impl Fn(&[Vec<u8>]) -> bool) -> Vec<Vec<u8>> {
    let mut chunk = tokens.len() / 2;
    while chunk >= 1 {
        let mut start = 0;
        while start < tokens.len() {
            let end = (start + chunk).min(tokens.len());
            let mut candidate = tokens.clone();
            candidate.drain(start..end);
            if fails(&candidate) {
                tokens = candidate;
            } else {
                start += chunk;
            }
        }
        chunk /= 2;
    }
    tokens
}

/// Escapes bytes for a readable failure report.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        for character in chunk.valid().chars() {
            match character {
                '\x1b' => out.push_str("\\e"),
                '\r' => out.push_str("\\r"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
                c => out.push(c),
            }
        }
        for byte in chunk.invalid() {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out
}
