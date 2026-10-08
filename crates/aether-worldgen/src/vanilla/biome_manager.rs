//! `BiomeManager`: the block-resolution biome lookup.
//!
//! Biomes are stored per quart (4×4×4 blocks), but nothing that asks "what
//! biome is this block in" reads the quart under it directly. Vanilla jitters
//! the lookup — each of the eight surrounding quart corners is displaced by a
//! seeded pseudo-random offset and the nearest one wins — which is what turns
//! the staircase of quart cells into the ragged biome edges players see. The
//! surface rules and carvers read biomes through this.

/// `LinearCongruentialGenerator.next`.
#[inline]
fn lcg(seed: i64, salt: i64) -> i64 {
    seed.wrapping_mul(
        seed.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
    )
    .wrapping_add(salt)
}

#[inline]
fn fiddle(seed: i64) -> f64 {
    let d = (seed >> 24).rem_euclid(1024) as f64 / 1024.0;
    (d - 0.5) * 0.9
}

fn fiddled_distance(seed: i64, x: i32, y: i32, z: i32, fx: f64, fy: f64, fz: f64) -> f64 {
    let mut s = lcg(seed, x as i64);
    s = lcg(s, y as i64);
    s = lcg(s, z as i64);
    s = lcg(s, x as i64);
    s = lcg(s, y as i64);
    s = lcg(s, z as i64);
    let a = fiddle(s);
    s = lcg(s, seed);
    let b = fiddle(s);
    s = lcg(s, seed);
    let c = fiddle(s);
    let sq = |v: f64| v * v;
    sq(fz + c) + sq(fy + b) + sq(fx + a)
}

/// `BiomeManager.obfuscateSeed`: the first eight bytes (little-endian) of the
/// SHA-256 of the seed's little-endian bytes.
pub fn obfuscate_seed(seed: i64) -> i64 {
    let d = sha256(&seed.to_le_bytes());
    i64::from_le_bytes(d[0..8].try_into().unwrap())
}

/// The quart whose (jittered) centre is nearest block `(x, y, z)`.
pub fn zoomed_quart(zoom_seed: i64, x: i32, y: i32, z: i32) -> (i32, i32, i32) {
    let i = x - 2;
    let j = y - 2;
    let k = z - 2;
    let qx = i >> 2;
    let qy = j >> 2;
    let qz = k >> 2;
    let fx = (i & 3) as f64 / 4.0;
    let fy = (j & 3) as f64 / 4.0;
    let fz = (k & 3) as f64 / 4.0;
    let mut best = 0;
    let mut best_d = f64::INFINITY;
    for c in 0..8 {
        let ux = c & 4 == 0;
        let uy = c & 2 == 0;
        let uz = c & 1 == 0;
        let d = fiddled_distance(
            zoom_seed,
            if ux { qx } else { qx + 1 },
            if uy { qy } else { qy + 1 },
            if uz { qz } else { qz + 1 },
            if ux { fx } else { fx - 1.0 },
            if uy { fy } else { fy - 1.0 },
            if uz { fz } else { fz - 1.0 },
        );
        if best_d > d {
            best = c;
            best_d = d;
        }
    }
    (
        if best & 4 == 0 { qx } else { qx + 1 },
        if best & 2 == 0 { qy } else { qy + 1 },
        if best & 1 == 0 { qz } else { qz + 1 },
    )
}

// --- SHA-256 ---------------------------------------------------------------
// Only ever hashes one 8-byte seed, once per world.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 of `msg`.
pub fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut data = msg.to_vec();
    let bit_len = (msg.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes());
    for block in data.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_abc() {
        let d = sha256(b"abc");
        assert_eq!(
            d[..4],
            [0xba, 0x78, 0x16, 0xbf],
            "sha256(abc) starts ba7816bf"
        );
    }
}
