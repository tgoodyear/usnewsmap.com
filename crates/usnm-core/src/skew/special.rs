//! Special functions used by the geographic-skew posterior.

/// `ln Gamma(x)` for `x > 0` (Lanczos, g = 7, n = 9; about 15 significant digits).
pub fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: Gamma(x) Gamma(1 - x) = pi / sin(pi x).
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut sum = C[0];
    for (i, c) in C.iter().enumerate().skip(1) {
        sum += c / (x + i as f64);
    }
    let t = x + G + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// Regularized lower incomplete gamma function `P(a, x)`.
pub fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let front = (-x + a * x.ln() - ln_gamma(a)).exp();
    // Both expansions need on the order of sqrt(a) terms near x = a.
    let limit = 1_000 + (50.0 * a.sqrt()) as usize;
    if x < a + 1.0 {
        // Series.
        let (mut ap, mut sum, mut del) = (a, 1.0 / a, 1.0 / a);
        for _ in 0..limit {
            ap += 1.0;
            del *= x / ap;
            sum += del;
            if del.abs() < sum.abs() * 1e-16 {
                break;
            }
        }
        (sum * front).clamp(0.0, 1.0)
    } else {
        // Continued fraction for Q(a, x) (modified Lentz).
        let tiny = 1e-300;
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / tiny;
        let mut d = 1.0 / b;
        let mut h = d;
        for i in 1..limit {
            let an = -(i as f64) * (i as f64 - a);
            b += 2.0;
            d = an * d + b;
            if d.abs() < tiny {
                d = tiny;
            }
            c = b + an / c;
            if c.abs() < tiny {
                c = tiny;
            }
            d = 1.0 / d;
            let delta = d * c;
            h *= delta;
            if (delta - 1.0).abs() < 1e-16 {
                break;
            }
        }
        (1.0 - front * h).clamp(0.0, 1.0)
    }
}

/// Inverse of the standard normal CDF (Acklam's rational approximation,
/// relative error below 1.2e-9).
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    if p < 0.02425 {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - 0.02425 {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    }
}

/// Wilson and Hilferty's approximation to the `p` quantile of `Gamma(shape, 1)`.
fn wilson_hilferty(p: f64, shape: f64) -> f64 {
    let v = 1.0 / (9.0 * shape);
    shape * (1.0 - v + normal_quantile(p) * v.sqrt()).powi(3)
}

/// Above this shape the Wilson and Hilferty approximation is used as is
/// (relative error well below 1e-6 there).
const LARGE_SHAPE: f64 = 1e4;

/// The `p` quantile of `Gamma(shape, rate = 1)`: Newton's method on
/// [`gamma_p`], kept inside a bisection bracket, from a Wilson and Hilferty
/// start; the approximation alone for very large shapes.
pub fn gamma_quantile(p: f64, shape: f64) -> f64 {
    if p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if shape > LARGE_SHAPE {
        return wilson_hilferty(p, shape);
    }
    let ln_g = ln_gamma(shape);
    let (mut lo, mut hi) = (0.0, shape.max(1.0));
    while gamma_p(shape, hi) < p {
        lo = hi;
        hi *= 2.0;
    }
    // Start: Wilson and Hilferty, or for small shapes the leading term of the
    // series, P(a, x) ~ x^a / Gamma(a + 1).
    let mut x = if shape >= 1.0 {
        wilson_hilferty(p, shape)
    } else {
        let x = ((p.ln() + ln_gamma(shape + 1.0)) / shape).exp();
        if x < f64::MIN_POSITIVE {
            // Below the smallest normal f64: as close to 0 as it gets.
            return x;
        }
        x
    };
    if !(x > lo && x < hi) {
        x = (lo + hi) / 2.0;
    }
    for _ in 0..200 {
        let f = gamma_p(shape, x) - p;
        if f < 0.0 {
            lo = x;
        } else {
            hi = x;
        }
        let pdf = ((shape - 1.0) * x.ln() - x - ln_g).exp();
        let mut next = x - f / pdf;
        if !(next > lo && next < hi) {
            next = (lo + hi) / 2.0;
        }
        if (next - x).abs() <= 1e-13 * x || hi - lo <= 1e-13 * hi {
            return next;
        }
        x = next;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    #[test]
    fn ln_gamma_matches_known_values() {
        assert!(close(ln_gamma(1.0), 0.0, 1e-13));
        assert!(close(ln_gamma(5.0), 24f64.ln(), 1e-13));
        assert!(close(
            ln_gamma(0.5),
            std::f64::consts::PI.sqrt().ln(),
            1e-13
        ));
        // ln(99!) = 359.1342053695754.
        assert!(close(ln_gamma(100.0), 359.134_205_369_575_4, 1e-13));
        assert!(close(ln_gamma(0.1), 2.252_712_651_734_206, 1e-12));
    }

    #[test]
    fn gamma_p_matches_known_values() {
        // P(1, x) = 1 - e^-x.
        for x in [0.1, 1.0, 3.0, 20.0] {
            assert!(close(gamma_p(1.0, x), 1.0 - (-x).exp(), 1e-12));
        }
        // Chi-square with k df: CDF(x) = P(k/2, x/2); 95th percentiles.
        assert!(close(gamma_p(0.5, 3.841_458_821 / 2.0), 0.95, 1e-9));
        assert!(close(gamma_p(1.0, 5.991_464_547 / 2.0), 0.95, 1e-9));
        assert!(close(gamma_p(5.0, 18.307_038_05 / 2.0), 0.95, 1e-9));
        assert!(close(gamma_p(50.0, 124.342_113_4 / 2.0), 0.95, 1e-7));
        assert!(close(gamma_p(500.0, 1_074.679_449 / 2.0), 0.95, 1e-6));
    }

    #[test]
    fn normal_quantile_matches_known_values() {
        assert!(close(normal_quantile(0.5), 0.0, 1e-12));
        assert!(close(normal_quantile(0.975), 1.959_963_985, 1e-8));
        assert!(close(normal_quantile(0.05), -1.644_853_627, 1e-8));
        assert!(close(normal_quantile(0.001), -3.090_232_306, 1e-8));
    }

    #[test]
    fn gamma_quantile_inverts_gamma_p() {
        for shape in [0.01, 0.05, 0.5, 1.0, 3.0, 40.0, 999.0, 9_999.0] {
            for p in [0.05, 0.5, 0.95] {
                let q = gamma_quantile(p, shape);
                assert!(close(gamma_p(shape, q), p, 1e-9), "shape {shape} p {p}");
            }
        }
        // Below that the lower quantiles underflow (the 5% point of
        // Gamma(0.001) is about 1e-1301); they come out as 0 or close to it.
        assert!(gamma_quantile(0.05, 0.001) < 1e-300);
        assert!(close(
            gamma_p(0.001, gamma_quantile(0.95, 0.001)),
            0.95,
            1e-9
        ));
        // Very large shapes use Wilson and Hilferty: the median of
        // Gamma(k) is k - 1/3 + 8 / (405 k) + O(1/k^2).
        for k in [2e4, 1e6, 1e7] {
            let m = k - 1.0 / 3.0 + 8.0 / (405.0 * k);
            assert!(close(gamma_quantile(0.5, k), m, 1e-9), "k {k}");
        }
        // ... and agree with the exact inversion just above the switch.
        let k = 1.5e4;
        let exact = {
            let (mut lo, mut hi) = (0.9 * k, 1.1 * k);
            for _ in 0..100 {
                let mid = (lo + hi) / 2.0;
                if gamma_p(k, mid) < 0.05 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            (lo + hi) / 2.0
        };
        assert!(close(gamma_quantile(0.05, k), exact, 1e-7));
    }
}
