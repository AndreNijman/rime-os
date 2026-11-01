//! narHash spellings. Nix 2.35 prints SRI (`sha256-<base64>`); older output and
//! narinfo files use `sha256:<nix base32>`. Both decode to the same 32 bytes, and
//! comparisons are made on the bytes, never on the text.

const NIX32: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Nix's own base32 (reversed bit order, its own alphabet), as in libutil's
/// `Hash::parseNonSRIUnprefixed`.
pub fn nix32_decode(s: &str, size: usize) -> Option<Vec<u8>> {
    let want = (size * 8 - 1) / 5 + 1;
    if s.len() != want {
        return None;
    }
    let mut out = vec![0u8; size];
    let bytes = s.as_bytes();
    for n in 0..bytes.len() {
        let c = bytes[bytes.len() - n - 1];
        let digit = NIX32.iter().position(|&x| x == c)? as u16;
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        out[i] |= (digit << j) as u8;
        let carry = digit >> (8 - j);
        if i + 1 < size {
            out[i + 1] |= carry as u8;
        } else if carry != 0 {
            return None;
        }
    }
    Some(out)
}

/// Nix base32 of bytes (the inverse of [`nix32_decode`]).
pub fn nix32_encode(h: &[u8]) -> String {
    let len = (h.len() * 8 - 1) / 5 + 1;
    let mut s = String::with_capacity(len);
    for n in (0..len).rev() {
        let b = n * 5;
        let i = b / 8;
        let j = b % 8;
        let lo = (h[i] as u16) >> j;
        let hi = if i + 1 >= h.len() { 0 } else { (h[i + 1] as u16) << (8 - j) };
        s.push(NIX32[((lo | hi) & 0x1f) as usize] as char);
    }
    s
}

/// Standard base64 with padding.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut pad = 0;
    for c in s.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            '=' => {
                pad += 1;
                continue;
            }
            _ => return None,
        };
        if pad > 0 {
            return None;
        }
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if pad > 2 || acc != 0 {
        return None;
    }
    Some(out)
}

/// The 32 bytes of a sha256 narHash in either spelling, or `None`.
pub fn nar_hash_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if let Some(b64) = s.strip_prefix("sha256-") {
        let b = b64_decode(b64)?;
        return (b.len() == 32).then_some(b);
    }
    if let Some(rest) = s.strip_prefix("sha256:") {
        if rest.len() == 64 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
            return (0..32).map(|i| u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16).ok()).collect();
        }
        return nix32_decode(rest, 32);
    }
    None
}

/// Whether two narHash strings name the same hash (both must parse).
pub fn same_nar_hash(a: &str, b: &str) -> bool {
    match (nar_hash_bytes(a), nar_hash_bytes(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // sha256 of the empty string, in the three spellings Nix 2.35.4 prints (measured with `nix hash convert`).
    const SRI: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const NIX32_EMPTY: &str = "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73";
    const HEX: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn the_three_spellings_of_one_hash_are_the_same_hash() {
        assert!(same_nar_hash(SRI, NIX32_EMPTY));
        assert!(same_nar_hash(SRI, HEX));
        assert_eq!(nix32_encode(&nar_hash_bytes(SRI).unwrap()), &NIX32_EMPTY[7..]);
    }

    #[test]
    fn a_different_hash_and_garbage_are_not_the_same() {
        let other = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert!(!same_nar_hash(SRI, other));
        assert!(!same_nar_hash(SRI, "sha256-not base64"));
        assert!(!same_nar_hash("garbage", "garbage"));
        assert!(nar_hash_bytes("sha512-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=").is_none());
        assert!(nar_hash_bytes("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c7e").is_none());
    }

    #[test]
    fn nix32_round_trips() {
        let bytes: Vec<u8> = (0u8..32).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        assert_eq!(nix32_decode(&nix32_encode(&bytes), 32).unwrap(), bytes);
    }
}
