//! Panic backtrace symbolication (DEV-03): resolve the `Backtrace: PC:SP ...` line ESP-IDF's
//! panic handler prints to function, file and line from the firmware's ELF files (DWARF,
//! inlined frames included). ESP-IDF already turns return addresses into call-site addresses,
//! so each PC is looked up as printed.

use std::path::{Path, PathBuf};

pub struct Symbolizer {
    elves: Vec<(PathBuf, addr2line::Loader)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub pc: u32,
    pub function: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub inlined: bool,
}

impl Symbolizer {
    pub fn open(paths: &[PathBuf]) -> Result<Symbolizer, String> {
        let mut elves = Vec::new();
        for p in paths {
            let loader = addr2line::Loader::new(p).map_err(|e| format!("{}: {e}", p.display()))?;
            elves.push((p.clone(), loader));
        }
        Ok(Symbolizer { elves })
    }

    pub fn is_empty(&self) -> bool {
        self.elves.is_empty()
    }

    /// Frames for `pc`, innermost (inlined) first. Empty when no ELF covers it.
    pub fn resolve(&self, pc: u32) -> Vec<Frame> {
        for (_, loader) in &self.elves {
            let mut out = Vec::new();
            if let Ok(mut frames) = loader.find_frames(pc as u64) {
                while let Ok(Some(f)) = frames.next() {
                    let function = f.function.as_ref().and_then(|n| n.demangle().ok()).map(|n| n.into_owned());
                    let (file, line) = match &f.location {
                        Some(l) => (l.file.map(str::to_owned), l.line),
                        None => (None, None),
                    };
                    out.push(Frame { pc, function, file, line, inlined: false });
                }
            }
            if out.is_empty() {
                if let Some(sym) = loader.find_symbol(pc as u64) {
                    out.push(Frame { pc, function: Some(sym.to_string()), file: None, line: None, inlined: false });
                }
            }
            if !out.is_empty() {
                let n = out.len();
                for f in &mut out[..n - 1] {
                    f.inlined = true;
                }
                return out;
            }
        }
        Vec::new()
    }

    /// For a console line holding an ESP-IDF backtrace, the resolved frames as display lines.
    pub fn annotate(&self, line: &str) -> Option<Vec<String>> {
        let rest = &line[line.find("Backtrace:")? + "Backtrace:".len()..];
        let mut out = Vec::new();
        for (depth, token) in rest.split_whitespace().enumerate() {
            let pc_text = token.split(':').next()?;
            let pc = u32::from_str_radix(pc_text.trim_start_matches("0x"), 16).ok()?;
            let frames = self.resolve(pc);
            if frames.is_empty() {
                out.push(format!("  #{depth} {pc:#010x} ??"));
            }
            for f in frames {
                let func = f.function.as_deref().unwrap_or("??");
                let loc = match (&f.file, f.line) {
                    (Some(file), Some(line)) => format!(" at {}:{line}", short(file)),
                    (Some(file), None) => format!(" at {}", short(file)),
                    _ => String::new(),
                };
                let tag = if f.inlined { " (inlined)" } else { "" };
                out.push(format!("  #{depth} {pc:#010x} {func}{loc}{tag}"));
            }
        }
        Some(out)
    }
}

/// Paths are shown relative to the last `components/` or `main/` so logs stay readable.
fn short(path: &str) -> String {
    let p = Path::new(path);
    let comps: Vec<_> = p.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    match comps.iter().rposition(|c| c == "main" || c == "components") {
        Some(i) => comps[i..].join("/"),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_keep_the_component_tail() {
        assert_eq!(short("/x/y/fixtures-src/diag-panic/main/main.c"), "main/main.c");
        assert_eq!(short("/idf/components/freertos/app_startup.c"), "components/freertos/app_startup.c");
        assert_eq!(short("rel.c"), "rel.c");
    }

    #[test]
    fn non_backtrace_lines_are_ignored() {
        let s = Symbolizer { elves: Vec::new() };
        assert!(s.annotate("I (10) boot: hello").is_none());
        assert_eq!(s.annotate("Backtrace: 0x4200a40d:0x3fcea830").unwrap(), ["  #0 0x4200a40d ??"]);
    }
}
