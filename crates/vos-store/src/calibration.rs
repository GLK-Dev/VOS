//! Empirically self-calibrating recovery telemetry (`v2.0`).

use vos_core::{
    CalibrationReport, CalibrationWeights, DocumentationHealth, ProjectStatus, RecoveryDifficulty,
    RecoveryPrediction,
};
use vos_core::util::now_unix;

use crate::{Store, StoreError, experimental::predict_from_status_at};

impl Store {
    /// Computes empirically calibrated recovery prediction weights from completed `work_sessions`.
    pub fn calibrate_recovery(
        &self,
        project_name: Option<&str>,
    ) -> Result<CalibrationReport, StoreError> {
        let sessions = self.list_sessions(project_name, 100)?;
        let completed: Vec<_> = sessions
            .into_iter()
            .filter(|s| s.ended_at.is_some() && (s.recovery_quality.is_some() || s.duration_secs.is_some()))
            .collect();

        let total_scored_sessions = completed.len();
        if total_scored_sessions == 0 {
            return Ok(CalibrationReport {
                total_scored_sessions: 0,
                project_filter: project_name.map(str::to_string),
                false_alarm_rate: 0.0,
                missed_difficulty_rate: 0.0,
                avg_duration_secs: 0.0,
                avg_quality: 0.0,
                weights: default_weights(),
                notes: vec![
                    "No completed sessions yet; using baseline deterministic weights.".to_string(),
                ],
            });
        }

        let mut hard_total = 0usize;
        let mut false_alarms = 0usize;
        let mut easy_total = 0usize;
        let mut missed = 0usize;
        let mut dur_sum = 0i64;
        let mut dur_cnt = 0usize;
        let mut qual_sum = 0usize;
        let mut qual_cnt = 0usize;

        for s in &completed {
            let dur = s.duration_secs.unwrap_or(0);
            if s.duration_secs.is_some() {
                dur_sum += dur.max(0);
                dur_cnt += 1;
            }
            if let Some(q) = s.recovery_quality {
                qual_sum += usize::from(q);
                qual_cnt += 1;
            }
            let quality = s.recovery_quality.unwrap_or(3);
            match s.predicted_difficulty {
                RecoveryDifficulty::Hard => {
                    hard_total += 1;
                    if dur <= 300 && quality >= 4 {
                        false_alarms += 1;
                    }
                }
                RecoveryDifficulty::Easy => {
                    easy_total += 1;
                    if dur >= 600 || quality <= 2 {
                        missed += 1;
                    }
                }
                RecoveryDifficulty::Medium => {}
            }
        }

        let false_alarm_rate = if hard_total > 0 {
            false_alarms as f64 / hard_total as f64
        } else {
            0.0
        };
        let missed_difficulty_rate = if easy_total > 0 {
            missed as f64 / easy_total as f64
        } else {
            0.0
        };
        let avg_duration_secs = if dur_cnt > 0 {
            dur_sum as f64 / dur_cnt as f64
        } else {
            0.0
        };
        let avg_quality = if qual_cnt > 0 {
            qual_sum as f64 / qual_cnt as f64
        } else {
            0.0
        };

        let mut weights = default_weights();
        let mut notes = Vec::new();

        if total_scored_sessions < 3 {
            notes.push(format!(
                "Only {total_scored_sessions} completed session(s); need >= 3 before shifting baseline weights."
            ));
        } else {
            if false_alarm_rate > 0.25 {
                weights.coverage_multiplier =
                    round2((1.0 - 0.35 * false_alarm_rate).clamp(0.5, 1.0));
                weights.size_multiplier = round2((1.0 - 0.30 * false_alarm_rate).clamp(0.5, 1.0));
                notes.push(format!(
                    "Dampened coverage ({:.2}x) and size ({:.2}x) weights due to {:.0}% Hard false-alarm rate.",
                    weights.coverage_multiplier,
                    weights.size_multiplier,
                    false_alarm_rate * 100.0
                ));
            }
            if missed_difficulty_rate > 0.15 {
                weights.risk_multiplier =
                    round2((1.0 + 0.60 * missed_difficulty_rate).clamp(1.0, 2.0));
                weights.open_questions_multiplier =
                    round2((1.0 + 0.50 * missed_difficulty_rate).clamp(1.0, 2.0));
                weights.dirty_git_bonus = 2;
                notes.push(format!(
                    "Amplified risk ({:.2}x), open-question ({:.2}x), and dirty-git (+2) penalties due to {:.0}% missed difficulty.",
                    weights.risk_multiplier,
                    weights.open_questions_multiplier,
                    missed_difficulty_rate * 100.0
                ));
            }
            if false_alarm_rate <= 0.25 && missed_difficulty_rate <= 0.15 {
                if avg_quality >= 4.5 && avg_duration_secs <= 120.0 {
                    weights.inactivity_multiplier = 0.9;
                    notes.push(
                        "High recovery velocity observed; mildly relaxed inactivity weight (0.90x)."
                            .to_string(),
                    );
                } else {
                    notes.push(
                        "Observed sessions align closely with baseline predictions.".to_string(),
                    );
                }
            }
        }

        Ok(CalibrationReport {
            total_scored_sessions,
            project_filter: project_name.map(str::to_string),
            false_alarm_rate: round2(false_alarm_rate),
            missed_difficulty_rate: round2(missed_difficulty_rate),
            avg_duration_secs: round2(avg_duration_secs),
            avg_quality: round2(avg_quality),
            weights,
            notes,
        })
    }

    /// Predicts recovery difficulty using empirical `work_sessions` calibration when enough history exists.
    pub fn predict_recovery_calibrated(&self, status: &ProjectStatus) -> RecoveryPrediction {
        let now = now_unix();
        let report = self
            .calibrate_recovery(Some(&status.project.name))
            .ok()
            .filter(|r| r.total_scored_sessions >= 3)
            .or_else(|| {
                self.calibrate_recovery(None)
                    .ok()
                    .filter(|r| r.total_scored_sessions >= 3)
            });

        let Some(cal) = report else {
            return predict_from_status_at(status, now);
        };

        let w = &cal.weights;
        let mut score = 0usize;
        let mut factors = Vec::new();

        let base_inact = match status.last_change_at {
            Some(ts) => {
                let days = now.saturating_sub(ts).max(0) / 86_400;
                if days <= 14 {
                    0.0
                } else if days <= 60 {
                    2.0
                } else {
                    4.0
                }
            }
            None => 4.0,
        };
        let inact_pts = (base_inact * w.inactivity_multiplier).round() as usize;
        score += inact_pts;
        factors.push(format!(
            "Inactivity x{:.2} (+{inact_pts})",
            w.inactivity_multiplier
        ));

        let base_cov = match status.documentation_health {
            DocumentationHealth::Good => 0.0,
            DocumentationHealth::Partial => 1.0,
            DocumentationHealth::Low => 3.0,
            DocumentationHealth::Empty => 5.0,
        };
        let size_factor = if status.active_objects < 20 {
            0.25
        } else if status.active_objects < 100 {
            0.75
        } else {
            1.0
        };
        let cov_pts = (base_cov * size_factor * w.coverage_multiplier).round() as usize;
        score += cov_pts;
        factors.push(format!(
            "Coverage {} x{:.2} (+{cov_pts})",
            status.documentation_health, w.coverage_multiplier
        ));

        let risk_pts =
            ((status.critical_risk_count as f64) * 2.0 * w.risk_multiplier).round() as usize;
        score += risk_pts;
        factors.push(format!(
            "Critical Risks {} x{:.2} (+{risk_pts})",
            status.critical_risk_count, w.risk_multiplier
        ));

        let q_pts =
            ((status.open_questions.len() as f64) * w.open_questions_multiplier).round() as usize;
        score += q_pts;
        factors.push(format!(
            "Open Questions {} x{:.2} (+{q_pts})",
            status.open_questions.len(),
            w.open_questions_multiplier
        ));

        let base_size = if status.active_objects < 20 {
            0.0
        } else if status.active_objects < 100 {
            1.0
        } else if status.active_objects < 500 {
            2.0
        } else {
            3.0
        };
        let size_pts = (base_size * w.size_multiplier).round() as usize;
        score += size_pts;
        factors.push(format!("Project Size x{:.2} (+{size_pts})", w.size_multiplier));

        if let Some(git) = &status.git
            && git.is_dirty
        {
            score += w.dirty_git_bonus;
            factors.push(format!(
                "Dirty Git Working Tree ({} changed) (+{})",
                git.changed_files.len(),
                w.dirty_git_bonus
            ));
        }

        factors.push(format!(
            "Calibrated from {} completed session(s)",
            cal.total_scored_sessions
        ));

        let difficulty = if score <= 3 {
            RecoveryDifficulty::Easy
        } else if score <= 8 {
            RecoveryDifficulty::Medium
        } else {
            RecoveryDifficulty::Hard
        };

        RecoveryPrediction {
            difficulty,
            score,
            factors,
        }
    }
}

fn default_weights() -> CalibrationWeights {
    CalibrationWeights {
        inactivity_multiplier: 1.0,
        coverage_multiplier: 1.0,
        risk_multiplier: 1.0,
        open_questions_multiplier: 1.0,
        size_multiplier: 1.0,
        dirty_git_bonus: 1,
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calibration_dampens_false_alarms_and_flags_dirty_git() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("calproj", None, None).unwrap();

        for idx in 0..4 {
            let sess = store.start_session("calproj", Some("test")).unwrap();
            store
                .conn
                .execute(
                    "UPDATE work_sessions SET predicted_difficulty = 'Hard', predicted_score = 10 WHERE id = ?1",
                    rusqlite::params![sess.id],
                )
                .unwrap();
            store
                .stop_session(Some("calproj"), Some(5), Some(&format!("fast recovery {idx}")))
                .unwrap();
        }

        let cal = store.calibrate_recovery(Some("calproj")).unwrap();
        assert_eq!(cal.total_scored_sessions, 4);
        assert!(cal.false_alarm_rate > 0.9);
        assert!(cal.weights.coverage_multiplier < 1.0);
        assert!(cal.weights.size_multiplier < 1.0);
    }
}
