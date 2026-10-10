//! Just enough ELF to read a binary's `DT_NEEDED` list and interpreter.
//!
//! The question it answers: can this new first-party binary run against the
//! libraries the machine is running *now*? It can when every library it loads
//! (transitively) is byte-identical between the booted and staged trees; the
//! CLI walks the closure with this and the file diff. A binary that links a
//! library the release also changed is deferred, because the old library is
//! what a process started today would load.
//!
//! The input comes from a verified deployment, but it is still parsed as if it
//! were hostile: every offset is bounds-checked, every count is capped, and
//! anything this reader does not understand is an error rather than "no
//! dependencies", because "no dependencies" would be a permission.

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: i64 = 0;
const DT_NEEDED: i64 = 1;
const DT_STRTAB: i64 = 5;
const DT_STRSZ: i64 = 10;

/// Dependencies read from one ELF file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Needs {
    pub interp: Option<String>,
    pub needed: Vec<String>,
}

fn u16_at(b: &[u8], off: usize) -> Result<u16, String> {
    let s = b.get(off..off.checked_add(2).ok_or("overflow")?).ok_or("truncated ELF")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    let s = b.get(off..off.checked_add(4).ok_or("overflow")?).ok_or("truncated ELF")?;
    Ok(u32::from_le_bytes(s.try_into().unwrap()))
}
fn u64_at(b: &[u8], off: usize) -> Result<u64, String> {
    let s = b.get(off..off.checked_add(8).ok_or("overflow")?).ok_or("truncated ELF")?;
    Ok(u64::from_le_bytes(s.try_into().unwrap()))
}
fn to_usize(v: u64) -> Result<usize, String> {
    usize::try_from(v).map_err(|_| "offset does not fit".to_string())
}

/// Is this an ELF file at all?
pub fn is_elf(b: &[u8]) -> bool {
    b.len() >= 4 && &b[..4] == b"\x7fELF"
}

fn cstr(b: &[u8], off: usize, limit: usize) -> Result<String, String> {
    let end_limit = limit.min(b.len());
    if off >= end_limit {
        return Err("string offset out of range".into());
    }
    let s = &b[off..end_limit];
    let n = s.iter().position(|&c| c == 0).ok_or("unterminated string")?;
    let v = std::str::from_utf8(&s[..n]).map_err(|_| "non-UTF-8 string")?;
    if v.is_empty() || v.contains('/') && !v.starts_with('/') {
        return Err(format!("implausible dependency name {v:?}"));
    }
    Ok(v.to_string())
}

/// Read the interpreter and `DT_NEEDED` entries. `Ok(None)` for a file that is
/// not ELF (a script, a QML file); an error for an ELF this reader cannot
/// vouch for (32-bit, big-endian, malformed).
pub fn needs(b: &[u8]) -> Result<Option<Needs>, String> {
    if !is_elf(b) {
        return Ok(None);
    }
    if b.len() < 64 {
        return Err("truncated ELF header".into());
    }
    if b[4] != 2 || b[5] != 1 {
        return Err("not a 64-bit little-endian ELF".into());
    }
    let phoff = to_usize(u64_at(b, 0x20)?)?;
    let phentsize = usize::from(u16_at(b, 0x36)?);
    let phnum = usize::from(u16_at(b, 0x38)?);
    if phentsize < 56 || phnum > 512 {
        return Err("implausible program header table".into());
    }
    let mut loads: Vec<(u64, u64, u64)> = Vec::new(); // (vaddr, offset, filesz)
    let mut dynamic: Option<(usize, usize)> = None;
    let mut interp = None;
    for i in 0..phnum {
        let ph = phoff.checked_add(i.checked_mul(phentsize).ok_or("overflow")?).ok_or("overflow")?;
        let p_type = u32_at(b, ph)?;
        let p_offset = u64_at(b, ph + 8)?;
        let p_vaddr = u64_at(b, ph + 16)?;
        let p_filesz = u64_at(b, ph + 32)?;
        match p_type {
            PT_LOAD => loads.push((p_vaddr, p_offset, p_filesz)),
            PT_DYNAMIC => dynamic = Some((to_usize(p_offset)?, to_usize(p_filesz)?)),
            PT_INTERP => {
                let off = to_usize(p_offset)?;
                let end = off.checked_add(to_usize(p_filesz)?).ok_or("overflow")?;
                interp = Some(cstr(b, off, end)?);
            }
            _ => {}
        }
    }
    let Some((doff, dsz)) = dynamic else {
        // Statically linked: depends on no shared object.
        return Ok(Some(Needs { interp, needed: vec![] }));
    };
    let dend = doff.checked_add(dsz).ok_or("overflow")?;
    if dend > b.len() {
        return Err("dynamic section out of range".into());
    }
    let mut needed_offs = Vec::new();
    let (mut strtab, mut strsz) = (None, None);
    let mut off = doff;
    let mut terminated = false;
    while off + 16 <= dend {
        let tag = u64_at(b, off)? as i64;
        let val = u64_at(b, off + 8)?;
        match tag {
            DT_NULL => {
                terminated = true;
                break;
            }
            DT_NEEDED => {
                if needed_offs.len() >= 256 {
                    return Err("implausibly many DT_NEEDED entries".into());
                }
                needed_offs.push(val)
            }
            DT_STRTAB => strtab = Some(val),
            DT_STRSZ => strsz = Some(val),
            _ => {}
        }
        off += 16;
    }
    if !terminated {
        return Err("dynamic section has no DT_NULL".into());
    }
    if needed_offs.is_empty() {
        return Ok(Some(Needs { interp, needed: vec![] }));
    }
    let strtab = strtab.ok_or("DT_NEEDED without DT_STRTAB")?;
    let strsz = strsz.ok_or("DT_NEEDED without DT_STRSZ")?;
    // DT_STRTAB is a virtual address; find the file offset through PT_LOAD.
    let file_off = loads
        .iter()
        .find(|(va, _, sz)| strtab >= *va && strtab < va.saturating_add(*sz))
        .map(|(va, fo, _)| fo + (strtab - va))
        .ok_or("DT_STRTAB is not inside any loaded segment")?;
    let base = to_usize(file_off)?;
    let limit = base.checked_add(to_usize(strsz)?).ok_or("overflow")?;
    let mut needed = Vec::new();
    for n in needed_offs {
        needed.push(cstr(b, base.checked_add(to_usize(n)?).ok_or("overflow")?, limit)?);
    }
    Ok(Some(Needs { interp, needed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 64-bit LE dynamic ELF: one PT_LOAD covering the whole file,
    /// a PT_INTERP, and a PT_DYNAMIC with two DT_NEEDED entries.
    fn tiny_elf(needed: &[&str], terminate: bool) -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // phoff
        b[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        b[0x38..0x3a].copy_from_slice(&3u16.to_le_bytes());
        let ph = |b: &mut Vec<u8>, i: usize, t: u32, off: u64, va: u64, sz: u64| {
            let p = 64 + i * 56;
            b[p..p + 4].copy_from_slice(&t.to_le_bytes());
            b[p + 8..p + 16].copy_from_slice(&off.to_le_bytes());
            b[p + 16..p + 24].copy_from_slice(&va.to_le_bytes());
            b[p + 32..p + 40].copy_from_slice(&sz.to_le_bytes());
        };
        let vbase = 0x400000u64;
        ph(&mut b, 0, PT_LOAD, 0, vbase, 0x400);
        // interp at 0x200
        let interp = b"/lib64/ld-linux-x86-64.so.2\0";
        b[0x200..0x200 + interp.len()].copy_from_slice(interp);
        ph(&mut b, 1, PT_INTERP, 0x200, vbase + 0x200, interp.len() as u64);
        // strtab at 0x300
        let mut strtab = vec![0u8];
        let mut offs = vec![];
        for n in needed {
            offs.push(strtab.len() as u64);
            strtab.extend_from_slice(n.as_bytes());
            strtab.push(0);
        }
        b[0x300..0x300 + strtab.len()].copy_from_slice(&strtab);
        // dynamic at 0x100
        let mut d = 0x100;
        let mut ent = |b: &mut Vec<u8>, tag: i64, val: u64| {
            b[d..d + 8].copy_from_slice(&tag.to_le_bytes());
            b[d + 8..d + 16].copy_from_slice(&val.to_le_bytes());
            d += 16;
        };
        for o in &offs {
            ent(&mut b, DT_NEEDED, *o);
        }
        ent(&mut b, DT_STRTAB, vbase + 0x300);
        ent(&mut b, DT_STRSZ, strtab.len() as u64);
        if terminate {
            ent(&mut b, DT_NULL, 0);
        }
        let dsz = (d - 0x100) as u64;
        ph(&mut b, 2, PT_DYNAMIC, 0x100, vbase + 0x100, dsz);
        b
    }

    #[test]
    fn reads_needed_and_interp() {
        let n = needs(&tiny_elf(&["libc.so.6", "libdbus-1.so.3"], true)).unwrap().unwrap();
        assert_eq!(n.needed, vec!["libc.so.6", "libdbus-1.so.3"]);
        assert_eq!(n.interp.as_deref(), Some("/lib64/ld-linux-x86-64.so.2"));
    }

    #[test]
    fn not_elf_is_none() {
        assert_eq!(needs(b"#!/bin/sh\necho\n").unwrap(), None);
        assert_eq!(needs(b"import QtQuick\n").unwrap(), None);
    }

    #[test]
    fn malformed_is_an_error_never_empty() {
        let mut e = tiny_elf(&["libc.so.6"], true);
        e[4] = 1; // 32-bit
        assert!(needs(&e).is_err());
        assert!(needs(&tiny_elf(&["libc.so.6"], false)).is_err());
        let full = tiny_elf(&["libc.so.6"], true);
        assert!(needs(&full[..0x120]).is_err());
        assert!(needs(b"\x7fELF\x02\x01").is_err());
        // A program header count that would walk off the file.
        let mut e = tiny_elf(&["libc.so.6"], true);
        e[0x38..0x3a].copy_from_slice(&400u16.to_le_bytes());
        assert!(needs(&e).is_err());
    }

    #[test]
    fn reads_a_real_binary_when_one_is_present() {
        // Exercised against the test runner itself, which is a real dynamic
        // ELF on every platform this crate builds for.
        let exe = std::env::current_exe().unwrap();
        let b = std::fs::read(exe).unwrap();
        let n = needs(&b).unwrap().unwrap();
        assert!(n.needed.iter().any(|l| l.starts_with("libc.so")), "{n:?}");
    }
}
