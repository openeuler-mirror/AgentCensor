//! Minimal ELF symbol resolver for dynamic TLS probe plans.
//!
//! The resolver is intentionally read-only. It computes file offsets for
//! known TLS symbols; attachment and coverage state are separate operations.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::tls_coverage::TlsLibrary;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsDirection {
    Inbound,
    Outbound,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlsProbePoint {
    pub symbol: String,
    pub file_offset: u64,
    pub direction: TlsDirection,
    pub retprobe: bool,
}

pub fn resolve_library_plan(
    path: &Path,
    library: TlsLibrary,
) -> Result<Vec<TlsProbePoint>, String> {
    let symbols: &[(&str, TlsDirection)] = match library {
        TlsLibrary::OpenSsl => &[
            ("SSL_write", TlsDirection::Outbound),
            ("SSL_read", TlsDirection::Inbound),
            ("SSL_write_ex", TlsDirection::Outbound),
            ("SSL_read_ex", TlsDirection::Inbound),
        ],
        TlsLibrary::GnuTls => &[
            ("gnutls_record_send", TlsDirection::Outbound),
            ("gnutls_record_recv", TlsDirection::Inbound),
        ],
        TlsLibrary::Nss => &[
            ("PR_Write", TlsDirection::Outbound),
            ("PR_Read", TlsDirection::Inbound),
        ],
        // Go exports method symbols in statically linked binaries, optionally
        // suffixed with an ABI marker such as `.abiinternal`.
        TlsLibrary::GoTls => &[
            ("crypto/tls.(*Conn).Write", TlsDirection::Outbound),
            ("crypto/tls.(*Conn).Read", TlsDirection::Inbound),
        ],
        // Rustls symbols are mangled; probe plans match stable semantic
        // fragments of the symbol names.
        TlsLibrary::Rustls => &[
            ("*rustls_write_tls", TlsDirection::Outbound),
            ("*rustls_read_tls", TlsDirection::Inbound),
        ],
    };
    let names = symbols.iter().map(|(name, _)| *name).collect::<Vec<_>>();
    let mandatory = names
        .iter()
        .copied()
        .filter(|name| !matches!(*name, "SSL_write_ex" | "SSL_read_ex"))
        .collect::<Vec<_>>();
    let mut offsets = BTreeMap::new();
    let mut first_error = None;
    for name in mandatory {
        match resolve_symbol_offsets(path, &[name]) {
            Ok(found) => offsets.extend(found),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    // The *_ex entry points are version-dependent: they add coverage when
    // present but must not block the classic probes on older libssl releases.
    for optional_name in ["SSL_write_ex", "SSL_read_ex"] {
        if let Ok(optional) = resolve_symbol_offsets(path, &[optional_name]) {
            offsets.extend(optional);
        }
    }
    let points = symbols
        .iter()
        .filter_map(|(name, direction)| {
            offsets.get(*name).copied().map(|offset| TlsProbePoint {
                symbol: (*name).to_string(),
                file_offset: offset,
                direction: *direction,
                retprobe: false,
            })
        })
        .collect::<Vec<_>>();
    if points.is_empty() {
        return Err(
            first_error.unwrap_or_else(|| format!("missing TLS symbols in {}", path.display()))
        );
    }
    Ok(points)
}

fn resolve_symbol_offsets(path: &Path, required: &[&str]) -> Result<BTreeMap<String, u64>, String> {
    let data = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let elf = ElfImage::parse(&data)?;
    let sections = parse_sections(&data)?;
    let mut virtual_addresses = BTreeMap::new();
    for section in &sections {
        if section.kind != 2 && section.kind != 11 {
            continue;
        }
        read_symbol_table(&data, &sections, section, required, &mut virtual_addresses)?;
    }
    required
        .iter()
        .map(|name| {
            let address = virtual_addresses
                .get(*name)
                .copied()
                .ok_or_else(|| format!("missing ELF symbol {name}"))?;
            Ok(((*name).to_string(), elf.executable_offset(address)?))
        })
        .collect()
}

struct ElfImage {
    segments: Vec<LoadSegment>,
}

struct LoadSegment {
    file_offset: u64,
    virtual_address: u64,
    file_size: u64,
    executable: bool,
}

impl ElfImage {
    fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 64 || &data[..4] != b"\x7fELF" || data[4] != 2 || data[5] != 1 {
            return Err("target is not ELF64 little-endian".to_string());
        }
        let phoff = get_u64(data, 32)?;
        let entsize = u64::from(get_u16(data, 54)?);
        let count = u64::from(get_u16(data, 56)?);
        let mut segments = Vec::new();
        for index in 0..count {
            let offset = table_offset(phoff, entsize, index)?;
            let header = bounded(data, offset, entsize)?;
            if get_u32(header, 0)? != 1 {
                continue;
            }
            let flags = get_u32(header, 4)?;
            segments.push(LoadSegment {
                file_offset: get_u64(header, 8)?,
                virtual_address: get_u64(header, 16)?,
                file_size: get_u64(header, 32)?,
                executable: flags & 1 != 0,
            });
        }
        if segments.is_empty() {
            return Err("ELF has no load segments".to_string());
        }
        Ok(Self { segments })
    }

    fn executable_offset(&self, address: u64) -> Result<u64, String> {
        self.segments
            .iter()
            .find_map(|segment| {
                if !segment.executable || address < segment.virtual_address {
                    return None;
                }
                let relative = address - segment.virtual_address;
                (relative < segment.file_size).then(|| segment.file_offset + relative)
            })
            .ok_or_else(|| format!("TLS symbol virtual address 0x{address:x} is not executable"))
    }
}

struct Section {
    kind: u32,
    offset: u64,
    size: u64,
    link: u32,
    entry_size: u64,
}

fn parse_sections(data: &[u8]) -> Result<Vec<Section>, String> {
    let table = get_u64(data, 40)?;
    let entry = u64::from(get_u16(data, 58)?);
    let count = u64::from(get_u16(data, 60)?);
    if table == 0 || entry == 0 || count == 0 {
        return Err("ELF has no section table".to_string());
    }
    (0..count)
        .map(|index| {
            let header = bounded(data, table_offset(table, entry, index)?, entry)?;
            Ok(Section {
                kind: get_u32(header, 4)?,
                offset: get_u64(header, 24)?,
                size: get_u64(header, 32)?,
                link: get_u32(header, 40)?,
                entry_size: get_u64(header, 56)?,
            })
        })
        .collect()
}

fn read_symbol_table(
    data: &[u8],
    sections: &[Section],
    section: &Section,
    required: &[&str],
    output: &mut BTreeMap<String, u64>,
) -> Result<(), String> {
    if section.entry_size < 24 {
        return Err("ELF symbol entry is too small".to_string());
    }
    let strings = sections
        .get(section.link as usize)
        .ok_or_else(|| "ELF string table missing".to_string())?;
    let strings = bounded(data, strings.offset, strings.size)?;
    let table = bounded(data, section.offset, section.size)?;
    let entry = usize::try_from(section.entry_size).map_err(|_| "ELF entry overflow")?;
    for symbol in table.chunks_exact(entry) {
        if get_u16(symbol, 6)? == 0 {
            continue;
        }
        let name_offset = get_u32(symbol, 0)? as usize;
        let address = get_u64(symbol, 8)?;
        if address == 0 || name_offset >= strings.len() {
            continue;
        }
        let tail = &strings[name_offset..];
        let end = tail
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| "unterminated ELF string".to_string())?;
        let name = std::str::from_utf8(&tail[..end]).map_err(|_| "invalid ELF symbol name")?;
        for required_name in required {
            if symbol_matches(required_name, name) {
                output
                    .entry((*required_name).to_string())
                    .or_insert(address);
            }
        }
    }
    Ok(())
}

fn symbol_matches(required: &str, actual: &str) -> bool {
    if let Some(fragment) = required.strip_prefix('*') {
        // Mangled Rust symbols vary in qualification; requiring the fragment
        // to occur is deliberately conservative.
        actual.contains(fragment)
    } else {
        actual == required || actual.strip_suffix(".abiinternal") == Some(required)
    }
}

fn table_offset(base: u64, entry: u64, index: u64) -> Result<u64, String> {
    base.checked_add(
        entry
            .checked_mul(index)
            .ok_or_else(|| "ELF table overflow".to_string())?,
    )
    .ok_or_else(|| "ELF table overflow".to_string())
}

fn bounded(data: &[u8], offset: u64, size: u64) -> Result<&[u8], String> {
    let offset = usize::try_from(offset).map_err(|_| "ELF offset overflow")?;
    let size = usize::try_from(size).map_err(|_| "ELF size overflow")?;
    let end = offset
        .checked_add(size)
        .ok_or_else(|| "ELF bounds overflow".to_string())?;
    data.get(offset..end)
        .ok_or_else(|| "ELF data is truncated".to_string())
}

fn get_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = data
        .get(offset..offset + 2)
        .ok_or_else(|| "ELF data is truncated".to_string())?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn get_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "ELF data is truncated".to_string())?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn get_u64(data: &[u8], offset: usize) -> Result<u64, String> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or_else(|| "ELF data is truncated".to_string())?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_openssl_symbols_when_library_is_present() {
        let path = Path::new("/usr/lib/x86_64-linux-gnu/libssl.so.3");
        if !path.exists() {
            return;
        }
        let points = resolve_library_plan(path, TlsLibrary::OpenSsl).expect("resolve OpenSSL");
        assert!(points.iter().any(|point| point.symbol == "SSL_read"));
        assert!(points.iter().all(|point| point.file_offset > 0));
    }

    #[test]
    fn rejects_non_elf_target_explicitly() {
        let error = resolve_library_plan(Path::new("/etc/hosts"), TlsLibrary::OpenSsl)
            .expect_err("non-ELF must fail");
        assert!(error.contains("ELF"));
    }

    #[test]
    fn matches_go_abi_suffix_and_rustls_mangled_fragment() {
        assert!(symbol_matches(
            "crypto/tls.(*Conn).Read",
            "crypto/tls.(*Conn).Read.abiinternal"
        ));
        assert!(symbol_matches(
            "*rustls_read_tls",
            "_ZN6rustls4conn18rustls_read_tls17hdeadbeefE"
        ));
        assert!(!symbol_matches("*rustls_read_tls", "not_a_tls_symbol"));
    }
}
