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

impl ObjectFormat {
    /// The value's spelling in a description file.
    fn name(self) -> &'static str {
        match self {
            ObjectFormat::Elf => "elf",
            ObjectFormat::Macho => "macho",
            ObjectFormat::Coff => "coff",
            ObjectFormat::Wasm => "wasm",
        }
    }
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

impl CallConv {
    /// The value's spelling in a description file.
    fn name(self) -> &'static str {
        match self {
            CallConv::Sysv64 => "sysv64",
            CallConv::Aapcs64 => "aapcs64",
            CallConv::AppleAapcs64 => "apple-aapcs64",
            CallConv::Win64 => "win64",
            CallConv::WinArm64 => "win-arm64",
            CallConv::Lp64d => "lp64d",
            CallConv::Aapcs => "aapcs",
        }
    }

    /// The width, in bits, of the pointers this convention passes.
    fn pointer_width(self) -> u32 {
        match self {
            CallConv::Aapcs => 32,
            CallConv::Sysv64
            | CallConv::Aapcs64
            | CallConv::AppleAapcs64
            | CallConv::Win64
            | CallConv::WinArm64
            | CallConv::Lp64d => 64,
        }
    }

    /// Whether this is a convention of the instruction set `arch`: `x86_64`,
    /// `aarch64` and `riscv64` by name, and 32-bit Arm by an `arch` that
    /// starts with `arm`, but not `arm64`, or with `thumb`.
    fn fits_arch(self, arch: &str) -> bool {
        match self {
            CallConv::Sysv64 | CallConv::Win64 => arch == "x86_64",
            CallConv::Aapcs64 | CallConv::AppleAapcs64 | CallConv::WinArm64 => arch == "aarch64",
            CallConv::Lp64d => arch == "riscv64",
            CallConv::Aapcs => {
                (arch.starts_with("arm") && !arch.starts_with("arm64")) || arch.starts_with("thumb")
            }
        }
    }
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

impl Capability {
    /// The value's spelling in a description file.
    fn name(self) -> &'static str {
        match self {
            Capability::Heap => "heap",
            Capability::Threads => "threads",
            Capability::Fs => "fs",
            Capability::Net => "net",
            Capability::Stdio => "stdio",
            Capability::Clock => "clock",
            Capability::Random => "random",
            Capability::Process => "process",
            Capability::Env => "env",
        }
    }
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
    /// The value's spelling in a description file.
    fn name(self) -> &'static str {
        match self {
            LinkerFlavor::LdLld => "ld.lld",
            LinkerFlavor::Ld64 => "ld64",
            LinkerFlavor::LldLink => "lld-link",
            LinkerFlavor::WasmLd => "wasm-ld",
        }
    }

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
        Target::parse_from(text, || format!("built-in target `{name}`"))
    }

    /// The built-in target whose `arch` and `os` are the arguments.
    pub fn for_platform(arch: &str, os: &str) -> Result<Target, Error> {
        for name in builtin::builtin_names() {
            let target = Target::builtin(name)?;
            if target.arch == arch && target.os == os {
                return Ok(target);
            }
        }
        Err(Error::Unknown(format!("{arch}-{os}")))
    }

    /// The built-in target of the machine this process runs on.
    pub fn host() -> Result<Target, Error> {
        Target::for_platform(std::env::consts::ARCH, std::env::consts::OS)
    }

    /// Reads a description from a file.
    pub fn load(path: &Path) -> Result<Target, Error> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        Target::parse_from(&text, || path.display().to_string())
    }

    /// Parses `text`, naming `origin` in the error.
    fn parse_from(text: &str, origin: impl FnOnce() -> String) -> Result<Target, Error> {
        Target::parse(text).map_err(|message| Error::Invalid {
            origin: origin(),
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
        // Neither `name` nor `triple` ever reads as an option, and neither holds
        // a path separator. Names a file system reserves, such as `nul`, are the
        // concern of whatever builds a path from one.
        if !is_component(&self.name, &['-', '_']) {
            return Err(format!(
                "`name` must start with a lowercase letter or a digit and hold only lowercase \
                 letters, digits, `-` and `_`, not {:?}",
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
        if !is_component(&self.triple, &['_', '.', '-']) {
            return Err(format!(
                "`triple` must start with a lowercase letter or a digit and hold only lowercase \
                 letters, digits, `_`, `.` and `-`, not {:?}",
                self.triple
            ));
        }
        if !matches!(self.pointer_width, 32 | 64) {
            return Err(format!(
                "`pointer-width` must be 32 or 64, not {}",
                self.pointer_width
            ));
        }
        let conv = self.call_conv;
        if conv.pointer_width() != self.pointer_width {
            return Err(format!(
                "`call-conv` {} needs a `pointer-width` of {}, not {}",
                conv.name(),
                conv.pointer_width(),
                self.pointer_width
            ));
        }
        if !conv.fits_arch(&self.arch) {
            return Err(format!(
                "`call-conv` {} is not a convention of `arch` `{}`",
                conv.name(),
                self.arch
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
                return Err(format!("capability {} is listed twice", capability.name()));
            }
        }
        let reads = self.linker.flavor.object_format();
        if reads != self.object_format {
            return Err(format!(
                "linker {} reads {} objects, but `object-format` is {}",
                self.linker.flavor.name(),
                reads.name(),
                self.object_format.name()
            ));
        }
        Ok(())
    }
}

/// Whether `s` starts with a lowercase ASCII letter or a digit and is made of
/// those and `extra`: as a path component or an argument, it never reads as
/// an option.
fn is_component(s: &str, extra: &[char]) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) && is_name(s, extra)
}

/// Whether `s` is non-empty and made of lowercase ASCII letters, digits and `extra`.
fn is_name(s: &str, extra: &[char]) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}
