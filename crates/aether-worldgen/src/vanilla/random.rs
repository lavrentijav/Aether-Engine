//! Vanilla's modern random source: xoroshiro128++ with the seeding vanilla
//! wraps around it.
//!
//! Since 1.18 worldgen draws from `XoroshiroRandomSource` rather than the
//! legacy 48-bit LCG in [`crate::beta::random`]. Two pieces have to match for
//! anything downstream to line up:
//!
//! * the generator itself — plain xoroshiro128++ (Blackman & Vigna), and
//! * how a `u64` world seed becomes its 128-bit state, and how that state is
//!   *forked* per noise field.
//!
//! The fork is the subtle part. Each noise gets its own stream derived from a
//! namespaced string (`minecraft:temperature` and friends) hashed with MD5,
//! which is why an MD5 lives down at the bottom of this file: without it the
//! per-field streams diverge and every noise value after them is wrong.

/// Stafford variant 13 of the splitmix64 finalizer — vanilla's `mixStafford13`.
#[inline]
fn mix_stafford13(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Expand a 64-bit world seed into the 128-bit state xoroshiro needs.
///
/// Vanilla's `RandomSupport.upgradeSeedTo128bit`: xor by the golden-ratio
/// constant, step by another, and run both halves through the finalizer.
fn upgrade_seed_to_128(seed: u64) -> (u64, u64) {
    let lo = seed ^ 0x6A09_E667_F3BC_C909;
    let hi = lo.wrapping_add(0x9E37_79B9_7F4A_7C15);
    (mix_stafford13(lo), mix_stafford13(hi))
}

/// xoroshiro128++ with vanilla's seeding and draw helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XoroshiroRandom {
    lo: u64,
    hi: u64,
}

impl XoroshiroRandom {
    /// A source seeded the way vanilla seeds one from a world seed.
    pub fn new(seed: u64) -> Self {
        let (lo, hi) = upgrade_seed_to_128(seed);
        Self::from_state(lo, hi)
    }

    /// A source over an explicit 128-bit state.
    ///
    /// An all-zero state is a fixed point of xoroshiro, so vanilla substitutes
    /// two constants for it rather than emitting an endless run of zeroes.
    pub fn from_state(lo: u64, hi: u64) -> Self {
        if lo == 0 && hi == 0 {
            return Self {
                lo: 0x9E37_79B9_7F4A_7C15,
                hi: 0x6A09_E667_F3BC_C909,
            };
        }
        Self { lo, hi }
    }

    /// The raw generator step.
    pub fn next_u64(&mut self) -> u64 {
        let l = self.lo;
        let mut m = self.hi;
        // The "++" scrambler: rotate the sum, add the low word back.
        let out = l.wrapping_add(m).rotate_left(17).wrapping_add(l);
        m ^= l;
        self.lo = l.rotate_left(49) ^ m ^ (m << 21);
        self.hi = m.rotate_left(28);
        out
    }

    /// `nextLong()`.
    pub fn next_i64(&mut self) -> i64 {
        self.next_u64() as i64
    }

    /// `next(bits)`: the top `bits` of a draw.
    pub fn next_bits(&mut self, bits: u32) -> i32 {
        (self.next_u64() >> (64 - bits)) as i32
    }

    /// `nextInt()`.
    pub fn next_i32(&mut self) -> i32 {
        self.next_u64() as i32
    }

    /// `nextInt(bound)`: uniform in `0..bound`, with vanilla's rejection loop
    /// for bounds that do not divide the range evenly.
    ///
    /// The draw is Lemire's multiply-shift over `nextInt()`, and `nextInt()`
    /// on this generator is the **low** 32 bits of a draw (`(int) nextLong()`),
    /// not the high ones. Taking the high half instead still yields uniform
    /// values in range — so a range check cannot catch the mistake — while
    /// producing a completely different sequence, and with it a completely
    /// different noise permutation table.
    pub fn next_i32_bounded(&mut self, bound: i32) -> i32 {
        debug_assert!(bound > 0);
        let bound_u = bound as u64;
        let mut r = self.next_u64() as u32 as u64;
        let mut m = r.wrapping_mul(bound_u);
        let mut l = m & 0xFFFF_FFFF;
        if l < bound_u {
            // Reject the short tail so every value stays equally likely.
            let t = (0u64.wrapping_sub(bound_u)) % bound_u;
            while l < t {
                r = self.next_u64() as u32 as u64;
                m = r.wrapping_mul(bound_u);
                l = m & 0xFFFF_FFFF;
            }
        }
        (m >> 32) as i32
    }

    /// `nextDouble()`: uniform in `[0, 1)` from the top 53 bits.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * 1.110_223_024_625_156_5E-16
    }

    /// `nextFloat()`: uniform in `[0, 1)` from the top 24 bits.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 * 5.960_464_5E-8
    }

    /// A factory for the per-field streams a noise stack forks off.
    pub fn fork_positional(&mut self) -> PositionalFactory {
        PositionalFactory {
            lo: self.next_u64(),
            hi: self.next_u64(),
        }
    }
}

/// Forks named sub-streams off one parent state.
///
/// Every noise field in the router names itself (`minecraft:temperature`,
/// `minecraft:ridge`, …) and gets a stream keyed by that name, so adding a
/// field cannot shift the streams of the fields around it.
#[derive(Debug, Clone, Copy)]
pub struct PositionalFactory {
    lo: u64,
    hi: u64,
}

impl PositionalFactory {
    /// The stream for `name`, keyed by the MD5 of the string.
    pub fn from_hash_of(&self, name: &str) -> XoroshiroRandom {
        let digest = md5(name.as_bytes());
        let a = u64::from_be_bytes(digest[0..8].try_into().unwrap());
        let b = u64::from_be_bytes(digest[8..16].try_into().unwrap());
        XoroshiroRandom::from_state(a ^ self.lo, b ^ self.hi)
    }

    /// The stream for a block position.
    pub fn at(&self, x: i32, y: i32, z: i32) -> XoroshiroRandom {
        let seed = block_seed(x, y, z);
        XoroshiroRandom::from_state(seed ^ self.lo, self.hi)
    }
}

/// Vanilla's `Mth.getSeed`: fold a block position into one value.
fn block_seed(x: i32, y: i32, z: i32) -> u64 {
    let l =
        (x.wrapping_mul(3_129_871)) as i64 ^ ((z as i64).wrapping_mul(116_129_781)) ^ (y as i64);
    let l = l
        .wrapping_mul(l)
        .wrapping_mul(42_317_861)
        .wrapping_add(l.wrapping_mul(11));
    (l >> 16) as u64
}

// --- MD5 -------------------------------------------------------------------
// Needed only to key the named noise streams; nothing here is security
// sensitive. Kept local so the crate stays dependency-free.

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// MD5 digest of `input`.
pub fn md5(input: &[u8]) -> [u8; 16] {
    let k: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32);

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );

    for chunk in msg.chunks_exact(64) {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(MD5_S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_published_vectors() {
        // RFC 1321 test suite — an external check on the digest that keys
        // every named noise stream.
        let hex = |d: [u8; 16]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(md5(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            hex(md5(b"abcdefghijklmnopqrstuvwxyz")),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
    }

    #[test]
    fn xoroshiro_matches_the_published_algorithm() {
        // Reference xoroshiro128++ (Blackman & Vigna) stepped independently
        // here: rotl(s0 + s1, 17) + s0, then the state update. Agreement over
        // a long run pins the generator itself, separately from how vanilla
        // seeds it.
        let (mut s0, mut s1) = (0x0123_4567_89AB_CDEFu64, 0xFEDC_BA98_7654_3210u64);
        let mut r = XoroshiroRandom::from_state(s0, s1);
        for step in 0..1000 {
            let expect = s0.wrapping_add(s1).rotate_left(17).wrapping_add(s0);
            let t = s1 ^ s0;
            s0 = s0.rotate_left(49) ^ t ^ (t << 21);
            s1 = t.rotate_left(28);
            assert_eq!(r.next_u64(), expect, "step {step}");
        }
    }

    #[test]
    fn an_all_zero_state_is_replaced() {
        // Zero is a fixed point: left alone it would emit nothing but zeroes.
        let mut r = XoroshiroRandom::from_state(0, 0);
        assert_ne!(r.next_u64(), 0);
    }

    #[test]
    fn named_streams_are_independent() {
        let mut parent = XoroshiroRandom::new(42);
        let f = parent.fork_positional();
        let mut a = f.from_hash_of("minecraft:temperature");
        let mut b = f.from_hash_of("minecraft:vegetation");
        assert_ne!(a.next_u64(), b.next_u64());
        // Same name, same stream — the property the whole router relies on.
        let mut a2 = f.from_hash_of("minecraft:temperature");
        let mut a3 = f.from_hash_of("minecraft:temperature");
        assert_eq!(a2.next_u64(), a3.next_u64());
    }

    #[test]
    fn bounded_draws_stay_in_range() {
        let mut r = XoroshiroRandom::new(7);
        for _ in 0..10_000 {
            let v = r.next_i32_bounded(37);
            assert!((0..37).contains(&v));
        }
    }

    #[test]
    fn doubles_stay_in_the_unit_interval() {
        let mut r = XoroshiroRandom::new(9);
        for _ in 0..10_000 {
            let v = r.next_f64();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn the_seeded_stream_matches_the_game() {
        // Reference values printed by the game's own `XoroshiroRandomSource`,
        // not by this crate — the seeding path and every draw helper on top of
        // it have to agree with the real thing, and only the real thing can
        // say so.
        //
        // ```java
        // XoroshiroRandomSource r = new XoroshiroRandomSource(42L);
        // System.out.printf("0x%016x%n", r.nextLong());
        // ```
        let mut r = XoroshiroRandom::new(42);
        for want in [
            0xbed4_a3d4_69c5_d91fu64,
            0x65e3_01cb_50e8_f4ab,
            0x9752_d3d4_db9a_2abd,
            0x43d8_d313_7b6e_0186,
        ] {
            assert_eq!(r.next_u64(), want);
        }

        // `nextInt(bound)` is the draw that seeds every noise permutation
        // table, and it reads the *low* half of a draw. A high-half version
        // stays perfectly in range, so only a reference sequence catches it.
        let mut r = XoroshiroRandom::new(42);
        let want = [105, 80, 217, 121, 167, 71, 233, 101, 181, 185];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(r.next_i32_bounded(256 - i as i32), *w, "draw {i}");
        }

        let mut r = XoroshiroRandom::new(42);
        for want in [
            7.454_321_282_946_447e-1,
            3.979_951_020_600_401e-1,
            5.911_075_968_429_961e-1,
        ] {
            assert_eq!(r.next_f64(), want);
        }
    }
}
