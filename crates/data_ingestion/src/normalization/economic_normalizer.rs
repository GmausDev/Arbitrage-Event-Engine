// crates/data_ingestion/src/normalization/economic_normalizer.rs

use common::events::{CalendarEvent, EconomicRelease};

use crate::connectors::{RawCalendarEntry, RawEconomicData};

pub struct EconomicNormalizer;

impl EconomicNormalizer {
    /// Convert raw economic data into `EconomicRelease` events.
    ///
    /// Items with empty indicator names are discarded.
    pub fn normalize_releases(raw: Vec<RawEconomicData>) -> Vec<EconomicRelease> {
        raw.into_iter()
            .filter_map(|r| {
                if r.indicator.is_empty() {
                    tracing::warn!("economic_normalizer: skipping release with empty indicator");
                    return None;
                }
                Some(EconomicRelease {
                    indicator: r.indicator,
                    value: r.value,
                    forecast: r.forecast,
                    previous: r.previous,
                    timestamp: r.release_time,
                })
            })
            .collect()
    }

    /// Convert raw calendar entries into `CalendarEvent` payloads.
    pub fn normalize_calendar(raw: Vec<RawCalendarEntry>) -> Vec<CalendarEvent> {
        raw.into_iter()
            .filter_map(|r| {
                if r.event_id.is_empty() || r.title.is_empty() {
                    tracing::warn!("economic_normalizer: skipping calendar entry with empty id/title");
                    return None;
                }
                Some(CalendarEvent {
                    event_id: r.event_id,
                    title: r.title,
                    scheduled_time: r.scheduled_time,
                    category: r.category,
                    expected_impact: r.expected_impact.clamp(0.0, 1.0),
                })
            })
            .collect()
    }

    /// Compute the "surprise" as `(actual - forecast) / |forecast|`.
    ///
    /// Returns `None` when no forecast is available or forecast is zero.
    pub fn surprise_factor(release: &EconomicRelease) -> Option<f64> {
        let forecast = release.forecast?;
        if forecast.abs() < f64::EPSILON {
            return None;
        }
        Some((release.value - forecast) / forecast.abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crate::connectors::RawEconomicData;

    #[test]
    fn normalize_releases_discards_empty_indicator() {
        let data = RawEconomicData {
            indicator: "".into(),
            value: 1.0,
            forecast: None,
            previous: None,
            release_time: Utc::now(),
        };
        let result = EconomicNormalizer::normalize_releases(vec![data]);
        assert!(result.is_empty());
    }

    #[test]
    fn normalize_releases_preserves_optional_fields() {
        let data = RawEconomicData {
            indicator: "US_CPI".into(),
            value: 3.1,
            forecast: Some(3.2),
            previous: Some(3.4),
            release_time: Utc::now(),
        };
        let result = EconomicNormalizer::normalize_releases(vec![data]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].forecast, Some(3.2));
        assert_eq!(result[0].previous, Some(3.4));
    }

    #[test]
    fn surprise_factor_positive_beat() {
        let release = EconomicRelease {
            indicator: "US_NFP".into(),
            value: 220_000.0,
            forecast: Some(185_000.0),
            previous: None,
            timestamp: Utc::now(),
        };
        let s = EconomicNormalizer::surprise_factor(&release).unwrap();
        assert!(s > 0.0, "upside surprise should be positive");
        assert!((s - (35_000.0 / 185_000.0)).abs() < 1e-9);
    }

    #[test]
    fn surprise_factor_none_when_no_forecast() {
        let release = EconomicRelease {
            indicator: "X".into(),
            value: 1.0,
            forecast: None,
            previous: None,
            timestamp: Utc::now(),
        };
        assert!(EconomicNormalizer::surprise_factor(&release).is_none());
    }

    #[test]
    fn normalize_calendar_clamps_impact() {
        let entry = RawCalendarEntry {
            event_id: "e1".into(),
            title: "T".into(),
            scheduled_time: Utc::now(),
            category: "test".into(),
            expected_impact: 1.5, // > 1.0
        };
        let result = EconomicNormalizer::normalize_calendar(vec![entry]);
        assert_eq!(result[0].expected_impact, 1.0);
    }
}
