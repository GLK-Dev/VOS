use vos_core::util::now_unix;
use vos_core::{DocumentationHealth, ProjectStatus, RecoveryDifficulty, RecoveryPrediction};

use crate::Store;

impl Store {
    /// Predicts project re-entry difficulty (`Easy`, `Medium`, `Hard`) from existing status signals.
    pub fn predict_recovery_difficulty(&self, status: &ProjectStatus) -> RecoveryPrediction {
        predict_from_status_at(status, now_unix())
    }
}

pub(crate) fn predict_from_status_at(status: &ProjectStatus, now: i64) -> RecoveryPrediction {
    let mut score = 0_usize;
    let mut factors = Vec::new();

    let (inactivity_pts, inactivity_label) = match status.last_change_at {
        Some(ts) => {
            let days = now.saturating_sub(ts).max(0) / 86_400;
            if days <= 14 {
                (0, format!("Recent activity ({days} days ago) (+0)"))
            } else if days <= 60 {
                (2, format!("Warm inactivity ({days} days ago) (+2)"))
            } else {
                (4, format!("Dormant {days} days (+4)"))
            }
        }
        None => (4, "No recorded activity (+4)".to_string()),
    };
    score += inactivity_pts;
    factors.push(inactivity_label);

    let base_cov_pts: f64 = match status.documentation_health {
        DocumentationHealth::Good => 0.0,
        DocumentationHealth::Partial => 1.0,
        DocumentationHealth::Low => 3.0,
        DocumentationHealth::Empty => 5.0,
    };
    let size_factor: f64 = if status.active_objects < 20 {
        0.25
    } else if status.active_objects < 100 {
        0.75
    } else {
        1.0
    };
    let cov_pts = (base_cov_pts * size_factor).round() as usize;
    score += cov_pts;
    factors.push(format!(
        "Coverage {} x{size_factor:.2} (+{cov_pts})",
        status.documentation_health
    ));

    let risk_pts = status.critical_risk_count * 2;
    score += risk_pts;
    factors.push(format!(
        "Critical Risks: {} (+{risk_pts})",
        status.critical_risk_count
    ));

    let q_pts = status.open_questions.len();
    score += q_pts;
    factors.push(format!("Open Questions: {q_pts} (+{q_pts})"));

    let (size_pts, size_tier) = if status.active_objects < 20 {
        (0, "Small")
    } else if status.active_objects < 100 {
        (1, "Medium")
    } else if status.active_objects < 500 {
        (2, "Large")
    } else {
        (3, "Very Large")
    };
    score += size_pts;
    factors.push(format!(
        "Project Size: {size_tier} ({} files) (+{size_pts})",
        status.active_objects
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

#[cfg(test)]
mod tests {
    use vos_core::{DocumentationHealth, Project, ProjectHealth, ProjectStatus, RecoveryDifficulty};

    use super::*;

    fn synthetic_status(
        days_ago: Option<i64>,
        doc_health: DocumentationHealth,
        critical_risk_count: usize,
        open_questions: usize,
        active_objects: usize,
    ) -> (ProjectStatus, i64) {
        let now = 1_800_000_000_i64;
        let last_change_at = days_ago.map(|d| now - d * 86_400);
        let questions = (0..open_questions).map(|i| format!("Q{i}")).collect();
        (
            ProjectStatus {
                project: Project {
                    id: "proj_1".to_string(),
                    name: "sample".to_string(),
                    root_path: None,
                    description: None,
                    created_at: now,
                    updated_at: now,
                },
                health: ProjectHealth::Active,
                last_change_at,
                active_objects,
                deleted_objects: 0,
                key_documents: Vec::new(),
                open_questions: questions,
                pending_work: Vec::new(),
                active_areas: Vec::new(),
                coverage_ratio: 10.0,
                documentation_health: doc_health,
                critical_risk_count,
                high_risk_count: 0,
                critical_areas: Vec::new(),
                dependency_hubs: Vec::new(),
                risk_areas: Vec::new(),
            },
            now,
        )
    }

    #[test]
    fn predicts_easy_medium_and_hard_recovery_deterministically() {
        let (easy_st, now) = synthetic_status(Some(2), DocumentationHealth::Good, 0, 0, 12);
        let easy = predict_from_status_at(&easy_st, now);
        assert_eq!(easy.difficulty, RecoveryDifficulty::Easy);
        assert_eq!(easy.score, 0);

        let (med_st, _) = synthetic_status(Some(20), DocumentationHealth::Partial, 1, 1, 50);
        let med = predict_from_status_at(&med_st, now);
        assert_eq!(med.difficulty, RecoveryDifficulty::Medium);
        assert_eq!(med.score, 2 + 1 + 2 + 1 + 1);

        let (hard_st, _) = synthetic_status(Some(94), DocumentationHealth::Partial, 2, 3, 50);
        let hard = predict_from_status_at(&hard_st, now);
        assert_eq!(hard.difficulty, RecoveryDifficulty::Hard);
        assert_eq!(hard.score, 4 + 1 + 4 + 3 + 1);
        assert!(hard.factors.iter().any(|f| f.contains("Dormant 94 days")));

        // Micro-repo (<20 files) with Empty coverage scales down (`5 * 0.25 -> 1`).
        let (micro_st, _) = synthetic_status(Some(15), DocumentationHealth::Empty, 0, 0, 2);
        let micro = predict_from_status_at(&micro_st, now);
        assert_eq!(micro.difficulty, RecoveryDifficulty::Easy);
        assert_eq!(micro.score, 2 + 1);
    }
}
