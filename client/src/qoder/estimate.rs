//! Estimate missing usage from a context ratio and measured per-token prices.
//! A window is inferred over a stable segment, never by choosing the smallest
//! integer-compatible denominator. Cache/output splits remain estimates.

use crate::coeffs::{CreditsField, ModelCoeff, Prices};
use tokscale_core::TokenBreakdown;

const WINDOWS: [u64; 8] = [
    128_000, 180_000, 200_000, 256_000, 400_000, 500_000, 1_000_000, 2_000_000,
];
const MAX_TOKENS: f64 = (1u64 << 52) as f64;
const INTEGER_TOLERANCE: f64 = 1e-6;

#[derive(Debug, Clone, Default)]
pub(super) struct Usage {
    pub tokens: TokenBreakdown,
    pub ratio: Option<f64>,
    pub credits: Option<f64>,
    pub original_credits: Option<f64>,
    pub reset: bool,
}

impl Usage {
    fn charge(&self, prices: &Prices) -> Option<f64> {
        match prices.credits_field {
            CreditsField::Billed => self.credits,
            CreditsField::Original => self.original_credits,
        }
        .filter(|c| c.is_finite() && *c > 0.0)
    }

    fn reported_input(&self) -> Option<i64> {
        let input = self.tokens.input.saturating_add(self.tokens.cache_read);
        (input > 0).then_some(input)
    }
}

fn input_at(usage: &Usage, window: u64) -> Option<i64> {
    let ratio = usage
        .ratio
        .filter(|r| r.is_finite() && *r > 0.0 && *r <= 1.0)?;
    let value = ratio * window as f64;
    (value > 0.0 && value < MAX_TOKENS && (value - value.round()).abs() <= INTEGER_TOLERANCE)
        .then_some(value.round() as i64)
}

fn candidates(usage: &Usage, windows: &[u64], prices: &Prices) -> Vec<u64> {
    windows
        .iter()
        .copied()
        .filter(|w| {
            let Some(input) = input_at(usage, *w) else {
                return false;
            };
            if let Some(reported) = usage.reported_input() {
                return input == reported;
            }
            let Some(charge) = usage.charge(prices) else {
                return false;
            };
            // Even a completely cached request cannot cost less than this.
            let minimum = prices.fresh_input.min(prices.cache_read) * input as f64;
            minimum.is_finite() && minimum <= charge + charge.abs() * 1e-6 + 1e-10
        })
        .collect()
}

/// None means missing evidence/ambiguous window, not a zero-token request.
pub(super) fn estimate(rows: &[Usage], coeff: &ModelCoeff) -> Vec<Option<TokenBreakdown>> {
    let mut result = vec![None; rows.len()];
    let mut windows = WINDOWS.to_vec();
    windows.extend(&coeff.windows.observed);
    for row in rows {
        if let (Some(input), Some(ratio)) = (row.reported_input(), row.ratio) {
            let window = input as f64 / ratio;
            if window.is_finite()
                && window > 0.0
                && window < MAX_TOKENS
                && (window - window.round()).abs() <= INTEGER_TOLERANCE
            {
                windows.push(window.round() as u64);
            }
        }
    }
    windows.retain(|w| *w > 0 && (*w as f64) < MAX_TOKENS);
    windows.sort_unstable();
    windows.dedup();

    let mut start = 0;
    let mut common = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let next = candidates(row, &windows, &coeff.prices);
        let shrunk = index > start
            && row
                .ratio
                .zip(rows[index - 1].ratio)
                .is_some_and(|(now, prev)| now < prev);
        let intersection: Vec<_> = common
            .iter()
            .copied()
            .filter(|w| next.contains(w))
            .collect();
        if row.reset || shrunk || next.is_empty() || (!common.is_empty() && intersection.is_empty())
        {
            fill_segment(
                &rows[start..index],
                &common,
                &coeff.prices,
                &mut result[start..index],
            );
            start = index;
            common = next;
        } else {
            common = if common.is_empty() {
                next
            } else {
                intersection
            };
        }
        if common.is_empty() {
            start = index + 1;
        }
    }
    fill_segment(&rows[start..], &common, &coeff.prices, &mut result[start..]);
    result
}

fn fill_segment(
    rows: &[Usage],
    windows: &[u64],
    prices: &Prices,
    result: &mut [Option<TokenBreakdown>],
) {
    let Some(window) = select_window(rows, windows, prices) else {
        return;
    };
    let mut previous = None;
    for (row, target) in rows.iter().zip(result) {
        let Some(input) = input_at(row, window) else {
            previous = None;
            continue;
        };
        if row.tokens.total() == 0 {
            *target = row
                .charge(prices)
                .and_then(|charge| split_tokens(input, previous, charge, prices));
        }
        previous = Some(input);
    }
}

fn select_window(rows: &[Usage], windows: &[u64], prices: &Prices) -> Option<u64> {
    if windows.len() == 1 {
        return windows.first().copied();
    }
    if rows.len() < 2 {
        return None;
    }
    let mut scored = Vec::new();
    for &window in windows {
        let mut previous = None;
        let mut negative = 0usize;
        for row in rows {
            let input = input_at(row, window)?;
            if row.tokens.total() == 0 {
                if let (Some(prev), Some(charge)) = (previous, row.charge(prices)) {
                    let cached = input.min(prev);
                    let base = prices.fresh_input * (input - cached) as f64
                        + prices.cache_read * cached as f64;
                    negative += usize::from(base > charge + charge.abs() * 1e-6 + 1e-10);
                }
            }
            previous = Some(input);
        }
        scored.push((negative, window));
    }
    scored.sort_unstable();
    match scored.as_slice() {
        [(score, window), (next, _), ..] if score < next => Some(*window),
        _ => None,
    }
}

/// Project the cache-chain guess onto the range allowed by a nonnegative output.
/// Cold-start cache hits are possible; a negative residual must not be hidden
/// while leaving an impossible (input, cache, output) split behind.
fn split_tokens(
    input: i64,
    previous: Option<i64>,
    charge: f64,
    prices: &Prices,
) -> Option<TokenBreakdown> {
    let i = input as f64;
    let base = prices.fresh_input * i;
    if !base.is_finite() {
        return None;
    }
    let difference = prices.fresh_input - prices.cache_read;
    let mut lower: f64 = 0.0;
    let mut upper = i;
    if difference > 0.0 {
        lower = ((base - charge) / difference).max(0.0).ceil();
    } else if difference < 0.0 {
        upper = ((charge - base) / -difference).min(i).floor();
    } else if base > charge + 1e-10 {
        return None;
    }
    if !lower.is_finite() || !upper.is_finite() || lower > upper || upper < 0.0 {
        return None;
    }
    let guess = previous.filter(|p| *p <= input).unwrap_or(0) as f64;
    let cached = guess.clamp(lower, upper) as i64;
    let output =
        (charge - prices.fresh_input * (input - cached) as f64 - prices.cache_read * cached as f64)
            / prices.output;
    if !output.is_finite() || output < -INTEGER_TOLERANCE || output >= MAX_TOKENS {
        return None;
    }
    Some(TokenBreakdown {
        input: input - cached,
        output: output.max(0.0).round() as i64,
        cache_read: cached,
        cache_write: 0,
        reasoning: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coeff() -> ModelCoeff {
        serde_json::from_str(
            r#"{"prices":{"freshInput":1,"output":5,"cacheRead":0.1},"windows":{"observed":[]}}"#,
        )
        .unwrap()
    }

    fn unknown(input: i64, window: u64, credits: f64) -> Usage {
        Usage {
            ratio: Some(input as f64 / window as f64),
            credits: Some(credits),
            ..Usage::default()
        }
    }

    #[test]
    fn uses_later_evidence_to_resolve_an_ambiguous_first_window() {
        let rows = [
            unknown(7904, 400_000, 2624.0),
            unknown(8001, 400_000, 1046.0),
        ];
        let got = estimate(&rows, &coeff());
        for (tokens, input) in got.iter().zip([7904, 8001]) {
            let tokens = tokens.as_ref().unwrap();
            assert_eq!(tokens.input + tokens.cache_read, input);
        }
    }

    #[test]
    fn never_picks_the_smallest_of_equally_valid_windows() {
        let rows = [unknown(2000, 400_000, 10000.0)];
        assert!(estimate(&rows, &coeff())[0].is_none());
    }

    #[test]
    fn observed_tokens_anchor_windows_without_being_replaced() {
        let mut anchor = unknown(7904, 400_000, 2624.0);
        anchor.tokens = TokenBreakdown {
            input: 1904,
            cache_read: 6000,
            output: 24,
            ..TokenBreakdown::default()
        };
        let got = estimate(&[anchor, unknown(8001, 400_000, 1046.0)], &coeff());
        assert!(got[0].is_none());
        let t = got[1].as_ref().unwrap();
        assert_eq!(t.input + t.cache_read, 8001);
    }

    #[test]
    fn original_credits_are_not_silently_replaced_by_discounted_credits() {
        let mut coeff = coeff();
        coeff.prices.credits_field = CreditsField::Original;
        let mut row = unknown(7904, 400_000, 1.0);
        assert!(estimate(&[row.clone()], &coeff)[0].is_none());
        row.original_credits = Some(2624.0);
        let mut second = unknown(8001, 400_000, 1.0);
        second.original_credits = Some(1046.0);
        assert!(estimate(&[row, second], &coeff).iter().all(Option::is_some));
    }

    #[test]
    fn rejects_missing_ratio_and_impossible_or_nonfinite_usage() {
        for (ratio, credits) in [
            (None, 10.0),
            (Some(f64::NAN), 10.0),
            (Some(0.1), 0.001),
            (Some(0.1), f64::INFINITY),
        ] {
            let row = Usage {
                ratio,
                credits: Some(credits),
                ..Usage::default()
            };
            assert!(estimate(&[row], &coeff())[0].is_none());
        }
    }

    #[test]
    fn resets_the_cache_guess_when_input_shrinks() {
        let t = split_tokens(1000, Some(5000), 1050.0, &coeff().prices).unwrap();
        assert_eq!(t.cache_read, 0);
        assert_eq!(t.input, 1000);
        assert_eq!(t.output, 10);
    }

    #[test]
    fn cold_start_cache_hits_produce_a_feasible_split() {
        let prices = coeff().prices;
        let t = split_tokens(7904, None, 2624.0, &prices).unwrap();
        assert!(t.cache_read > 0);
        assert_eq!(t.input + t.cache_read, 7904);
        let cost = prices.fresh_input * t.input as f64
            + prices.cache_read * t.cache_read as f64
            + prices.output * t.output as f64;
        assert!((cost - 2624.0).abs() <= prices.output / 2.0);
    }
}
