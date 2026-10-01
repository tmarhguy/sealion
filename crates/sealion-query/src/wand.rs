//! WAND and Block-Max WAND top-k retrieval (spec §38–39, milestone 09).
//!
//! Both algorithms return **exactly** what exhaustive scoring returns
//! (§41): same DocIds, same scores, same order. They only skip full
//! scoring of documents that cannot enter the top-k.
//!
//! - **WAND**: per-term score upper bounds steer a pivot loop over posting
//!   cursors. Bounds here are exact per-term maxima (scored once per term
//!   over its postings), so skipping is safe by construction.
//! - **Block-Max WAND**: postings are windowed into blocks (64 entries);
//!   each block carries its own max contribution. Whole blocks whose bound
//!   cannot beat the heap threshold are skipped without decoding their
//!   documents. Bounds are computed per query in memory (baseline);
//!   persisting them in the segment format is milestone 16 work.
//!
//! `(scored, skipped, blocks_skipped)` counters back the §90 ablation
//! (exhaustive vs WAND vs Block-Max WAND) and `--explain` diagnostics.

use sealion_core::config::{AnalysisConfig, RankingConfig};
use sealion_core::document::DocId;
use sealion_core::error::Result;
use sealion_core::field::Field;
use sealion_index::view::IndexView;

use crate::execute::search_view;
use crate::query::Query;
use crate::rank::{
    bm25_term_contrib, expand_terms, field_weight, Explain, RankedHit, ScoreCtx, TermScore,
};

/// Block size (postings) for Block-Max bounds.
pub const BLOCK_SIZE: usize = 64;

/// Slack for bound-vs-threshold comparisons. Skips happen only when a
/// doc's max-possible score is below `threshold - EPS`, so exact ties
/// (and float rounding) always fall through to full evaluation and the
/// deterministic (score desc, DocId asc) order stays exact.
const EPS: f32 = 1e-6;

/// Work counters for ablation reporting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WandStats {
    /// Documents fully scored (BM25 over all terms).
    pub scored: usize,
    /// Cursor advances over documents that were never fully scored.
    pub skipped: usize,
    /// Blocks skipped wholesale (Block-Max WAND only).
    pub blocks_skipped: usize,
}

/// One scoring term's postings plus precomputed bounds.
struct TermList {
    field: Field,
    term: String,
    /// (doc, tf) in DocId order.
    postings: Vec<(DocId, usize)>,
    /// Exact max contribution of this term over any document.
    upper: f32,
    idf: f32,
    df: usize,
    cursor: usize,
    /// Per-block max contributions, aligned to `postings` windows.
    block_max: Vec<f32>,
}

impl TermList {
    fn doc_at(&self) -> Option<DocId> {
        self.postings.get(self.cursor).map(|(d, _)| *d)
    }

    fn advance(&mut self) {
        self.cursor += 1;
    }

    /// Advance to the first posting with doc >= target. Returns steps moved.
    fn advance_to(&mut self, target: DocId) -> usize {
        let from = self.cursor;
        while self.cursor < self.postings.len() && self.postings[self.cursor].0 < target {
            self.cursor += 1;
        }
        self.cursor - from
    }

    fn block_of(&self, cursor: usize) -> usize {
        cursor / BLOCK_SIZE
    }
}

/// Shared preparation: candidates, expanded terms, stats, term lists with
/// exact upper bounds and block maxima.
struct Prepared {
    candidates: Vec<DocId>,
    lists: Vec<TermList>,
    avg: [f64; 4],
    n: usize,
    max_timestamp: u64,
    // Note: the max non-lexical addition (authority + freshness) is folded
    // directly into every term/block bound in `prepare`, so skips stay
    // exact (§41) when the milestone 18 blends are enabled. It is not kept
    // as a field because bounds already carry it.
}

fn prepare<V: IndexView>(
    view: &V,
    analysis: &AnalysisConfig,
    ranking: &RankingConfig,
    query: &Query,
) -> Result<Prepared> {
    let candidates = search_view(view, query)?;
    let scoring_terms = expand_terms(view, query);
    let n = view.len();
    let mut avg_cache = [0f64; 4];
    if n > 0 {
        for f in Field::ALL {
            avg_cache[f.index()] = view.field_token_total(f, analysis) as f64 / n as f64;
        }
    }
    // Corpus-newest timestamp + max non-lexical addition (one doc scan).
    let mut max_timestamp = 0u64;
    for id in view.all_doc_ids() {
        if let Some(d) = view.get(id) {
            max_timestamp = max_timestamp.max(d.timestamp);
        }
    }
    let mut max_extra = 0f32;
    if ranking.authority_weight != 0.0 || ranking.freshness_weight != 0.0 {
        for id in view.all_doc_ids() {
            if let Some(d) = view.get(id) {
                let mut e = 0f32;
                if ranking.authority_weight != 0.0 {
                    e += (ranking.authority_weight as f32) * crate::rank::doc_authority(d);
                }
                if ranking.freshness_weight != 0.0 {
                    e += (ranking.freshness_weight as f32)
                        * crate::rank::doc_recency(
                            d.timestamp,
                            max_timestamp,
                            ranking.freshness_halflife_days,
                        );
                }
                max_extra = max_extra.max(e);
            }
        }
    }
    let mut lists = Vec::new();
    for (f, term) in scoring_terms {
        let plist = view.postings(f, &term)?;
        let df = plist.len();
        let weight = field_weight(ranking, f);
        let mut postings = Vec::with_capacity(plist.len());
        let mut upper = 0f32;
        let mut idf_v = 0f32;
        let mut block_max: Vec<f32> = Vec::new();
        let mut block_cur = 0f32;
        for (i, p) in plist.iter().enumerate() {
            let tf = p.freq();
            let doc_len = view.field_length(p.doc, f, analysis);
            let (contrib, idf_now) =
                bm25_term_contrib(tf, df, n, doc_len, avg_cache[f.index()], ranking, weight);
            idf_v = idf_now;
            upper = upper.max(contrib);
            block_cur = block_cur.max(contrib);
            postings.push((p.doc, tf));
            if (i + 1) % BLOCK_SIZE == 0 || i + 1 == plist.len() {
                block_max.push(block_cur);
                block_cur = 0.0;
            }
        }
        // Fold the non-lexical ceiling into every bound: any doc's true
        // score is at most its lexical contribution here plus max_extra.
        let upper = upper + max_extra;
        let block_max: Vec<f32> = block_max.into_iter().map(|b| b + max_extra).collect();
        lists.push(TermList {
            field: f,
            term,
            postings,
            upper,
            idf: idf_v,
            df,
            cursor: 0,
            block_max,
        });
    }
    Ok(Prepared {
        candidates,
        lists,
        avg: avg_cache,
        n,
        max_timestamp,
    })
}

/// Maintain heap order and report the threshold (lowest score in a full
/// heap, else NEG_INFINITY while still filling).
fn heap_insert_dry(heap: &mut Vec<RankedHit>, top_k: usize) -> f32 {
    heap.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc.cmp(&b.doc))
    });
    heap.truncate(top_k);
    if heap.len() == top_k {
        heap.last().map(|h| h.score).unwrap_or(f32::NEG_INFINITY)
    } else {
        f32::NEG_INFINITY
    }
}

fn full_score<V: IndexView>(
    ctx: &crate::rank::ScoreCtx<'_, V>,
    lists: &[TermList],
    avg_cache: &[f64; 4],
    n: usize,
    doc: DocId,
    lengths: &mut crate::rank::LengthMemo,
    max_timestamp: u64,
) -> (
    f32,
    Vec<TermScore>,
    [(Field, f32); 4],
    crate::rank::ExtraScores,
) {
    let (view, analysis, ranking) = (ctx.view, ctx.analysis, ctx.ranking);
    let mut total = 0f32;
    let mut terms = Vec::new();
    let mut field_totals = [
        (Field::Title, 0f32),
        (Field::Heading, 0f32),
        (Field::Body, 0f32),
        (Field::Anchor, 0f32),
    ];
    for tl in lists {
        // tf via cursor-adjacent binary search on the term's postings.
        let tf = match tl.postings.binary_search_by(|(d, _)| d.cmp(&doc)) {
            Ok(i) => tl.postings[i].1,
            Err(_) => continue,
        };
        let doc_len = crate::rank::memo_field_length(view, doc, tl.field, analysis, lengths);
        let (contrib, _) = bm25_term_contrib(
            tf,
            tl.df,
            n,
            doc_len,
            avg_cache[tl.field.index()],
            ranking,
            field_weight(ranking, tl.field),
        );
        if tl.idf == 0.0 {
            continue;
        }
        total += contrib;
        field_totals[tl.field.index()].1 += contrib;
        terms.push(TermScore {
            field: tl.field,
            term: tl.term.clone(),
            tf,
            df: tl.df,
            idf: tl.idf,
            doc_len,
            avg_len: avg_cache[tl.field.index()] as f32,
            contrib,
        });
    }
    // Authority + freshness, identical math to exhaustive `score_doc`.
    let mut extra = crate::rank::ExtraScores::default();
    if ranking.authority_weight != 0.0 || ranking.freshness_weight != 0.0 {
        if let Some(stored) = view.get(doc) {
            if ranking.authority_weight != 0.0 {
                extra.authority =
                    (ranking.authority_weight as f32) * crate::rank::doc_authority(stored);
                total += extra.authority;
            }
            if ranking.freshness_weight != 0.0 {
                extra.freshness = (ranking.freshness_weight as f32)
                    * crate::rank::doc_recency(
                        stored.timestamp,
                        max_timestamp,
                        ranking.freshness_halflife_days,
                    );
                total += extra.freshness;
            }
        }
    }
    (total, terms, field_totals, extra)
}

/// WAND top-k: exact results, skipped full evaluations counted.
pub fn wand_search<V: IndexView>(
    view: &V,
    analysis: &AnalysisConfig,
    ranking: &RankingConfig,
    query: &Query,
    top_k: usize,
) -> Result<(Vec<RankedHit>, WandStats)> {
    let prepared = prepare(view, analysis, ranking, query)?;
    let Prepared {
        candidates,
        mut lists,
        avg: avg_cache,
        n,
        max_timestamp,
        ..
    } = prepared;
    let mut stats = WandStats::default();
    if top_k == 0 {
        return Ok((Vec::new(), stats));
    }
    // No scoring terms (e.g. site-only filter): fall back to exhaustive
    // ordering (DocId asc, score 0) to match `ranked_search` exactly.
    if lists.iter().all(|tl| tl.postings.is_empty()) {
        let hits = crate::rank::ranked_search(view, analysis, ranking, query, top_k)?;
        stats.scored = hits.len();
        return Ok((hits, stats));
    }
    lists.retain(|tl| !tl.postings.is_empty());
    let mut heap: Vec<RankedHit> = Vec::new();
    let mut threshold = f32::NEG_INFINITY;
    // Boolean membership gate: only true candidates may enter the heap,
    // otherwise a non-member could poison the threshold and break §41.
    // `candidates` from `prepare` is DocId-sorted, so binary search works.
    let is_member = |doc: DocId| -> bool { candidates.binary_search(&doc).is_ok() };
    // Field lengths are fixed for the query: memoize across candidates.
    let mut lengths: crate::rank::LengthMemo = Default::default();
    let ctx = ScoreCtx {
        view,
        analysis,
        ranking,
    };
    loop {
        // Sort term indices by current cursor doc.
        let mut order: Vec<usize> = (0..lists.len()).collect();
        // Any exhausted list drops out of pivot consideration.
        order.retain(|&i| lists[i].doc_at().is_some());
        if order.is_empty() {
            break;
        }
        order.sort_by_key(|&i| lists[i].doc_at().expect("retained"));
        // Prefix sums of upper bounds in cursor order; pivot = first term
        // whose cumulative bound exceeds threshold (minus EPS so ties are
        // always evaluated, never skipped).
        let mut acc = 0f32;
        let mut pivot = None;
        for &i in &order {
            acc += lists[i].upper;
            if acc > threshold - EPS {
                pivot = Some(i);
                break;
            }
        }
        let Some(piv) = pivot else {
            break; // No remaining doc can enter the top-k.
        };
        let pivot_doc = lists[piv].doc_at().expect("pivot live");
        // Textbook WAND: candidate iff every list at-or-before the pivot
        // in cursor order already sits on pivot_doc.
        let pos = order.iter().position(|&i| i == piv).expect("ordered");
        if order[..pos]
            .iter()
            .all(|&i| lists[i].doc_at() == Some(pivot_doc))
        {
            // Skip scoring entirely for Boolean non-members (milestone 16:
            // AND/NOT queries traverse OR-space; scoring non-members only
            // to discard them wasted most of WAND's budget).
            if !is_member(pivot_doc) {
                stats.skipped += 1;
                for tl in lists.iter_mut() {
                    if tl.doc_at() == Some(pivot_doc) {
                        tl.advance();
                    }
                }
                continue;
            }
            let (total, terms, field_totals, extra) = full_score(
                &ctx,
                &lists,
                &avg_cache,
                n,
                pivot_doc,
                &mut lengths,
                max_timestamp,
            );
            stats.scored += 1;
            // Every evaluated member is pushed; truncation keeps the exact
            // top-k (superset evaluation + deterministic order = exact).
            if is_member(pivot_doc) {
                heap.push(RankedHit {
                    doc: pivot_doc,
                    score: total,
                    explain: Explain {
                        doc: pivot_doc,
                        total,
                        terms,
                        field_totals,
                        authority: extra.authority,
                        freshness: extra.freshness,
                    },
                });
                threshold = heap_insert_dry(&mut heap, top_k);
            }
            for tl in lists.iter_mut() {
                if tl.doc_at() == Some(pivot_doc) {
                    tl.advance();
                }
            }
        } else {
            // Advance lagging cursors to the pivot doc.
            for &i in &order {
                if i == piv {
                    break;
                }
                stats.skipped += lists[i].advance_to(pivot_doc);
            }
        }
    }
    // Heap already holds only Boolean members in order; truncate only.
    // (Membership was gated before each insert, so the threshold stayed
    // valid throughout — this is what keeps WAND exact for AND/NOT.)
    heap.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc.cmp(&b.doc))
    });
    heap.truncate(top_k);
    Ok((heap, stats))
}

/// Block-Max WAND top-k: like [`wand_search`] but skips whole blocks whose
/// max contribution cannot beat the threshold.
pub fn block_max_wand_search<V: IndexView>(
    view: &V,
    analysis: &AnalysisConfig,
    ranking: &RankingConfig,
    query: &Query,
    top_k: usize,
) -> Result<(Vec<RankedHit>, WandStats)> {
    let prepared = prepare(view, analysis, ranking, query)?;
    let Prepared {
        candidates,
        mut lists,
        avg: avg_cache,
        n,
        max_timestamp,
        ..
    } = prepared;
    let mut stats = WandStats::default();
    if top_k == 0 {
        return Ok((Vec::new(), stats));
    }
    if lists.iter().all(|tl| tl.postings.is_empty()) {
        let hits = crate::rank::ranked_search(view, analysis, ranking, query, top_k)?;
        stats.scored = hits.len();
        return Ok((hits, stats));
    }
    lists.retain(|tl| !tl.postings.is_empty());
    let mut heap: Vec<RankedHit> = Vec::new();
    let mut threshold = f32::NEG_INFINITY;
    let is_member = |doc: DocId| -> bool { candidates.binary_search(&doc).is_ok() };
    let mut lengths: crate::rank::LengthMemo = Default::default();
    let ctx = ScoreCtx {
        view,
        analysis,
        ranking,
    };
    // Sum of global term uppers (recomputed as lists exhaust).
    let upper_sum = |lists: &[TermList]| -> f32 { lists.iter().map(|tl| tl.upper).sum() };

    loop {
        let mut order: Vec<usize> = (0..lists.len())
            .filter(|&i| lists[i].doc_at().is_some())
            .collect();
        if order.is_empty() {
            break;
        }
        order.sort_by_key(|&i| lists[i].doc_at().expect("live"));
        // Whole-block skip: any doc in list i's cursor block scores at
        // most block_max_i + Σ_{j≠i} upper_j. If that is below threshold
        // (minus EPS), no doc in the block can enter the top-k, so jump
        // the whole block without decoding its documents.
        if heap.len() == top_k {
            let total_upper = upper_sum(&lists);
            let mut jumped = false;
            for tl in lists.iter_mut() {
                if tl.doc_at().is_none() {
                    continue;
                }
                let block = tl.block_of(tl.cursor);
                let bmax = tl.block_max.get(block).copied().unwrap_or(tl.upper);
                // bound = bmax + Σ_{j≠i} upper_j = total_upper - upper_i + bmax.
                if total_upper - tl.upper + bmax < threshold - EPS {
                    let end = ((block + 1) * BLOCK_SIZE).min(tl.postings.len());
                    stats.blocks_skipped += 1;
                    stats.skipped += end - tl.cursor;
                    tl.cursor = end;
                    jumped = true;
                }
            }
            if jumped {
                continue;
            }
        }
        // One standard WAND pivot step over global upper bounds.
        let mut acc = 0f32;
        let mut pivot = None;
        for &i in &order {
            // Skip lists exhausted by block jumps above.
            if lists[i].doc_at().is_none() {
                continue;
            }
            acc += lists[i].upper;
            if acc > threshold - EPS {
                pivot = Some(i);
                break;
            }
        }
        let Some(piv) = pivot else { break };
        let pivot_doc = lists[piv].doc_at().expect("pivot live");
        let pos = order.iter().position(|&i| i == piv).expect("ordered");
        // Order may contain indices exhausted by block jumps; filter them
        // for the alignment check (exhausted lists hold no docs back).
        let live_before: Vec<usize> = order[..pos]
            .iter()
            .copied()
            .filter(|&i| lists[i].doc_at().is_some())
            .collect();
        if live_before
            .iter()
            .all(|&i| lists[i].doc_at() == Some(pivot_doc))
        {
            if !is_member(pivot_doc) {
                stats.skipped += 1;
                for tl in lists.iter_mut() {
                    if tl.doc_at() == Some(pivot_doc) {
                        tl.advance();
                    }
                }
                continue;
            }
            let (total, terms, field_totals, extra) = full_score(
                &ctx,
                &lists,
                &avg_cache,
                n,
                pivot_doc,
                &mut lengths,
                max_timestamp,
            );
            stats.scored += 1;
            if is_member(pivot_doc) {
                heap.push(RankedHit {
                    doc: pivot_doc,
                    score: total,
                    explain: Explain {
                        doc: pivot_doc,
                        total,
                        terms,
                        field_totals,
                        authority: extra.authority,
                        freshness: extra.freshness,
                    },
                });
                threshold = heap_insert_dry(&mut heap, top_k);
            }
            for tl in lists.iter_mut() {
                if tl.doc_at() == Some(pivot_doc) {
                    tl.advance();
                }
            }
        } else {
            for &i in &live_before {
                stats.skipped += lists[i].advance_to(pivot_doc);
            }
        }
    }
    heap.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc.cmp(&b.doc))
    });
    heap.truncate(top_k);
    Ok((heap, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::StemmerKind;
    use sealion_core::document::{Document, Source};
    use sealion_index::mem_index::MemIndex;

    fn cfg() -> (AnalysisConfig, RankingConfig) {
        (
            AnalysisConfig {
                stop_words: false,
                stemmer: StemmerKind::None,
                ..AnalysisConfig::default()
            },
            RankingConfig::default(),
        )
    }

    fn doc(id: u64, title: &str, body: &str) -> Document {
        Document {
            id: DocId(id),
            source: Source::Synthetic { name: "w".into() },
            url: format!("t://{id}"),
            title: title.into(),
            headings: Vec::new(),
            body: body.into(),
            anchor_text: Vec::new(),
            metadata: Default::default(),
            timestamp: 0,
            language: "en".into(),
            content_hash: String::new(),
        }
    }

    #[test]
    fn wand_matches_exhaustive_with_blends_enabled() {
        // Milestone 18: authority + freshness + TF-IDF must not break §41.
        let (a, _) = cfg();
        let vocab = ["alpha", "beta", "gamma", "delta", "compiler", "database"];
        let mut rng: u64 = 0xbeef_1234;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as usize
        };
        for scoring in [
            RankingConfig {
                scoring: sealion_core::config::ScoringKind::TfIdf,
                ..RankingConfig::default()
            },
            RankingConfig {
                authority_weight: 3.0,
                freshness_weight: 2.0,
                ..RankingConfig::default()
            },
        ] {
            let mut idx = MemIndex::new(a.clone());
            for id in 0..40u64 {
                let body = (0..8)
                    .map(|_| vocab[next() % vocab.len()])
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut d = doc(id, "title here", &body);
                d.metadata.insert(
                    "authority".to_string(),
                    format!("{:.3}", (next() % 100) as f64 / 100.0),
                );
                d.timestamp = 1_000_000 + (next() % 60) as u64 * 86_400;
                idx.add_document(d);
            }
            for _ in 0..20 {
                let t1 = vocab[next() % vocab.len()].to_string();
                let t2 = vocab[next() % vocab.len()].to_string();
                let q = Query::or(vec![Query::term(None, t1), Query::term(None, t2)]);
                let want = crate::rank::ranked_search(&idx, &a, &scoring, &q, 5).unwrap();
                let (got, _) = wand_search(&idx, &a, &scoring, &q, 5).unwrap();
                assert_same_order(&want, &got, "wand blends");
                let (got_bm, _) = block_max_wand_search(&idx, &a, &scoring, &q, 5).unwrap();
                assert_same_order(&want, &got_bm, "block-max blends");
            }
        }
    }

    fn corpus() -> MemIndex {
        let (a, _) = cfg();
        let mut idx = MemIndex::new(a);
        idx.add_document(doc(
            1,
            "compiler design",
            "compiler optimization passes passes",
        ));
        idx.add_document(doc(2, "interpreter guide", "bytecode interpreter loops"));
        idx.add_document(doc(
            3,
            "compiler backend",
            "javascript code generation compiler",
        ));
        idx.add_document(doc(4, "database systems", "distributed database systems"));
        idx.add_document(doc(5, "compiler compiler", "compiler"));
        idx
    }

    fn queries() -> Vec<Query> {
        vec![
            Query::term(None, "compiler"),
            Query::term(Some(Field::Body), "compiler"),
            Query::and(vec![
                Query::term(None, "compiler"),
                Query::term(None, "optimization"),
            ]),
            Query::or(vec![
                Query::term(None, "compiler"),
                Query::term(None, "interpreter"),
            ]),
            Query::and(vec![
                Query::term(None, "compiler"),
                Query::negate(Query::term(None, "javascript")),
            ]),
            Query::Phrase {
                field: Field::Body,
                terms: vec!["database".into(), "systems".into()],
            },
        ]
    }

    fn assert_same_order(a: &[RankedHit], b: &[RankedHit], what: &str) {
        assert_eq!(a.len(), b.len(), "{what}: len {} vs {}", a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.doc, y.doc, "{what}: doc order");
            assert!(
                (x.score - y.score).abs() < 1e-5,
                "{what}: score {}",
                x.doc.0
            );
        }
    }

    #[test]
    fn wand_matches_exhaustive_on_hand_cases() {
        let (a, r) = cfg();
        let idx = corpus();
        for q in queries() {
            let want = crate::rank::ranked_search(&idx, &a, &r, &q, 10).unwrap();
            let (got, stats) = wand_search(&idx, &a, &r, &q, 10).unwrap();
            assert_same_order(&want, &got, "wand");
            let (got_bm, _) = block_max_wand_search(&idx, &a, &r, &q, 10).unwrap();
            assert_same_order(&want, &got_bm, "block-max");
            let _ = stats;
        }
    }

    #[test]
    fn wand_matches_exhaustive_on_generated_corpus() {
        let (a, r) = cfg();
        let vocab = [
            "alpha", "beta", "gamma", "delta", "compiler", "database", "systems",
        ];
        let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) as usize
        };
        let mut idx = MemIndex::new(a.clone());
        for id in 0..60u64 {
            let body = (0..12)
                .map(|_| vocab[next() % vocab.len()])
                .collect::<Vec<_>>()
                .join(" ");
            let title = (0..3)
                .map(|_| vocab[next() % vocab.len()])
                .collect::<Vec<_>>()
                .join(" ");
            idx.add_document(doc(id, &title, &body));
        }
        for _ in 0..50 {
            let t1 = vocab[next() % vocab.len()].to_string();
            let t2 = vocab[next() % vocab.len()].to_string();
            let q = match next() % 4 {
                0 => Query::term(None, t1),
                1 => Query::and(vec![Query::term(None, t1), Query::term(None, t2)]),
                2 => Query::or(vec![Query::term(None, t1), Query::term(None, t2)]),
                _ => Query::and(vec![
                    Query::term(None, t1),
                    Query::negate(Query::term(None, t2)),
                ]),
            };
            for top_k in [1, 3, 10] {
                let want = crate::rank::ranked_search(&idx, &a, &r, &q, top_k).unwrap();
                let (got, _) = wand_search(&idx, &a, &r, &q, top_k).unwrap();
                assert_same_order(&want, &got, "wand fuzz {q:?} k={top_k}");
                let (got_bm, _) = block_max_wand_search(&idx, &a, &r, &q, top_k).unwrap();
                assert_same_order(&want, &got_bm, "block-max fuzz {q:?} k={top_k}");
            }
        }
    }

    #[test]
    fn wand_skips_work_on_selective_queries() {
        let (a, r) = cfg();
        let mut idx = MemIndex::new(a.clone());
        // Rare high-idf docs FIRST (small DocIds): the heap threshold rises
        // immediately, so WAND can prune/terminate over the filler tail.
        // (With good docs last, threshold stays low and every doc must be
        // examined — correct but uninformative. Fuzz tests cover that shape.)
        idx.add_document(doc(0, "rare gem", "common zephyrquux words"));
        idx.add_document(doc(1, "rare gem", "common zephyrquux words"));
        for id in 2..202u64 {
            idx.add_document(doc(id, "common filler", "common filler words everywhere"));
        }
        let q = Query::or(vec![
            Query::term(None, "common"),
            Query::term(None, "zephyrquux"),
        ]);
        let want = crate::rank::ranked_search(&idx, &a, &r, &q, 2).unwrap();
        assert_eq!(want.len(), 2);
        assert_eq!(want[0].doc, DocId(0));
        let (got, stats) = wand_search(&idx, &a, &r, &q, 2).unwrap();
        assert_same_order(&want, &got, "selective");
        assert!(
            stats.scored <= 5,
            "scored={} skipped={}",
            stats.scored,
            stats.skipped
        );
        let (got_bm, bstats) = block_max_wand_search(&idx, &a, &r, &q, 2).unwrap();
        assert_same_order(&want, &got_bm, "selective block-max");
        assert!(bstats.scored <= 5, "bm scored={}", bstats.scored);
    }
}
