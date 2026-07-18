//! Deterministische UUIDs (RFC-4122-Format) aus einem Seed-String.
//!
//! NMOS-Ressourcen brauchen stabile UUIDs. Statt Zufall (v4) leiten wir sie
//! **deterministisch** aus Node-Name + Ressourcen-Rolle ab — so bleibt die
//! Identität eines Senders/Receivers über Neustarts gleich (wichtig für
//! Controller/Registry). Format-gültig (8-4-4-4-12 hex, Version/Variant gesetzt),
//! ohne externe Crate.

/// 64-bit FNV-1a mit wählbarem Startwert.
fn fnv64(bytes: &[u8], mut hash: u64) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Erzeugt eine format-gültige UUID (v5-artig, „name-based") aus `seed`.
pub fn uuid_from(seed: &str) -> String {
    let h1 = fnv64(seed.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let h2 = fnv64(seed.as_bytes(), 0x8422_2325_cbf2_9ce4);
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&h1.to_be_bytes());
    b[8..].copy_from_slice(&h2.to_be_bytes());
    // Version 5 (Name-based) + RFC-4122-Variant.
    b[6] = (b[6] & 0x0F) | 0x50;
    b[8] = (b[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_is_uuid_shaped() {
        let u = uuid_from("taktwerk:node");
        assert_eq!(u.len(), 36);
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        // Version-Nibble = 5, Variant-Bits = 10xx.
        assert_eq!(&u[14..15], "5");
        assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn deterministic_and_distinct() {
        assert_eq!(uuid_from("a"), uuid_from("a"));
        assert_ne!(uuid_from("a"), uuid_from("b"));
    }
}
