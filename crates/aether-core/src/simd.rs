//! Runtime SIMD dispatch scaffold.
//!
//! The engine stores every sub-chunk property (solid / collision / redstone …)
//! as a **Structure-of-Arrays bit mask**: 4096 bits = `[u64; 64]`. Combining
//! those masks (`a & b`, `a | b`, `a & !b`, `popcount`) is the single most
//! common inner loop in physics, lighting and redstone, so it is the natural
//! place to prove out the SIMD ladder.
//!
//! Each operation has:
//! * a portable **scalar** implementation (the correctness oracle), and
//! * accelerated **SSE4.2** (128-bit) and **AVX2** (256-bit) paths.
//!
//! [`dispatch`] picks the best implementation **once** from the CPU's reported
//! features and hands back a `&'static dyn MaskOps`. Callers keep the reference;
//! there is no per-call feature check.
//!
//! AVX-512 is *detected* (see [`Backend::detect`]) but, until the wider path is
//! validated against the scalar oracle, it is routed to the AVX2 implementation.
//! This is a documented Phase 0 scaffold gap, not a defect.

/// Which backend a [`MaskOps`] implementation represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Portable fallback; always available.
    Scalar,
    /// 128-bit SSE4.2.
    Sse42,
    /// 256-bit AVX2.
    Avx2,
    /// 512-bit AVX-512 (detected; currently serviced by the AVX2 path).
    Avx512,
}

impl Backend {
    /// The best backend the *current* CPU can run.
    ///
    /// On non-x86 targets this is always [`Backend::Scalar`].
    pub fn detect() -> Backend {
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("avx512f") {
                return Backend::Avx512;
            }
            if std::is_x86_feature_detected!("avx2") {
                return Backend::Avx2;
            }
            if std::is_x86_feature_detected!("sse4.2") {
                return Backend::Sse42;
            }
        }
        Backend::Scalar
    }

    /// Human-readable name for telemetry / logging.
    pub const fn name(self) -> &'static str {
        match self {
            Backend::Scalar => "scalar",
            Backend::Sse42 => "sse4.2",
            Backend::Avx2 => "avx2",
            Backend::Avx512 => "avx512",
        }
    }
}

/// Bit-mask operations over Structure-of-Arrays sub-chunk masks (`[u64]`).
///
/// All methods require `dst`, `a` and `b` to have the **same length**; they
/// panic otherwise. Lengths are otherwise unconstrained so the same trait
/// serves 64-word sub-chunk masks and smaller scratch buffers alike.
pub trait MaskOps: Send + Sync {
    /// Which backend this implementation is.
    fn backend(&self) -> Backend;
    /// `dst = a & b`
    fn and_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]);
    /// `dst = a | b`
    fn or_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]);
    /// `dst = a & !b`
    fn andnot_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]);
    /// Number of set bits across the whole slice.
    fn popcount(&self, a: &[u64]) -> u32;
}

#[inline]
fn check_lens(dst: &[u64], a: &[u64], b: &[u64]) {
    assert!(
        dst.len() == a.len() && a.len() == b.len(),
        "mask length mismatch: dst={}, a={}, b={}",
        dst.len(),
        a.len(),
        b.len()
    );
}

/// Portable scalar implementation — the correctness oracle for the SIMD paths.
pub struct Scalar;

impl MaskOps for Scalar {
    fn backend(&self) -> Backend {
        Backend::Scalar
    }
    fn and_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
        check_lens(dst, a, b);
        for i in 0..dst.len() {
            dst[i] = a[i] & b[i];
        }
    }
    fn or_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
        check_lens(dst, a, b);
        for i in 0..dst.len() {
            dst[i] = a[i] | b[i];
        }
    }
    fn andnot_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
        check_lens(dst, a, b);
        for i in 0..dst.len() {
            dst[i] = a[i] & !b[i];
        }
    }
    fn popcount(&self, a: &[u64]) -> u32 {
        a.iter().map(|w| w.count_ones()).sum()
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{check_lens, Backend, MaskOps};
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    /// 128-bit SSE4.2 path: two `u64` lanes per step.
    pub struct Sse42;

    impl MaskOps for Sse42 {
        fn backend(&self) -> Backend {
            Backend::Sse42
        }
        fn and_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
            check_lens(dst, a, b);
            // SAFETY: `dispatch` only hands out `Sse42` when `sse4.2` (which
            // implies `sse2`, the feature these intrinsics need) is present.
            unsafe { sse_binop(dst, a, b, BinOp::And) }
        }
        fn or_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
            check_lens(dst, a, b);
            // SAFETY: see `and_into`.
            unsafe { sse_binop(dst, a, b, BinOp::Or) }
        }
        fn andnot_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
            check_lens(dst, a, b);
            // SAFETY: see `and_into`.
            unsafe { sse_binop(dst, a, b, BinOp::AndNot) }
        }
        fn popcount(&self, a: &[u64]) -> u32 {
            a.iter().map(|w| w.count_ones()).sum()
        }
    }

    /// 256-bit AVX2 path: four `u64` lanes per step.
    pub struct Avx2;

    impl MaskOps for Avx2 {
        fn backend(&self) -> Backend {
            Backend::Avx2
        }
        fn and_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
            check_lens(dst, a, b);
            // SAFETY: `dispatch` only hands out `Avx2` when `avx2` is present.
            unsafe { avx2_binop(dst, a, b, BinOp::And) }
        }
        fn or_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
            check_lens(dst, a, b);
            // SAFETY: see `and_into`.
            unsafe { avx2_binop(dst, a, b, BinOp::Or) }
        }
        fn andnot_into(&self, dst: &mut [u64], a: &[u64], b: &[u64]) {
            check_lens(dst, a, b);
            // SAFETY: see `and_into`.
            unsafe { avx2_binop(dst, a, b, BinOp::AndNot) }
        }
        fn popcount(&self, a: &[u64]) -> u32 {
            a.iter().map(|w| w.count_ones()).sum()
        }
    }

    #[derive(Clone, Copy)]
    enum BinOp {
        And,
        Or,
        AndNot,
    }

    #[target_feature(enable = "sse4.2")]
    unsafe fn sse_binop(dst: &mut [u64], a: &[u64], b: &[u64], op: BinOp) {
        let n = dst.len();
        let chunks = n / 2;
        for i in 0..chunks {
            let off = i * 2;
            // SAFETY: `off + 2 <= n`, all three slices are `n` long.
            let va = _mm_loadu_si128(a.as_ptr().add(off) as *const __m128i);
            let vb = _mm_loadu_si128(b.as_ptr().add(off) as *const __m128i);
            let vr = match op {
                BinOp::And => _mm_and_si128(va, vb),
                BinOp::Or => _mm_or_si128(va, vb),
                // `_mm_andnot_si128(x, y)` computes `!x & y`, so pass `b` first.
                BinOp::AndNot => _mm_andnot_si128(vb, va),
            };
            _mm_storeu_si128(dst.as_mut_ptr().add(off) as *mut __m128i, vr);
        }
        for i in (chunks * 2)..n {
            dst[i] = match op {
                BinOp::And => a[i] & b[i],
                BinOp::Or => a[i] | b[i],
                BinOp::AndNot => a[i] & !b[i],
            };
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn avx2_binop(dst: &mut [u64], a: &[u64], b: &[u64], op: BinOp) {
        let n = dst.len();
        let chunks = n / 4;
        for i in 0..chunks {
            let off = i * 4;
            // SAFETY: `off + 4 <= n`, all three slices are `n` long.
            let va = _mm256_loadu_si256(a.as_ptr().add(off) as *const __m256i);
            let vb = _mm256_loadu_si256(b.as_ptr().add(off) as *const __m256i);
            let vr = match op {
                BinOp::And => _mm256_and_si256(va, vb),
                BinOp::Or => _mm256_or_si256(va, vb),
                BinOp::AndNot => _mm256_andnot_si256(vb, va),
            };
            _mm256_storeu_si256(dst.as_mut_ptr().add(off) as *mut __m256i, vr);
        }
        for i in (chunks * 4)..n {
            dst[i] = match op {
                BinOp::And => a[i] & b[i],
                BinOp::Or => a[i] | b[i],
                BinOp::AndNot => a[i] & !b[i],
            };
        }
    }
}

/// The selected implementation for the current CPU.
///
/// Chosen once from [`Backend::detect`]; keep the reference and reuse it.
///
/// ```
/// let ops = aether_core::simd::dispatch();
/// let a = [0b1010u64; 4];
/// let b = [0b0110u64; 4];
/// let mut out = [0u64; 4];
/// ops.and_into(&mut out, &a, &b);
/// assert_eq!(out, [0b0010u64; 4]);
/// ```
pub fn dispatch() -> &'static dyn MaskOps {
    static SCALAR: Scalar = Scalar;
    #[cfg(target_arch = "x86_64")]
    {
        static SSE42: x86::Sse42 = x86::Sse42;
        static AVX2: x86::Avx2 = x86::Avx2;
        match Backend::detect() {
            // AVX-512 is detected but routed to AVX2 until the wide path is
            // validated (documented scaffold gap).
            Backend::Avx512 | Backend::Avx2 => return &AVX2,
            Backend::Sse42 => return &SSE42,
            Backend::Scalar => return &SCALAR,
        }
    }
    #[allow(unreachable_code)]
    &SCALAR
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<u64>, Vec<u64>) {
        // 64 words = one full sub-chunk mask, plus a tail that is not a multiple
        // of the 4-lane AVX2 step, to exercise the scalar remainder.
        let a: Vec<u64> = (0..66)
            .map(|i| 0x0123_4567_89ab_cdef ^ (i as u64 * 0x9e37))
            .collect();
        let b: Vec<u64> = (0..66)
            .map(|i| 0xfedc_ba98_7654_3210 ^ (i as u64 * 0x1234))
            .collect();
        (a, b)
    }

    fn assert_matches_scalar(ops: &dyn MaskOps) {
        let (a, b) = sample();
        let scalar = Scalar;
        let n = a.len();

        let (mut got, mut want) = (vec![0u64; n], vec![0u64; n]);
        ops.and_into(&mut got, &a, &b);
        scalar.and_into(&mut want, &a, &b);
        assert_eq!(got, want, "and mismatch for {:?}", ops.backend());

        ops.or_into(&mut got, &a, &b);
        scalar.or_into(&mut want, &a, &b);
        assert_eq!(got, want, "or mismatch for {:?}", ops.backend());

        ops.andnot_into(&mut got, &a, &b);
        scalar.andnot_into(&mut want, &a, &b);
        assert_eq!(got, want, "andnot mismatch for {:?}", ops.backend());

        assert_eq!(ops.popcount(&a), scalar.popcount(&a));
    }

    #[test]
    fn dispatch_returns_something_and_matches_oracle() {
        let ops = dispatch();
        assert_matches_scalar(ops);
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn every_available_backend_matches_scalar() {
        assert_matches_scalar(&Scalar);
        if std::is_x86_feature_detected!("sse4.2") {
            assert_matches_scalar(&x86::Sse42);
        }
        if std::is_x86_feature_detected!("avx2") {
            assert_matches_scalar(&x86::Avx2);
        }
    }

    #[test]
    #[should_panic(expected = "mask length mismatch")]
    fn length_mismatch_panics() {
        let mut dst = [0u64; 3];
        Scalar.and_into(&mut dst, &[0u64; 2], &[0u64; 3]);
    }
}
