//! Machines, as binaries are built for them: this one, and the one a binary's header
//! names, so that oxploy does not try to start a binary built for another one.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// A processor and an operating system, such as x86_64 Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    /// The processor, as Rust names it, such as `x86_64`, or `universal` for a macOS
    /// binary built for several.
    pub arch: &'static str,
    /// The operating system, as people name it, such as `Linux`.
    pub os: &'static str,
}

impl Machine {
    /// The machine oxploy runs on.
    pub fn this() -> Machine {
        let os = match std::env::consts::OS {
            "linux" => "Linux",
            "macos" => "macOS",
            other => other,
        };
        Machine {
            arch: std::env::consts::ARCH,
            os,
        }
    }

    /// The machine the binary at `path` is built for, read from its header. `None` for a
    /// file that is not a binary oxploy knows, such as a script, which may run anywhere.
    pub fn of_binary(path: &Path) -> io::Result<Option<Machine>> {
        let mut header = [0; 20];
        let read = File::open(path)?.read(&mut header)?;
        Ok(Machine::from_header(&header[..read]))
    }

    fn from_header(header: &[u8]) -> Option<Machine> {
        let machine = |arch| Some(Machine { arch, os: "Linux" });
        match header {
            // ELF, little-endian, with its machine at byte 18.
            [0x7f, b'E', b'L', b'F', _, 1, ..] if header.len() >= 20 => {
                match u16::from_le_bytes([header[18], header[19]]) {
                    0x3e => machine("x86_64"),
                    0xb7 => machine("aarch64"),
                    0x03 => machine("x86"),
                    0x28 => machine("arm"),
                    0xf3 => machine("riscv64"),
                    _ => None,
                }
            }
            // A 64-bit Mach-O, little-endian, with its processor type at byte 4.
            [0xcf, 0xfa, 0xed, 0xfe, a, b, c, d, ..] => {
                let arch = match u32::from_le_bytes([*a, *b, *c, *d]) {
                    0x0100_0007 => "x86_64",
                    0x0100_000c => "aarch64",
                    _ => return None,
                };
                Some(Machine { arch, os: "macOS" })
            }
            // A universal macOS binary, with a build for each of several processors.
            [0xca, 0xfe, 0xba, 0xbe, ..] => Some(Machine {
                arch: "universal",
                os: "macOS",
            }),
            _ => None,
        }
    }

    /// Whether a binary built for this machine runs on `other`.
    pub fn runs_on(self, other: Machine) -> bool {
        self.os == other.os && (self.arch == other.arch || self.arch == "universal")
    }
}

impl fmt::Display for Machine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.arch, self.os)
    }
}

/// Whether a release's file is named for this machine, such as `api-x86_64-linux.tar.gz`
/// on x86_64 Linux.
pub fn named_for_this_machine(name: &str) -> bool {
    let arch: &[&str] = match std::env::consts::ARCH {
        "x86_64" => &["x86_64", "amd64"],
        "aarch64" => &["aarch64", "arm64"],
        other => &[other][..],
    };
    let os: &[&str] = match std::env::consts::OS {
        "macos" => &["macos", "darwin", "apple"],
        other => &[other][..],
    };
    let name = name.to_ascii_lowercase();
    arch.iter().any(|arch| name.contains(arch)) && os.iter().any(|os| name.contains(os))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_name_their_machine() {
        let mut elf = [0; 20];
        elf[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        elf[18] = 0x3e;
        let linux = Machine::from_header(&elf).unwrap();
        assert_eq!(linux.to_string(), "x86_64 Linux");

        let mac = Machine::from_header(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01]).unwrap();
        assert_eq!(mac.to_string(), "aarch64 macOS");
        assert!(!linux.runs_on(mac));
        assert!(mac.runs_on(mac));

        let universal = Machine::from_header(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]).unwrap();
        assert!(universal.runs_on(mac));
        assert_eq!(Machine::from_header(b"#!/bin/sh\necho hi\n"), None);
    }

    #[test]
    fn this_machine_runs_its_own_binaries() {
        let this = Machine::this();
        assert!(this.runs_on(this));
        let own = Machine::of_binary(&std::env::current_exe().unwrap()).unwrap();
        assert!(own.is_some_and(|own| own.runs_on(this)));
    }
}
