//! UEFI boot options, encoded and decoded as bytes.
//!
//! The installer makes exactly one firmware entry, "Rime OS Setup", and asks
//! the firmware to start it once (`BootNext`). Everything here is the byte
//! layout of that request, kept apart from the Windows calls that store it so
//! it can be checked on any machine: the encoder is tested against an entry a
//! real firmware accepted, read off this project's own laptop.
//!
//! UEFI 2.10 section 3.1.3 (EFI_LOAD_OPTION) and 10.3 (device paths):
//!
//! ```text
//!   u32  Attributes            LOAD_OPTION_ACTIVE = 1
//!   u16  FilePathListLength    bytes of the device path below
//!   u16  Description[]         UTF-16, NUL-terminated
//!   ..   FilePathList          HD() / File() / End
//!   ..   OptionalData          none
//! ```
//!
//! HD() is media type 4, subtype 1, 42 bytes: partition number, start LBA,
//! size in LBAs, the GPT partition GUID, MBR type 2 (GPT), signature type 2
//! (GUID). Firmware matches the GUID; the number and extent are what the spec
//! requires to be right anyway, and they are right.

use std::io;

pub const LOAD_OPTION_ACTIVE: u32 = 1;
/// The label of the one entry this program creates. The Linux installer
/// removes the entry after a successful install only when the label matches
/// this exactly, so it is a contract and not a display string.
pub const SETUP_DESCRIPTION: &str = "Rime OS Setup";
/// Where the setup loader lives on Rime's ESP. Also part of the contract.
pub const SETUP_LOADER: &str = "\\EFI\\rimeinst\\shimx64.efi";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardDrive {
    pub partition_number: u32,
    pub start_lba: u64,
    pub size_lba: u64,
    /// The GPT unique partition GUID, in its on-disk (mixed-endian) bytes.
    pub guid: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadOption {
    pub attributes: u32,
    pub description: String,
    pub hard_drive: Option<HardDrive>,
    pub file: Option<String>,
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect()
}

/// Encode an active load option that starts `file` from the GPT partition
/// described by `hd`. Refuses inputs that would make a malformed option
/// rather than truncating them.
pub fn encode(description: &str, hd: &HardDrive, file: &str) -> io::Result<Vec<u8>> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_string());
    if description.is_empty() || description.chars().count() > 64 {
        return Err(bad("boot entry description must be 1 to 64 characters"));
    }
    if !file.starts_with('\\') || file.contains('/') || file.contains('\0') {
        return Err(bad("boot entry path must be a backslash path from the ESP root"));
    }
    if hd.partition_number == 0 || hd.size_lba == 0 || hd.guid == [0; 16] {
        return Err(bad("boot entry partition is incomplete"));
    }
    let mut dp = Vec::new();
    // HD(): type 4 (media), subtype 1 (hard drive), length 42.
    dp.extend_from_slice(&[0x04, 0x01, 42, 0]);
    dp.extend_from_slice(&hd.partition_number.to_le_bytes());
    dp.extend_from_slice(&hd.start_lba.to_le_bytes());
    dp.extend_from_slice(&hd.size_lba.to_le_bytes());
    dp.extend_from_slice(&hd.guid);
    dp.extend_from_slice(&[0x02, 0x02]);
    // File(): type 4, subtype 4, length 4 + the NUL-terminated UTF-16 path.
    let path = utf16z(file);
    let len = u16::try_from(4 + path.len()).map_err(|_| bad("boot entry path too long"))?;
    dp.extend_from_slice(&[0x04, 0x04]);
    dp.extend_from_slice(&len.to_le_bytes());
    dp.extend_from_slice(&path);
    // End of the entire device path.
    dp.extend_from_slice(&[0x7f, 0xff, 0x04, 0x00]);

    let mut out = Vec::new();
    out.extend_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    out.extend_from_slice(&(dp.len() as u16).to_le_bytes());
    out.extend_from_slice(&utf16z(description));
    out.extend_from_slice(&dp);
    Ok(out)
}

/// Decode a load option. Only the parts this program reasons about are
/// extracted (an HD() node and a File() node); any other node is skipped by
/// its length. `None` for bytes that are not a well-formed option.
pub fn decode(b: &[u8]) -> Option<LoadOption> {
    if b.len() < 6 {
        return None;
    }
    let attributes = u32::from_le_bytes(b[0..4].try_into().ok()?);
    let dp_len = u16::from_le_bytes([b[4], b[5]]) as usize;
    let mut i = 6;
    let mut desc = Vec::new();
    loop {
        let c = u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]);
        i += 2;
        if c == 0 {
            break;
        }
        desc.push(c);
    }
    let description = String::from_utf16(&desc).ok()?;
    let dp = b.get(i..i.checked_add(dp_len)?)?;
    let (mut hard_drive, mut file) = (None, None);
    let mut j = 0;
    while j + 4 <= dp.len() {
        let (t, s) = (dp[j], dp[j + 1]);
        let l = u16::from_le_bytes([dp[j + 2], dp[j + 3]]) as usize;
        if l < 4 || j + l > dp.len() {
            return None;
        }
        let node = &dp[j..j + l];
        match (t, s) {
            (0x7f, 0xff) => break,
            (0x04, 0x01) if l == 42 => {
                hard_drive = Some(HardDrive {
                    partition_number: u32::from_le_bytes(node[4..8].try_into().ok()?),
                    start_lba: u64::from_le_bytes(node[8..16].try_into().ok()?),
                    size_lba: u64::from_le_bytes(node[16..24].try_into().ok()?),
                    guid: node[24..40].try_into().ok()?,
                });
            }
            (0x04, 0x04) => {
                let units: Vec<u16> = node[4..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .take_while(|c| *c != 0)
                    .collect();
                file = Some(String::from_utf16(&units).ok()?);
            }
            _ => {}
        }
        j += l;
    }
    Some(LoadOption { attributes, description, hard_drive, file })
}

/// `BootOrder` is a packed array of u16 option numbers.
pub fn decode_order(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}
pub fn encode_order(v: &[u16]) -> Vec<u8> {
    v.iter().flat_map(|n| n.to_le_bytes()).collect()
}

/// The lowest option number nothing uses: not an existing `Boot####` and not
/// named in `BootOrder` (an order entry with no variable behind it is a slot
/// the firmware may still be reserving). Starts at 0x0001 rather than 0 on
/// purpose: some firmware treats Boot0000 as its own.
pub fn free_number(existing: &[u16], order: &[u16]) -> Option<u16> {
    (1..=0xFFFFu16).find(|n| !existing.contains(n) && !order.contains(n))
}

/// Firmware variable name for an option number. Uppercase hex, as the spec
/// requires: `Boot000a` is a different, never-consulted variable.
pub fn option_name(n: u16) -> String {
    format!("Boot{n:04X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body of a real `Boot0000` read from /sys/firmware/efi/efivars on
    /// the L16 (the 4-byte efivarfs attribute prefix removed). Firmware
    /// accepted and booted it, which is the strongest available statement
    /// that this encoding is the one firmware expects.
    const REAL: &str = "01000000620041005000450058002d004f00530000000401\
        2a0001000000000800000000000000c0120000000000e27d\
        411c66575f4593181986108854240202040434005c004500\
        460049005c006600650064006f00720061005c0073006800\
        69006d007800360034002e0065006600690000007fff0400";

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn l16_hd() -> HardDrive {
        HardDrive {
            partition_number: 1,
            start_lba: 0x800,
            size_lba: 0x12c000,
            guid: unhex("e27d411c66575f459318198610885424").try_into().unwrap(),
        }
    }

    #[test]
    fn encodes_byte_for_byte_what_real_firmware_booted() {
        let got = encode("APEX-OS", &l16_hd(), "\\EFI\\fedora\\shimx64.efi").unwrap();
        assert_eq!(got, unhex(REAL));
    }

    #[test]
    fn decodes_what_it_encodes_and_what_firmware_wrote() {
        let real = decode(&unhex(REAL)).unwrap();
        assert_eq!(real.description, "APEX-OS");
        assert_eq!(real.attributes, LOAD_OPTION_ACTIVE);
        assert_eq!(real.hard_drive, Some(l16_hd()));
        assert_eq!(real.file.as_deref(), Some("\\EFI\\fedora\\shimx64.efi"));
        let ours = encode(SETUP_DESCRIPTION, &l16_hd(), SETUP_LOADER).unwrap();
        let back = decode(&ours).unwrap();
        assert_eq!(back.description, SETUP_DESCRIPTION);
        assert_eq!(back.file.as_deref(), Some(SETUP_LOADER));
    }

    #[test]
    fn refuses_what_would_be_malformed() {
        let hd = l16_hd();
        assert!(encode("", &hd, SETUP_LOADER).is_err());
        assert!(encode("x", &hd, "/EFI/x.efi").is_err());
        assert!(encode("x", &HardDrive { guid: [0; 16], ..hd.clone() }, SETUP_LOADER).is_err());
        assert!(encode("x", &HardDrive { partition_number: 0, ..hd }, SETUP_LOADER).is_err());
        assert!(decode(&[1, 0, 0, 0, 200, 0, 65, 0, 0, 0]).is_none(), "path length past the end");
    }

    #[test]
    fn free_number_skips_existing_and_ordered_numbers() {
        assert_eq!(free_number(&[0, 1, 2], &[]), Some(3));
        assert_eq!(free_number(&[0, 2], &[1]), Some(3));
        assert_eq!(free_number(&[], &[]), Some(1));
        assert_eq!(option_name(0x1a), "Boot001A");
        assert_eq!(decode_order(&encode_order(&[0, 0x20, 0x1d])), vec![0, 0x20, 0x1d]);
    }
}
