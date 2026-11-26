//! Multi-project Constellation portfolio aggregation and ASCII rendering (`vos ui`, `v2.0`).

use vos_core::util::now_unix;
use vos_core::{ConstellationNode, ConstellationPortfolio, FactStatus};

use crate::{Store, StoreError};

impl Store {
    /// Aggregates all registered projects into a live multi-project Constellation portfolio snapshot.
    pub fn constellation_portfolio(&self) -> Result<ConstellationPortfolio, StoreError> {
        let summaries = self.list_projects()?;
        let calibration = self.calibrate_recovery(None)?;
        let mut nodes = Vec::with_capacity(summaries.len());
        let mut total_objects = 0usize;
        let mut total_dirty_files = 0usize;
        let mut total_stale_facts = 0usize;

        for sum in summaries {
            let status = self.project_status(&sum.project.name)?;
            let pred = self.predict_recovery_calibrated(&status);
            let facts = self.list_facts(Some(&sum.project.name), false, 100)?;
            let stale_facts_count = facts
                .iter()
                .filter(|f| f.status == FactStatus::Stale)
                .count();
            let leases = self.list_leases(Some(&sum.project.name))?;
            let (dirty_files, branch) = match &status.git {
                Some(g) => (g.changed_files.len(), Some(g.branch.clone())),
                None => (0, None),
            };
            let top_areas = status
                .active_areas
                .iter()
                .take(3)
                .map(|a| a.area.clone())
                .collect();

            total_objects += status.active_objects;
            total_dirty_files += dirty_files;
            total_stale_facts += stale_facts_count;

            nodes.push(ConstellationNode {
                project_name: status.project.name,
                root_path: status.project.root_path,
                health: status.health,
                active_objects: status.active_objects,
                dirty_files,
                branch,
                predicted_difficulty: pred.difficulty,
                risk_score: pred.score,
                critical_risk_count: status.critical_risk_count,
                stale_facts_count,
                active_leases_count: leases.len(),
                open_questions_count: status.open_questions.len(),
                last_activity_at: status.last_change_at,
                top_areas,
            });
        }

        nodes.sort_by(|a, b| {
            (b.dirty_files > 0)
                .cmp(&(a.dirty_files > 0))
                .then_with(|| b.risk_score.cmp(&a.risk_score))
                .then_with(|| b.active_objects.cmp(&a.active_objects))
                .then_with(|| a.project_name.cmp(&b.project_name))
        });

        Ok(ConstellationPortfolio {
            generated_at: now_unix(),
            total_objects,
            total_dirty_files,
            total_stale_facts,
            projects: nodes,
            calibration,
        })
    }
}

/// Renders a crisp terminal ASCII/Unicode Constellation Matrix for `vos ui --once`.
pub fn render_constellation_ascii(p: &ConstellationPortfolio) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "VOS CONSTELLATION HUD | {} projects | {} objects | {} dirty files | {} stale facts\n",
        p.projects.len(),
        p.total_objects,
        p.total_dirty_files,
        p.total_stale_facts
    ));
    out.push_str(&format!(
        "Calibration: {} scored session(s), false_alarm={:.0}%, missed={:.0}%\n",
        p.calibration.total_scored_sessions,
        p.calibration.false_alarm_rate * 100.0,
        p.calibration.missed_difficulty_rate * 100.0
    ));
    out.push_str(&"-".repeat(92));
    out.push('\n');
    for node in &p.projects {
        let pulse = if node.dirty_files > 0 { "*" } else { " " };
        let branch = node.branch.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "{pulse} {:<14} | {:<6} | {:>4} objs | dirty:{:>2} ({:<18}) | stale:{} leases:{} q:{} | areas: {}\n",
            node.project_name,
            format!("{:?}", node.predicted_difficulty),
            node.active_objects,
            node.dirty_files,
            branch,
            node.stale_facts_count,
            node.active_leases_count,
            node.open_questions_count,
            if node.top_areas.is_empty() { "-".to_string() } else { node.top_areas.join(", ") }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constellation_aggregates_projects_and_renders_ascii() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("alpha", None, None).unwrap();
        store.create_project("beta", None, None).unwrap();
        let portfolio = store.constellation_portfolio().unwrap();
        assert_eq!(portfolio.projects.len(), 2);
        let ascii = render_constellation_ascii(&portfolio);
        assert!(ascii.contains("VOS CONSTELLATION HUD | 2 projects"));
        assert!(ascii.contains("alpha"));
    }
}
