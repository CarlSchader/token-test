//! Statistical helpers for the benchmark report.

/// Compute a percentile (0..=100) of a slice of values using linear
/// interpolation between closest ranks.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.len() == 1 {
        return Some(v[0]);
    }
    let clamped = p.clamp(0.0, 100.0);
    let rank = (clamped / 100.0) * (v.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = lo + 1;
    if hi >= v.len() {
        return Some(*v.last().unwrap());
    }
    let frac = rank - lo as f64;
    Some(v[lo] + (v[hi] - v[lo]) * frac)
}

/// Format a duration in human-friendly units (ms or s).
pub fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{:.0} ms", d.as_millis())
    } else {
        format!("{:.2} s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_empty() {
        assert!(percentile(&[], 50.0).is_none());
    }

    #[test]
    fn percentile_single() {
        assert_eq!(percentile(&[42.0], 99.0), Some(42.0));
    }

    #[test]
    fn percentile_known_values() {
        let v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert!((percentile(&v, 50.0).unwrap() - 50.5).abs() < 1e-9);
        assert!((percentile(&v, 0.0).unwrap() - 1.0).abs() < 1e-9);
        assert!((percentile(&v, 100.0).unwrap() - 100.0).abs() < 1e-9);
    }
}
