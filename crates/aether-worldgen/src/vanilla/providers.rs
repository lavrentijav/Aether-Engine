//! Value providers — `IntProvider`, `FloatProvider` and `HeightProvider` —
//! the little random distributions carvers and features are configured with.
//!
//! Each `sample` draws from the random source in exactly the game's order, so
//! a carver or feature fed the same stream makes the same choices.

use super::density::BuildError;
use super::json::Json;
use super::rng::Rng;
use super::surface::Anchor;

fn ty(j: &Json) -> &str {
    j.str_of("type").unwrap_or("").trim_start_matches("minecraft:")
}

/// `Mth.nextInt(r, min, max)`.
pub fn mth_next_int(r: &mut impl Rng, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        r.next_int_bounded(max - min + 1) + min
    }
}

/// `IntProvider`.
#[derive(Debug, Clone)]
pub enum IntProvider {
    /// `constant`.
    Constant(i32),
    /// `uniform`.
    Uniform(i32, i32),
    /// `biased_to_bottom`.
    BiasedToBottom(i32, i32),
    /// `clamped`.
    Clamped(Box<IntProvider>, i32, i32),
    /// `clamped_normal`.
    ClampedNormal { mean: f32, deviation: f32, min: i32, max: i32 },
    /// `weighted_list`.
    Weighted(Vec<(IntProvider, i32)>, i32),
}

impl IntProvider {
    /// Parse a provider or a bare number.
    pub fn parse(j: &Json) -> Result<Self, BuildError> {
        if let Some(v) = j.as_f64() {
            return Ok(IntProvider::Constant(v as i32));
        }
        Ok(match ty(j) {
            "constant" => IntProvider::Constant(j.i32_or("value", 0)),
            "uniform" => IntProvider::Uniform(j.i32_or("min_inclusive", 0), j.i32_or("max_inclusive", 0)),
            "biased_to_bottom" => {
                IntProvider::BiasedToBottom(j.i32_or("min_inclusive", 0), j.i32_or("max_inclusive", 0))
            }
            "clamped" => IntProvider::Clamped(
                Box::new(IntProvider::parse(
                    j.get("source").ok_or_else(|| BuildError::new("clamped int: no source"))?,
                )?),
                j.i32_or("min_inclusive", i32::MIN),
                j.i32_or("max_inclusive", i32::MAX),
            ),
            "clamped_normal" => IntProvider::ClampedNormal {
                mean: j.f64_or("mean", 0.0) as f32,
                deviation: j.f64_or("deviation", 1.0) as f32,
                min: j.i32_or("min_inclusive", 0),
                max: j.i32_or("max_inclusive", 0),
            },
            "weighted_list" => {
                let mut items = Vec::new();
                let mut total = 0;
                for e in j.get("distribution").and_then(Json::as_arr).unwrap_or(&[]) {
                    let w = e.i32_or("weight", 1);
                    let d = IntProvider::parse(e.get("data").unwrap_or(&Json::Null))?;
                    total += w;
                    items.push((d, w));
                }
                IntProvider::Weighted(items, total)
            }
            other => return Err(BuildError::new(format!("unknown int provider `{other}`"))),
        })
    }

    /// `sample`.
    pub fn sample(&self, r: &mut impl Rng) -> i32 {
        match self {
            IntProvider::Constant(v) => *v,
            IntProvider::Uniform(a, b) => r.next_int_bounded(b - a + 1) + a,
            IntProvider::BiasedToBottom(a, b) => {
                let n = r.next_int_bounded(b - a + 1) + 1;
                a + r.next_int_bounded(n)
            }
            IntProvider::Clamped(s, lo, hi) => s.sample(r).clamp(*lo, *hi),
            IntProvider::ClampedNormal { mean, deviation, min, max } => {
                let v = mean + r.next_gaussian() as f32 * deviation;
                v.clamp(*min as f32, *max as f32) as i32
            }
            IntProvider::Weighted(items, total) => {
                let mut i = r.next_int_bounded(*total);
                for (p, w) in items {
                    if i < *w {
                        return p.sample(r);
                    }
                    i -= w;
                }
                0
            }
        }
    }

    /// `getMaxValue`.
    pub fn max_value(&self) -> i32 {
        match self {
            IntProvider::Constant(v) => *v,
            IntProvider::Uniform(_, b) | IntProvider::BiasedToBottom(_, b) => *b,
            IntProvider::Clamped(s, _, hi) => s.max_value().min(*hi),
            IntProvider::ClampedNormal { max, .. } => *max,
            IntProvider::Weighted(items, _) => items.iter().map(|(p, _)| p.max_value()).max().unwrap_or(0),
        }
    }
}

/// `FloatProvider`.
#[derive(Debug, Clone)]
pub enum FloatProvider {
    /// `constant`.
    Constant(f32),
    /// `uniform`: `[min, max)`.
    Uniform(f32, f32),
    /// `clamped_normal`.
    ClampedNormal { mean: f32, deviation: f32, min: f32, max: f32 },
    /// `trapezoid`.
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatProvider {
    /// Parse a provider or a bare number.
    pub fn parse(j: &Json) -> Result<Self, BuildError> {
        if let Some(v) = j.as_f64() {
            return Ok(FloatProvider::Constant(v as f32));
        }
        Ok(match ty(j) {
            "constant" => FloatProvider::Constant(j.f64_or("value", 0.0) as f32),
            "uniform" => FloatProvider::Uniform(
                j.f64_or("min_inclusive", 0.0) as f32,
                j.f64_or("max_exclusive", 0.0) as f32,
            ),
            "clamped_normal" => FloatProvider::ClampedNormal {
                mean: j.f64_or("mean", 0.0) as f32,
                deviation: j.f64_or("deviation", 1.0) as f32,
                min: j.f64_or("min", 0.0) as f32,
                max: j.f64_or("max", 0.0) as f32,
            },
            "trapezoid" => FloatProvider::Trapezoid {
                min: j.f64_or("min", 0.0) as f32,
                max: j.f64_or("max", 0.0) as f32,
                plateau: j.f64_or("plateau", 0.0) as f32,
            },
            other => return Err(BuildError::new(format!("unknown float provider `{other}`"))),
        })
    }

    /// `sample`.
    pub fn sample(&self, r: &mut impl Rng) -> f32 {
        match self {
            FloatProvider::Constant(v) => *v,
            FloatProvider::Uniform(a, b) => r.next_float() * (b - a) + a,
            FloatProvider::ClampedNormal { mean, deviation, min, max } => {
                (mean + r.next_gaussian() as f32 * deviation).clamp(*min, *max)
            }
            FloatProvider::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let side = (range - plateau) / 2.0;
                let rest = range - side;
                min + r.next_float() * rest + r.next_float() * side
            }
        }
    }
}

/// `HeightProvider`, with its anchors resolved against the dimension.
#[derive(Debug, Clone)]
pub enum HeightProvider {
    /// `constant`.
    Constant(i32),
    /// `uniform`.
    Uniform(i32, i32),
    /// `biased_to_bottom`.
    BiasedToBottom(i32, i32, i32),
    /// `very_biased_to_bottom`.
    VeryBiasedToBottom(i32, i32, i32),
    /// `trapezoid`.
    Trapezoid(i32, i32, i32),
    /// `weighted_list`.
    Weighted(Vec<(HeightProvider, i32)>, i32),
}

impl HeightProvider {
    /// Parse, resolving anchors for a dimension `min_y .. min_y + height`.
    pub fn parse(j: &Json, min_y: i32, height: i32) -> Result<Self, BuildError> {
        let anchor = |k: &str| -> Result<i32, BuildError> {
            Ok(Anchor::parse(j.get(k).ok_or_else(|| BuildError::new(format!("height provider: no `{k}`")))?)?
                .resolve(min_y, height))
        };
        if j.get("type").is_none() {
            return Ok(HeightProvider::Constant(Anchor::parse(j)?.resolve(min_y, height)));
        }
        Ok(match ty(j) {
            "constant" => HeightProvider::Constant(anchor("value")?),
            "uniform" => HeightProvider::Uniform(anchor("min_inclusive")?, anchor("max_inclusive")?),
            "biased_to_bottom" => {
                HeightProvider::BiasedToBottom(anchor("min_inclusive")?, anchor("max_inclusive")?, j.i32_or("inner", 1))
            }
            "very_biased_to_bottom" => HeightProvider::VeryBiasedToBottom(
                anchor("min_inclusive")?,
                anchor("max_inclusive")?,
                j.i32_or("inner", 1),
            ),
            "trapezoid" => {
                HeightProvider::Trapezoid(anchor("min_inclusive")?, anchor("max_inclusive")?, j.i32_or("plateau", 0))
            }
            "weighted_list" => {
                let mut items = Vec::new();
                let mut total = 0;
                for e in j.get("distribution").and_then(Json::as_arr).unwrap_or(&[]) {
                    let w = e.i32_or("weight", 1);
                    total += w;
                    items.push((HeightProvider::parse(e.get("data").unwrap_or(&Json::Null), min_y, height)?, w));
                }
                HeightProvider::Weighted(items, total)
            }
            other => return Err(BuildError::new(format!("unknown height provider `{other}`"))),
        })
    }

    /// `sample`.
    pub fn sample(&self, r: &mut impl Rng) -> i32 {
        match self {
            HeightProvider::Constant(v) => *v,
            HeightProvider::Uniform(a, b) => {
                if a > b {
                    *a
                } else {
                    r.next_int_bounded(b - a + 1) + a
                }
            }
            HeightProvider::BiasedToBottom(a, b, inner) => {
                if b - a - inner + 1 <= 0 {
                    return *a;
                }
                let n = r.next_int_bounded(b - a - inner + 1);
                r.next_int_bounded(n + inner) + a
            }
            HeightProvider::VeryBiasedToBottom(a, b, inner) => {
                if b - a - inner + 1 <= 0 {
                    return *a;
                }
                let i = mth_next_int(r, a + inner, *b);
                let j = mth_next_int(r, *a, i - 1);
                mth_next_int(r, *a, j - 1 + inner)
            }
            HeightProvider::Trapezoid(a, b, plateau) => {
                if a > b {
                    return *a;
                }
                let range = b - a;
                if *plateau >= range {
                    return r.next_int_bounded(range + 1) + a;
                }
                let side = (range - plateau) / 2;
                let rest = range - side;
                a + r.next_int_bounded(rest + 1) + r.next_int_bounded(side + 1)
            }
            HeightProvider::Weighted(items, total) => {
                let mut i = r.next_int_bounded(*total);
                for (p, w) in items {
                    if i < *w {
                        return p.sample(r);
                    }
                    i -= w;
                }
                0
            }
        }
    }
}
