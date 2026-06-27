//! Shared, **language-parametric** harness for the per-language resolution
//! accuracy gates (`accuracy_gate.rs` = TS, `py_accuracy_gate.rs` = Python,
//! `rust_accuracy_gate.rs` = Rust).
//!
//! This is the generalization of the corpus driver that used to live, TS-coupled,
//! inside `accuracy_gate.rs`. The differential **core** (`resolve_differential` /
//! `resolve_differential_graph` / `accuracy_report` / `by_band`) is reused
//! UNCHANGED; what is factored here is the *driver + report + gate* scaffolding
//! every language shares:
//!
//! - [`AccuracyDoc`] — parse the committed `*-resolution.json` and assert the
//!   live [`AccuracyReport`] equals it (consistency: the published numbers can
//!   never silently drift), plus render the human tables for the gate log.
//! - [`assert_band_floors`] — gate each band with ≥ [`MIN_GATED_SITES`]
//!   adjudicable sites at its documented floor, and assert the §4.1 monotonicity
//!   invariant. A band below the gating threshold is reported but not gated
//!   (the small-corpus honesty rule).
//! - [`assert_band_nonvacuous`] — pin that the bands the language *claims* to
//!   calibrate actually carry adjudicable sites (so a "measured" number is never
//!   secretly vacuous).
//!
//! The TS gate keeps producing its outcomes through `resolve_differential` (the
//! builder-faithful path); the Python/Rust gates produce theirs through
//! `resolve_differential_graph` (the assembled-graph path). Both hand the
//! resulting `Vec<SiteOutcome>` to the **same** `accuracy_report`, so every
//! number in every language is computed by identical code.
#![allow(dead_code)] // each gate test uses a subset of these helpers.

use std::path::Path;

use serde_json::Value;
use strata_index::{AccuracyReport, Band};

/// A band is gated (and required non-vacuous) only when it has at least this many
/// SCIP-adjudicable sites — below it the sample is too small to mean anything, so
/// the band is reported but not gated (mirrors the TS small-corpus caveat).
pub const MIN_GATED_SITES: usize = 5;

/// 2-dp metric numbers compare within half a percentage point (matches the TS
/// gate's `EPS`).
pub const EPS: f64 = 5e-3;

/// Render the overall + per-band lines of a report for the gate log (printed on
/// success with `--nocapture`, and on failure by the assertions).
pub fn render_report(report: &AccuracyReport) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "overall: precision {:.4}, recall {:.4}, covered {}, uncovered {}\n",
        report.overall_precision,
        report.overall_recall,
        report.covered_sites,
        report.uncovered_sites
    ));
    s.push_str("band       sites  confirmed  denied  precision  unadjudicable\n");
    for m in &report.by_band {
        let prec = match m.precision {
            Some(p) => format!("{p:>5.2}"),
            None => "   --".to_string(),
        };
        s.push_str(&format!(
            "{:<9}  {:>4}   {:>8}  {:>5}     {}      {:>6}\n",
            m.band.name(),
            m.sites,
            m.confirmed,
            m.denied,
            prec,
            m.unadjudicable,
        ));
    }
    s
}

/// The committed machine-readable accuracy doc (`*-resolution.json`), with the
/// consistency check against a live report.
pub struct AccuracyDoc {
    doc: Value,
    path: String,
}

impl AccuracyDoc {
    /// Load the JSON doc at `manifest_relative` (relative to `CARGO_MANIFEST_DIR`).
    pub fn load(manifest_relative: &str) -> AccuracyDoc {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest_relative);
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "committed accuracy JSON missing at {}: {e}. Regenerate it from the live report.",
                path.display()
            )
        });
        let doc: Value = serde_json::from_str(&raw).expect("accuracy JSON parses");
        AccuracyDoc {
            doc,
            path: path.display().to_string(),
        }
    }

    /// Assert the committed doc's overall, coverage, and per-band numbers equal
    /// the live `report` — so the published report cannot silently drift from the
    /// code. (The per-band view is the calibration unit every language publishes;
    /// the TS doc additionally carries a `per_class` block, checked by the TS
    /// gate itself.)
    pub fn assert_matches(&self, report: &AccuracyReport) {
        let doc = &self.doc;
        assert_close(
            doc["overall"]["precision"].as_f64().unwrap(),
            report.overall_precision,
            "overall precision",
        );
        assert_close(
            doc["overall"]["recall"].as_f64().unwrap(),
            report.overall_recall,
            "overall recall",
        );
        assert_eq!(
            doc["covered_sites"].as_u64().unwrap() as usize,
            report.covered_sites,
            "covered_sites must match the doc ({})",
            self.path
        );
        assert_eq!(
            doc["uncovered_sites"].as_u64().unwrap() as usize,
            report.uncovered_sites,
            "uncovered_sites must match the doc ({})",
            self.path
        );

        let doc_bands = doc["by_band"].as_array().expect("by_band array");
        assert_eq!(
            doc_bands.len(),
            report.by_band.len(),
            "by_band count must match the doc ({})",
            self.path
        );
        for m in &report.by_band {
            let entry = doc_bands
                .iter()
                .find(|e| e["band"].as_str() == Some(m.band.name()))
                .unwrap_or_else(|| panic!("doc missing band {}", m.band.name()));
            assert_eq!(
                entry["sites"].as_u64().unwrap() as usize,
                m.sites,
                "{} band sites must match the doc",
                m.band.name()
            );
            assert_eq!(
                entry["confirmed"].as_u64().unwrap() as usize,
                m.confirmed,
                "{} band confirmed must match the doc",
                m.band.name()
            );
            assert_eq!(
                entry["denied"].as_u64().unwrap() as usize,
                m.denied,
                "{} band denied must match the doc",
                m.band.name()
            );
            assert_eq!(
                entry["unadjudicable"].as_u64().unwrap() as usize,
                m.unadjudicable,
                "{} band unadjudicable must match the doc",
                m.band.name()
            );
            match (entry["precision"].as_f64(), m.precision) {
                (Some(doc_p), Some(live_p)) => assert_close(doc_p, live_p, m.band.name()),
                (None, None) => {}
                (doc_p, live_p) => panic!(
                    "{} band precision defined-ness mismatch: doc {doc_p:?} vs live {live_p:?}",
                    m.band.name()
                ),
            }
        }
    }
}

/// A per-band floor for the gate: the band, its documented floor, and the
/// measured value the floor was derived from (recorded for the failure message).
pub struct BandFloor {
    pub band: Band,
    pub floor: f64,
}

/// Gate each band in `floors` that has ≥ [`MIN_GATED_SITES`] adjudicable sites at
/// its floor, and assert the §4.1 monotonicity invariant. A band below the
/// gating threshold is skipped (reported, not gated). Returns the rendered table
/// for logging.
pub fn assert_band_floors(report: &AccuracyReport, floors: &[BandFloor]) {
    for f in floors {
        let Some(m) = report.band(f.band) else {
            continue;
        };
        if m.sites < MIN_GATED_SITES {
            continue; // too few adjudicable sites to gate — small-corpus caveat.
        }
        let precision = m.precision.unwrap_or_else(|| {
            panic!(
                "gated band {} has {} sites but no precision",
                f.band.name(),
                m.sites
            )
        });
        assert!(
            precision + EPS >= f.floor,
            "{} band precision {:.4} below floor {} ({} adjudicable sites, {} unadjudicable)\n{}",
            f.band.name(),
            precision,
            f.floor,
            m.sites,
            m.unadjudicable,
            render_report(report),
        );
    }

    report.check_band_monotonicity().unwrap_or_else(|e| {
        panic!(
            "band monotonicity invariant violated: {e}\n{}",
            render_report(report)
        )
    });
}

/// Pin that each band named in `bands` carries at least [`MIN_GATED_SITES`]
/// adjudicable sites — the non-vacuity guard, so a "measured precision" the
/// report publishes is never secretly computed over too few sites (or zero, which
/// would be the vacuous `None`).
pub fn assert_band_nonvacuous(report: &AccuracyReport, bands: &[Band]) {
    for &band in bands {
        let m = report
            .band(band)
            .unwrap_or_else(|| panic!("report is missing band {}", band.name()));
        assert!(
            m.sites >= MIN_GATED_SITES,
            "{} band is vacuous: only {} adjudicable sites (need ≥ {MIN_GATED_SITES}). \
             Either enrich the corpus or stop gating this band.\n{}",
            band.name(),
            m.sites,
            render_report(report),
        );
        assert!(
            m.precision.is_some(),
            "{} band has sites but undefined precision — impossible if adjudicable",
            band.name()
        );
    }
}

/// Assert two 2-dp metric numbers agree within [`EPS`].
pub fn assert_close(doc: f64, live: f64, what: &str) {
    assert!(
        (doc - live).abs() <= EPS,
        "{what}: committed {doc:.4} != live {live:.4}"
    );
}
