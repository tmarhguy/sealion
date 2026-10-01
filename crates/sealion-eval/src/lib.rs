//! SeaLion relevance evaluation: judgments, metrics, regression (§51–53).
//!
//! Grades: `0 = irrelevant, 1 = marginal, 2 = relevant, 3 = highly
//! relevant`. Binary metrics (P@K, R@K, MRR, MAP) treat grade ≥ 1 as
//! relevant; nDCG uses full grades with exponential gains. Primary metric:
//! **nDCG@10**.
//!
//! File formats (TSV, `#` comments, blank lines skipped):
//!
//! ```text
//! queries.tsv:   <query-id>\t<query text>
//! judgments.tsv: <query-id>\t<grade 0-3>\t<doc basename>
//! ```
//!
//! Judgments reference document *basenames* (not full URLs) so the same
//! files evaluate on any machine: the harness resolves basenames to live
//! DocIds through the index under test.

use std::collections::{BTreeMap, HashMap};

use sealion_core::document::DocId;
use serde::{Deserialize, Serialize};

/// One evaluation query.
#[derive(Debug, Clone)]
pub struct EvalQuery {
    pub id: String,
    pub text: String,
}

/// Grade mapping: query-id → (doc basename → grade).
#[derive(Debug, Clone, Default)]
pub struct Qrels {
    grades: HashMap<String, HashMap<String, u8>>,
}

impl Qrels {
    pub fn grade(&self, query_id: &str, basename: &str) -> u8 {
        self.grades
            .get(query_id)
            .and_then(|m| m.get(basename))
            .copied()
            .unwrap_or(0)
    }

    pub fn query_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.grades.keys().map(String::as_str).collect();
        ids.sort();
        ids
    }
}

/// Parse `queries.tsv`.
pub fn load_queries(text: &str) -> Result<Vec<EvalQuery>, String> {
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (id, query) = line
            .split_once('\t')
            .ok_or_else(|| format!("queries line {}: expected `<id>\\t<text>`", lineno + 1))?;
        if id.trim().is_empty() || query.trim().is_empty() {
            return Err(format!("queries line {}: empty id or text", lineno + 1));
        }
        out.push(EvalQuery {
            id: id.trim().to_string(),
            text: query.trim().to_string(),
        });
    }
    Ok(out)
}

/// Parse `judgments.tsv`.
pub fn load_judgments(text: &str) -> Result<Qrels, String> {
    let mut qrels = Qrels::default();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (qid, grade, doc) = match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), Some(c)) => (a.trim(), b.trim(), c.trim()),
            _ => {
                return Err(format!(
                    "judgments line {}: expected `<query-id>\\t<grade>\\t<doc>`",
                    lineno + 1
                ))
            }
        };
        let grade: u8 = grade
            .parse()
            .map_err(|_| format!("judgments line {}: grade must be 0-3", lineno + 1))?;
        if grade > 3 || qid.is_empty() || doc.is_empty() {
            return Err(format!("judgments line {}: bad grade/id/doc", lineno + 1));
        }
        qrels
            .grades
            .entry(qid.to_string())
            .or_default()
            .insert(doc.to_string(), grade);
    }
    Ok(qrels)
}

/// Gain for nDCG: exponential (3→7, 2→3, 1→1, 0→0).
pub fn gain(grade: u8) -> f64 {
    match grade {
        0 => 0.0,
        1 => 1.0,
        2 => 3.0,
        _ => 7.0,
    }
}

/// Discounted cumulative gain @k over a grade sequence.
pub fn dcg(grades: &[u8], k: usize) -> f64 {
    grades
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, &g)| gain(g) / ((i + 2) as f64).log2())
        .sum()
}

/// nDCG@k: DCG over ranked grades divided by ideal DCG. The ideal comes
/// from *all* judged grades for the query (including unretrieved docs),
/// so missing relevant docs hurt. Empty ideal (nothing judged relevant)
/// scores 1.0 — nothing to find, nothing missed.
pub fn ndcg_at_k(ranked_grades: &[u8], all_grades: &[u8], k: usize) -> f64 {
    let mut ideal = all_grades.to_vec();
    ideal.sort_by(|a, b| b.cmp(a));
    let best = dcg(&ideal, k);
    if best <= 0.0 {
        return 1.0;
    }
    dcg(ranked_grades, k) / best
}

/// Precision@k (grade ≥ 1 relevant).
pub fn precision_at_k(ranked_grades: &[u8], k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let n = ranked_grades.len().min(k);
    if n == 0 {
        return 0.0;
    }
    ranked_grades.iter().take(k).filter(|&&g| g >= 1).count() as f64 / k as f64
}

/// Recall@k against the total relevant count for the query.
pub fn recall_at_k(ranked_grades: &[u8], total_relevant: usize, k: usize) -> f64 {
    if total_relevant == 0 {
        return 1.0;
    }
    ranked_grades.iter().take(k).filter(|&&g| g >= 1).count() as f64 / total_relevant as f64
}

/// Average precision (grade ≥ 1 relevant, over the full ranking).
pub fn average_precision(ranked_grades: &[u8], total_relevant: usize) -> f64 {
    if total_relevant == 0 {
        return 1.0;
    }
    let mut hits = 0usize;
    let mut sum = 0.0;
    for (i, &g) in ranked_grades.iter().enumerate() {
        if g >= 1 {
            hits += 1;
            sum += hits as f64 / (i + 1) as f64;
        }
    }
    sum / total_relevant as f64
}

/// Reciprocal rank of the first relevant (grade ≥ 1) result, 0 if none.
pub fn reciprocal_rank(ranked_grades: &[u8]) -> f64 {
    ranked_grades
        .iter()
        .position(|&g| g >= 1)
        .map(|i| 1.0 / (i + 1) as f64)
        .unwrap_or(0.0)
}

/// Per-query scores (serializable for `--save` reports).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryScores {
    pub query_id: String,
    pub query_text: String,
    pub ndcg_at_10: f64,
    pub mrr: f64,
    pub ap: f64,
    pub precision_at_10: f64,
    pub recall_at_10: f64,
    pub retrieved: usize,
    pub relevant: usize,
}

/// A full evaluation report (means + per-query rows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub queries: Vec<QueryScores>,
    pub mean_ndcg_at_10: f64,
    pub mean_mrr: f64,
    pub mean_ap: f64,
    pub mean_precision_at_10: f64,
    pub mean_recall_at_10: f64,
}

/// Score one run: `ranked` maps query-id → ranked DocIds (best first),
/// `rel` maps query-id → (DocId → grade).
pub fn evaluate(
    queries: &[EvalQuery],
    ranked: &BTreeMap<String, Vec<DocId>>,
    rel: &BTreeMap<String, HashMap<DocId, u8>>,
) -> Report {
    let mut rows = Vec::new();
    for q in queries {
        let empty = Vec::new();
        let list = ranked.get(&q.id).unwrap_or(&empty);
        let qrel = rel.get(&q.id);
        let grades: Vec<u8> = list
            .iter()
            .map(|id| qrel.and_then(|m| m.get(id)).copied().unwrap_or(0))
            .collect();
        let all_grades: Vec<u8> = qrel
            .map(|m| m.values().copied().collect())
            .unwrap_or_default();
        let total_relevant = all_grades.iter().filter(|&&g| g >= 1).count();
        rows.push(QueryScores {
            query_id: q.id.clone(),
            query_text: q.text.clone(),
            ndcg_at_10: ndcg_at_k(&grades, &all_grades, 10),
            mrr: reciprocal_rank(&grades),
            ap: average_precision(&grades, total_relevant),
            precision_at_10: precision_at_k(&grades, 10),
            recall_at_10: recall_at_k(&grades, total_relevant, 10),
            retrieved: list.len(),
            relevant: total_relevant,
        });
    }
    fn mean(rows: &[QueryScores], f: fn(&QueryScores) -> f64) -> f64 {
        if rows.is_empty() {
            0.0
        } else {
            rows.iter().map(f).sum::<f64>() / rows.len() as f64
        }
    }
    Report {
        mean_ndcg_at_10: mean(&rows, |r| r.ndcg_at_10),
        mean_mrr: mean(&rows, |r| r.mrr),
        mean_ap: mean(&rows, |r| r.ap),
        mean_precision_at_10: mean(&rows, |r| r.precision_at_10),
        mean_recall_at_10: mean(&rows, |r| r.recall_at_10),
        queries: rows,
    }
}

/// Regression comparison between a baseline and a candidate report (§53).
#[derive(Debug, Clone)]
pub struct Regression {
    pub queries_improved: usize,
    pub queries_regressed: usize,
    pub queries_unchanged: usize,
    pub delta_ndcg_at_10: f64,
    pub delta_mrr: f64,
    pub delta_ap: f64,
    /// (query-id, old nDCG, new nDCG) worst regressions first.
    pub largest_regressions: Vec<(String, f64, f64)>,
}

pub fn compare_reports(old: &Report, new: &Report) -> Regression {
    let old_by_id: HashMap<&str, f64> = old
        .queries
        .iter()
        .map(|q| (q.query_id.as_str(), q.ndcg_at_10))
        .collect();
    let mut improved = 0;
    let mut regressed = 0;
    let mut unchanged = 0;
    let mut regs = Vec::new();
    for q in &new.queries {
        let o = old_by_id.get(q.query_id.as_str()).copied().unwrap_or(0.0);
        let d = q.ndcg_at_10 - o;
        if d > 1e-9 {
            improved += 1;
        } else if d < -1e-9 {
            regressed += 1;
            regs.push((q.query_id.clone(), o, q.ndcg_at_10));
        } else {
            unchanged += 1;
        }
    }
    regs.sort_by(|a, b| (a.2 - a.1).partial_cmp(&(b.2 - b.1)).unwrap());
    Regression {
        queries_improved: improved,
        queries_regressed: regressed,
        queries_unchanged: unchanged,
        delta_ndcg_at_10: new.mean_ndcg_at_10 - old.mean_ndcg_at_10,
        delta_mrr: new.mean_mrr - old.mean_mrr,
        delta_ap: new.mean_ap - old.mean_ap,
        largest_regressions: regs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndcg_rewards_top_placement() {
        // Perfect ranking scores 1.0; reversed relevant pair scores less.
        assert!((ndcg_at_k(&[3, 2, 1, 0], &[3, 2, 1, 0], 10) - 1.0).abs() < 1e-12);
        let rev = ndcg_at_k(&[0, 1, 2, 3], &[3, 2, 1, 0], 10);
        assert!(rev < 1.0 && rev > 0.0, "{rev}");
        // Empty ideal (no relevant docs anywhere) is vacuously perfect.
        assert_eq!(ndcg_at_k(&[0, 0, 0], &[], 10), 1.0);
        // Missed relevant docs hurt: retrieved nothing of [3].
        assert_eq!(ndcg_at_k(&[0], &[3], 10), 0.0);
    }

    #[test]
    fn binary_metrics_behave() {
        assert_eq!(precision_at_k(&[1, 0, 1], 2), 0.5);
        assert_eq!(recall_at_k(&[1, 0, 1], 2, 3), 1.0);
        assert_eq!(reciprocal_rank(&[0, 0, 1]), 1.0 / 3.0);
        assert_eq!(reciprocal_rank(&[0, 0]), 0.0);
        // AP of perfect 2/2 ranking is 1.0.
        assert!((average_precision(&[1, 1], 2) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn report_and_regression() {
        let queries = vec![
            EvalQuery {
                id: "q1".into(),
                text: "a".into(),
            },
            EvalQuery {
                id: "q2".into(),
                text: "b".into(),
            },
        ];
        let ranked: BTreeMap<String, Vec<DocId>> = [
            ("q1".to_string(), vec![DocId(1), DocId(2)]),
            ("q2".to_string(), vec![DocId(3)]),
        ]
        .into_iter()
        .collect();
        let rel: BTreeMap<String, HashMap<DocId, u8>> = [
            ("q1".to_string(), [(DocId(1), 3)].into_iter().collect()),
            ("q2".to_string(), [(DocId(9), 2)].into_iter().collect()),
        ]
        .into_iter()
        .collect();
        let rep = evaluate(&queries, &ranked, &rel);
        assert_eq!(rep.queries.len(), 2);
        // q1: top hit highly relevant → nDCG 1.0. q2: missed → 0.0-ish.
        assert!((rep.queries[0].ndcg_at_10 - 1.0).abs() < 1e-12);
        assert_eq!(rep.queries[1].ndcg_at_10, 0.0);
        let reg = compare_reports(&rep, &rep);
        assert_eq!(reg.queries_regressed, 0);
        assert_eq!(reg.queries_unchanged, 2);
    }

    #[test]
    fn loaders_reject_garbage() {
        assert!(load_queries("no-tab-here").is_err());
        assert!(load_judgments("q1\tnot-a-grade\tdoc").is_err());
        assert!(load_judgments("q1\t5\tdoc").is_err());
        let q = load_queries("# comment\n\nq1\tcompiler optimization\n").unwrap();
        assert_eq!(q.len(), 1);
    }
}
