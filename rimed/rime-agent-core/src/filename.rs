//! A file name somebody else chose, reduced to one safe to create.
//!
//! One rule for both directions: rime-agentd applies it to a file a phone
//! saves into Downloads (contract §1.8), rime-remoted to a file it sends to a
//! phone (§2.6). The phone applies its own copy as well, since it trusts no
//! name it is handed.

/// The longest name a saved file is given, in bytes, before numbering.
pub const MAX_NAME: usize = 200;

/// What a name that is empty after reduction becomes.
pub const FALLBACK_NAME: &str = "shared file";

/// Reduce a name to one safe to create in a Downloads folder.
///
/// Never a path: everything up to the last `/` or `\` goes. Never hidden:
/// leading dots and spaces go. Refused, not cleaned, when it holds a control
/// character, because a person who sent one did not mean to.
pub fn file_name(raw: &str) -> Result<String, String> {
    if let Some(b) = raw.bytes().find(|b| *b < 0x20 || *b == 0x7f) {
        return Err(format!(
            "that file's name contains a control character (0x{b:02x}), so it was not saved"
        ));
    }
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let base = base.trim_start_matches(['.', ' ']).trim_end_matches(' ');
    if base.is_empty() {
        return Ok(FALLBACK_NAME.to_string());
    }
    Ok(cap(base, MAX_NAME))
}

/// Cut `name` to at most `max` bytes on a character boundary, keeping a short
/// extension.
fn cap(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    let ext = if ext.len() <= 20 { ext } else { "" };
    let mut keep = max - ext.len();
    while !stem.is_char_boundary(keep.min(stem.len())) {
        keep -= 1;
    }
    format!("{}{ext}", &stem[..keep.min(stem.len())])
}

/// `("IMG_2041", ".jpg")`. A name that is only an extension has no stem to
/// split off, so it is all stem.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The `n`th name for a file: `IMG_2041.jpg`, then `IMG_2041 (2).jpg`.
pub fn numbered(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    let (stem, ext) = split_ext(name);
    format!("{stem} ({n}){ext}")
}
