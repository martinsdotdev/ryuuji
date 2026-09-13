//! CityHash64, Version 1, as Firefox ships it. A Gecko browser's app user
//! model id on Windows is this hash of its install directory, so the port
//! has to be this exact version: 1.1 changed `HashLen0to16` and
//! `HashLen33to64` and gives a different number for the same path.
//!
//! Transcribed from Mozilla's vendored copy at
//! `other-licenses/nsis/Contrib/CityHash/cityhash/city.cpp`, which is
//! copyright 2011 Google, Inc. and licensed under the MIT licence. Only the
//! 64-bit function is ported.

const K0: u64 = 0xc3a5c85c97cb3127;
const K1: u64 = 0xb492b66fbe98f273;
const K2: u64 = 0x9ae16a3b2f90404f;
const K3: u64 = 0xc949d7c7509e6557;
const K_MUL: u64 = 0x9ddfea08eb382d69;

pub(crate) fn city_hash_64(s: &[u8]) -> u64 {
    let len = s.len();
    if len <= 16 {
        return hash_len_0_to_16(s);
    }
    if len <= 32 {
        return hash_len_17_to_32(s);
    }
    if len <= 64 {
        return hash_len_33_to_64(s);
    }

    let mut x = load64(s, 0);
    let mut y = load64(s, len - 16) ^ K1;
    let mut z = load64(s, len - 56) ^ K0;
    let mut v = weak_hash_len_32_with_seeds(&s[len - 64..], len as u64, y);
    let mut w = weak_hash_len_32_with_seeds(&s[len - 32..], (len as u64).wrapping_mul(K1), K0);
    z = z.wrapping_add(shift_mix(v.1).wrapping_mul(K1));
    x = rotate(z.wrapping_add(x), 39).wrapping_mul(K1);
    y = rotate(y, 33).wrapping_mul(K1);

    let mut remaining = (len - 1) & !63;
    let mut s = s;
    loop {
        x = rotate(
            x.wrapping_add(y)
                .wrapping_add(v.0)
                .wrapping_add(load64(s, 16)),
            37,
        )
        .wrapping_mul(K1);
        y = rotate(y.wrapping_add(v.1).wrapping_add(load64(s, 48)), 42).wrapping_mul(K1);
        x ^= w.1;
        y ^= v.0;
        z = rotate(z ^ w.0, 33);
        v = weak_hash_len_32_with_seeds(s, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak_hash_len_32_with_seeds(&s[32..], z.wrapping_add(w.1), y);
        std::mem::swap(&mut z, &mut x);
        s = &s[64..];
        remaining -= 64;
        if remaining == 0 {
            break;
        }
    }
    hash_len_16(
        hash_len_16(v.0, w.0)
            .wrapping_add(shift_mix(y).wrapping_mul(K1))
            .wrapping_add(z),
        hash_len_16(v.1, w.1).wrapping_add(x),
    )
}

fn load64(s: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(s[at..at + 8].try_into().expect("eight bytes"))
}

fn load32(s: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes(
        s[at..at + 4].try_into().expect("four bytes"),
    ))
}

fn rotate(val: u64, shift: u32) -> u64 {
    val.rotate_right(shift)
}

fn shift_mix(val: u64) -> u64 {
    val ^ (val >> 47)
}

/// `Hash128to64` from the original: Murmur-style mixing of two words.
fn hash_len_16(u: u64, v: u64) -> u64 {
    let mut a = (u ^ v).wrapping_mul(K_MUL);
    a ^= a >> 47;
    let mut b = (v ^ a).wrapping_mul(K_MUL);
    b ^= b >> 47;
    b.wrapping_mul(K_MUL)
}

fn hash_len_0_to_16(s: &[u8]) -> u64 {
    let len = s.len();
    if len > 8 {
        let a = load64(s, 0);
        let b = load64(s, len - 8);
        return hash_len_16(a, rotate(b.wrapping_add(len as u64), len as u32)) ^ b;
    }
    if len >= 4 {
        let a = load32(s, 0);
        return hash_len_16((len as u64).wrapping_add(a << 3), load32(s, len - 4));
    }
    if len > 0 {
        let a = u32::from(s[0]);
        let b = u32::from(s[len >> 1]);
        let c = u32::from(s[len - 1]);
        let y = u64::from(a.wrapping_add(b << 8));
        let z = u64::from((len as u32).wrapping_add(c << 2));
        return shift_mix(y.wrapping_mul(K2) ^ z.wrapping_mul(K3)).wrapping_mul(K2);
    }
    K2
}

fn hash_len_17_to_32(s: &[u8]) -> u64 {
    let len = s.len();
    let a = load64(s, 0).wrapping_mul(K1);
    let b = load64(s, 8);
    let c = load64(s, len - 8).wrapping_mul(K2);
    let d = load64(s, len - 16).wrapping_mul(K0);
    hash_len_16(
        rotate(a.wrapping_sub(b), 43)
            .wrapping_add(rotate(c, 30))
            .wrapping_add(d),
        a.wrapping_add(rotate(b ^ K3, 20))
            .wrapping_sub(c)
            .wrapping_add(len as u64),
    )
}

/// A 16-byte hash of the first 32 bytes of `s` seeded with `a` and `b`.
fn weak_hash_len_32_with_seeds(s: &[u8], a: u64, b: u64) -> (u64, u64) {
    let (w, x, y, z) = (load64(s, 0), load64(s, 8), load64(s, 16), load64(s, 24));
    let a = a.wrapping_add(w);
    let b = rotate(b.wrapping_add(a).wrapping_add(z), 21);
    let c = a;
    let a = a.wrapping_add(x).wrapping_add(y);
    let b = b.wrapping_add(rotate(a, 44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

fn hash_len_33_to_64(s: &[u8]) -> u64 {
    let len = s.len();
    let mut z = load64(s, 24);
    let mut a = load64(s, 0).wrapping_add(
        (len as u64)
            .wrapping_add(load64(s, len - 16))
            .wrapping_mul(K0),
    );
    let mut b = rotate(a.wrapping_add(z), 52);
    let mut c = rotate(a, 37);
    a = a.wrapping_add(load64(s, 8));
    c = c.wrapping_add(rotate(a, 7));
    a = a.wrapping_add(load64(s, 16));
    let vf = a.wrapping_add(z);
    let vs = b.wrapping_add(rotate(a, 31)).wrapping_add(c);
    a = load64(s, 16).wrapping_add(load64(s, len - 32));
    z = load64(s, len - 8);
    b = rotate(a.wrapping_add(z), 52);
    c = rotate(a, 37);
    a = a.wrapping_add(load64(s, len - 24));
    c = c.wrapping_add(rotate(a, 7));
    a = a.wrapping_add(load64(s, len - 16));
    let wf = a.wrapping_add(z);
    let ws = b.wrapping_add(rotate(a, 31)).wrapping_add(c);
    let r = shift_mix(
        vf.wrapping_add(ws)
            .wrapping_mul(K2)
            .wrapping_add(wf.wrapping_add(vs).wrapping_mul(K0)),
    );
    shift_mix(r.wrapping_mul(K0).wrapping_add(vs)).wrapping_mul(K2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        format!("{:016X}", city_hash_64(bytes))
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// The one externally observed vector: LibreWolf's media session on this
    /// machine reports this id for this install directory.
    #[test]
    fn librewolfs_install_directory_hashes_to_its_observed_app_id() {
        let bytes = utf16(r"C:\Program Files\LibreWolf");
        assert_eq!(bytes.len(), 52);
        assert_eq!(hex(&bytes), "83C1C0F3FA8524B1");
    }

    #[test]
    fn the_empty_input_is_k2() {
        assert_eq!(hex(b""), "9AE16A3B2F90404F");
    }

    /// Expected values from a Python transcription of the same vendored
    /// source, one input per branch of the C++ function.
    #[test]
    fn every_length_branch_matches_the_reference() {
        for (input, expected) in [
            (&b"a"[..], "2420662CD003ACFA"),
            (b"abc", "3A912F483A4ECE31"),
            (b"abcd", "F75A3B8A1499428D"),
            (b"hello", "23C7ADA5F323C8DF"),
            (b"hello wor", "177B274F02111477"),
            (b"0123456789abcdef", "099D21E99DAC3317"),
            (b"0123456789abcdefg", "0AFFC467E579303A"),
            (b"The quick brown fox jumps over", "63EFFFE9741FB54D"),
            (
                b"The quick brown fox jumps over the lazy dog",
                "E7BA87D247C31277",
            ),
        ] {
            assert_eq!(hex(input), expected, "{input:?}");
        }
        let counting: Vec<u8> = (0..65).collect();
        assert_eq!(hex(&counting), "510F4EF776C6476D");
        assert_eq!(hex(&[b'x'; 100]), "72D22BBCC6188483");
    }

    /// A per-user Firefox install directory is over 64 bytes of UTF-16, the
    /// branch a machine-wide install never reaches.
    #[test]
    fn a_per_user_firefox_path_takes_the_long_branch() {
        let bytes = utf16(r"C:\Users\umaru\AppData\Local\Mozilla Firefox");
        assert!(bytes.len() > 64);
        assert_eq!(hex(&bytes), "D52277D1BA334E98");
    }
}
