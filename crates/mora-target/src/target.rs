//! A target description and the rules it must follow (§19.4).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::builtin;
use crate::error::Error;

/// One target description, as written in its TOML file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Target {
    /// The name used by `--target` and in `lib/rt/<name>/`, such as `x86_64-linux`.
    pub name: String,
    /// The LLVM-style triple the backend and the linker receive.
    pub triple: String,
    /// The `Os` value of `[Cfg]` (§17.2).
    pub os: String,
    /// The `Arch` value of `[Cfg]` (§17.2).
    pub arch: String,
    /// Pointer width in bits: 32 or 64.
    pub pointer_width: u32,
    /// Byte order.
    pub endian: Endian,
    /// The object file format the backend writes.
    pub object_format: ObjectFormat,
    /// The C calling convention Mora functions use (§13.7).
    pub call_conv: CallConv,
    /// The code generator.
    pub backend: Backend,
    /// The runtime library linked into executables.
    pub runtime: Runtime,
    /// The platform capabilities the target offers (§19.4 rule 4).
    pub capabilities: Vec<Capability>,
    /// Alignments of the primitive types.
    pub align: Align,
    /// How executables are linked.
    pub linker: Linker,
}

/// Byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

/// An object file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObjectFormat {
    /// ELF, for Linux and bare metal.
    Elf,
    /// Mach-O, for macOS.
    Macho,
    /// COFF, for Windows.
    Coff,
    /// WebAssembly.
    Wasm,
}

/// A C calling convention (§13.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallConv {
    /// System V AMD64.
    Sysv64,
    /// AAPCS64.
    Aapcs64,
    /// Apple's arm64 variant of AAPCS64.
    AppleAapcs64,
    /// Microsoft x64.
    Win64,
    /// Microsoft's arm64 convention.
    WinArm64,
    /// The RISC-V LP64D convention.
    Lp64d,
    /// The 32-bit Arm AAPCS.
    Aapcs,
}

/// A code generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// Cranelift (§14.13).
    Cranelift,
}

/// The runtime library linked into executables (§19.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// `libmora_rt.a`: the hosted platform layer with `mora-rt-core` bundled.
    Hosted,
    /// `libmora_rt_core.a` alone, for a platform layer supplied with the program.
    Core,
}

/// A platform capability (§19.4 rule 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// A heap allocator.
    Heap,
    /// Operating system threads.
    Threads,
    /// A file system.
    Fs,
    /// Sockets.
    Net,
    /// Standard input, output and error.
    Stdio,
    /// A monotonic clock.
    Clock,
    /// A source of random bytes.
    Random,
    /// Processes and exit codes.
    Process,
    /// Environment variables and arguments.
    Env,
}

/// Alignments of the primitive types, in bytes. One-byte types are always
/// aligned to one byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Align {
    /// `short` and `ushort`.
    pub i16: u32,
    /// `int`, `uint` and `char`.
    pub i32: u32,
    /// `long` and `ulong`.
    pub i64: u32,
    /// `float`.
    pub f32: u32,
    /// `double`.
    pub f64: u32,
    /// Pointers, `nint` and `nuint`.
    pub pointer: u32,
}

/// How executables are linked (§14.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Linker {
    /// Which linker, by the name of the program.
    pub flavor: LinkerFlavor,
    /// Arguments passed before the objects and libraries.
    pub args: Vec<String>,
}

/// A linker, by the name it is run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkerFlavor {
    /// `ld.lld`, for ELF.
    #[serde(rename = "ld.lld")]
    LdLld,
    /// The macOS system linker, run as `xcrun ld`.
    #[serde(rename = "ld64")]
    Ld64,
    /// `lld-link`, for COFF.
    #[serde(rename = "lld-link")]
    LldLink,
    /// `wasm-ld`, for WebAssembly.
    #[serde(rename = "wasm-ld")]
    WasmLd,
}

impl LinkerFlavor {
    /// The object format this linker reads.
    pub fn object_format(self) -> ObjectFormat {
        match self {
            LinkerFlavor::LdLld => ObjectFormat::Elf,
            LinkerFlavor::Ld64 => ObjectFormat::Macho,
            LinkerFlavor::LldLink => ObjectFormat::Coff,
            LinkerFlavor::WasmLd => ObjectFormat::Wasm,
        }
    }
}

impl Target {
    /// The built-in target called `name`.
    pub fn builtin(name: &str) -> Result<Target, Error> {
        let Some(text) = builtin::text(name) else {
            return Err(Error::Unknown(name.to_owned()));
        };
        Target::parse(text).map_err(|message| Error::Invalid {
            origin: format!("built-in target `{name}`"),
            message,
        })
    }

    /// Reads a description from a file.
    pub fn load(path: &Path) -> Result<Target, Error> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        Target::parse(&text).map_err(|message| Error::Invalid {
            origin: path.display().to_string(),
            message,
        })
    }

    /// Resolves a `--target` argument: a path when it ends in `.toml`,
    /// otherwise the name of a built-in target.
    pub fn resolve(arg: &str) -> Result<Target, Error> {
        if arg.ends_with(".toml") {
            Target::load(Path::new(arg))
        } else {
            Target::builtin(arg)
        }
    }

    /// Parses and checks a description.
    pub fn parse(text: &str) -> Result<Target, String> {
        let target: Target = toml::from_str(text).map_err(|err| err.to_string())?;
        target.validate()?;
        Ok(target)
    }

    /// The `[Cfg]` conditions this target sets (§17.2).
    pub fn cfg(&self) -> [(&'static str, &str); 2] {
        [("Os", &self.os), ("Arch", &self.arch)]
    }

    /// The size of a pointer in bytes.
    pub fn pointer_bytes(&self) -> u32 {
        self.pointer_width / 8
    }

    /// Whether the target offers `capability`.
    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    fn validate(&self) -> Result<(), String> {
        if !is_name(&self.name, &['-', '_']) {
            return Err(format!(
                "`name` must be lowercase letters, digits, `-` and `_`, not `{}`",
                self.name
            ));
        }
        for (key, value) in [("os", &self.os), ("arch", &self.arch)] {
            if !is_name(value, &['_']) {
                return Err(format!(
                    "`{key}` must be lowercase letters, digits and `_`, not `{value}`"
                ));
            }
        }
        if self.triple.is_empty() {
            return Err("`triple` is empty".to_owned());
        }
        if !matches!(self.pointer_width, 32 | 64) {
            return Err(format!(
                "`pointer-width` must be 32 or 64, not {}",
                self.pointer_width
            ));
        }
        let align = &self.align;
        for (key, value, size) in [
            ("i16", align.i16, 2),
            ("i32", align.i32, 4),
            ("i64", align.i64, 8),
            ("f32", align.f32, 4),
            ("f64", align.f64, 8),
            ("pointer", align.pointer, self.pointer_bytes()),
        ] {
            if !value.is_power_of_two() || value > size {
                return Err(format!(
                    "`align.{key}` must be a power of two no larger than {size}, not {value}"
                ));
            }
        }
        for (i, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..i].contains(capability) {
                return Err(format!("capability {capability:?} is listed twice"));
            }
        }
        let reads = self.linker.flavor.object_format();
        if reads != self.object_format {
            return Err(format!(
                "linker {:?} reads {reads:?} objects, but `object-format` is {:?}",
                self.linker.flavor, self.object_format
            ));
        }
        Ok(())
    }
}

/// Whether `s` is non-empty and made of lowercase ASCII letters, digits and `extra`.
fn is_name(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}
