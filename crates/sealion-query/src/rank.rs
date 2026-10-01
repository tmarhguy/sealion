//! BM25 field-aware ranking, snippets, and explain (spec §27–29, §42, §50).
//!
//! Exhaustive top-k over Boolean candidates (milestone 07). WAND (09)
//! must produce identical ordering (§41); this module is the baseline it
//! is verified against.
//!
//! Formula per field:
//! ```text
//! IDF(t) = ln(1 + (N - df + 0.5) / (df + 0.5))
//! BM25_f(d, t) = IDF * (tf * (k1+1)) / (tf + k1 * (1 - b + b * len/avgdl))
//! score(d) = Σ_fields weight_f * Σ_terms BM25_f(d, t)
//! ```
//! Phrase queries score as the sum of their constituent terms (no bonus
//! yet; phrase/proximity bonuses are tuned with eval data in milestone 18).

use std::collections::BTreeMap;

use sealion_core::config::{AnalysisConfig, RankingConfig, ScoringKind};
use sealion_core::document::{DocId, Document};
use sealion_core::error::Result;
use sealion_core::field::Field;
use sealion_index::view::IndexView;

use crate::execute::search_view;
use crate::query::Query;

/// One term's contribution in one field.
#[derive(Debug, Clone)]
pub struct TermScore {
    pub field: Field,
    pub term: String,
    pub tf: usize,
    pub df: usize,
    pub idf: f32,
    pub doc_len: usize,
    pub avg_len: f32,
    pub contrib: f32,
}

/// Full score breakdown for `sealion search --explain` (§50).
#[derive(Debug, Clone)]
pub struct Explain {
    pub doc: DocId,
    pub total: f32,
    pub terms: Vec<TermScore>,
    pub field_totals: [(Field, f32); 4],
    /// Link-authority addition (`authority_weight × authority(doc)`, §55).
    pub authority: f32,
    /// Freshness addition (`freshness_weight × recency(doc)`, §56).
    pub freshness: f32,
}

/// A ranked hit: DocId + score + breakdown.
#[derive(Debug, Clone)]
pub struct RankedHit {
    pub doc: DocId,
    pub score: f32,
    pub explain: Explain,
}

/// Positive Lucene-style IDF. `df == 0` yields 0 (term absent everywhere).
pub fn idf(n: usize, df: usize) -> f32 {
    if df == 0 || n == 0 {
        return 0.0;
    }
    let (n, df) = (n as f64, df as f64);
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln() as f32
}

pub(crate) fn field_weight(cfg: &RankingConfig, field: Field) -> f64 {
    match field {
        Field::Title => cfg.title_weight,
        Field::Heading => cfg.heading_weight,
        Field::Body => cfg.body_weight,
        Field::Anchor => cfg.anchor_weight,
    }
}

/// One term-in-field lexical contribution. Shared by exhaustive scoring
/// and WAND so both paths compute bit-identical scores (§41).
///
/// TF-IDF (§27): `(1 + ln(tf)) * idf * weight`. BM25 (§28): standard
/// saturated form with length normalization. Same positive IDF either way.
pub(crate) fn bm25_term_contrib(
    tf: usize,
    df: usize,
    n: usize,
    doc_len: usize,
    avg_len: f64,
    ranking: &RankingConfig,
    weight: f64,
) -> (f32, f32) {
    let idf_v = idf(n, df);
    if idf_v == 0.0 || tf == 0 {
        return (0.0, idf_v);
    }
    let raw = match ranking.scoring {
        ScoringKind::TfIdf => idf_v as f64 * (1.0 + (tf as f64).ln()),
        ScoringKind::Bm25 => {
            let denom = tf as f64
                + ranking.bm25_k1
                    * (1.0 - ranking.bm25_b
                        + ranking.bm25_b * (doc_len as f64 / avg_len.max(1e-9)));
            idf_v as f64 * (tf as f64 * (ranking.bm25_k1 + 1.0)) / denom.max(1e-12)
        }
    };
    ((raw * weight) as f32, idf_v)
}

/// Authority value of a document: `metadata["authority"]` in [0, 1],
/// written by `sealion index authority` (PageRank, milestone 18).
/// Missing/unparseable reads as 0.
pub fn doc_authority(doc: &Document) -> f32 {
    doc.metadata
        .get("authority")
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(0.0)
}

/// Freshness recency in (0, 1]: exponential decay of document age against
/// the corpus-newest timestamp with the configured half-life (§56).
/// Unknown timestamps (0) yield no boost. Newest doc scores exactly 1.0.
pub fn doc_recency(doc_timestamp: u64, max_timestamp: u64, halflife_days: f64) -> f32 {
    if doc_timestamp == 0 || max_timestamp == 0 {
        return 0.0;
    }
    let age_days = max_timestamp.saturating_sub(doc_timestamp) as f64 / 86_400.0;
    let halflife = halflife_days.max(1.0);
    (0.5f64.powf(age_days / halflife)) as f32
}

/// Expand a query into concrete scoped scoring terms. Prefix/fuzzy leaves
/// expand against the live vocabulary; unscoped leaves expand per field.
/// Used by ranking (so `comp*` scores its expansions) and snippets.
pub fn expand_terms<V: IndexView>(view: &V, query: &Query) -> Vec<(Field, String)> {
    let mut vocab: BTreeMap<Field, Vec<String>> = BTreeMap::new();
    for f in Field::ALL {
        vocab.insert(f, view.field_terms(f));
    }
    expand_terms_from_vocab(&vocab, query)
}

/// Same as [`expand_terms`] but over a caller-supplied vocabulary. The
/// distributed coordinator passes the union vocabulary across shards so
/// every shard scores the same term set (§63 comparability).
pub fn expand_terms_from_vocab(
    vocab: &BTreeMap<Field, Vec<String>>,
    query: &Query,
) -> Vec<(Field, String)> {
    let mut out = Vec::new();
    fn walk(vocab: &BTreeMap<Field, Vec<String>>, q: &Query, out: &mut Vec<(Field, String)>) {
        match q {
            Query::Term { field, term } => match field {
                Some(f) => out_push(out, *f, term.clone()),
                None => {
                    for f in Field::ALL {
                        out_push(out, f, term.clone());
                    }
                }
            },
            Query::Phrase { field, terms } => {
                for t in terms {
                    out_push(out, *field, t.clone());
                }
            }
            Query::Prefix { field, prefix } => {
                let fields: &[Field] = match field {
                    Some(f) => std::slice::from_ref(f),
                    None => &Field::ALL,
                };
                for f in fields {
                    for cand in vocab.get(f).map(Vec::as_slice).unwrap_or(&[]) {
                        if cand.starts_with(prefix.as_str()) {
                            out_push(out, *f, cand.clone());
                        }
                    }
                }
            }
            Query::Fuzzy {
                field,
                term,
                distance,
            } => {
                let fields: &[Field] = match field {
                    Some(f) => std::slice::from_ref(f),
                    None => &Field::ALL,
                };
                for f in fields {
                    for cand in vocab.get(f).map(Vec::as_slice).unwrap_or(&[]) {
                        if crate::execute::edit_distance_capped(term, cand, *distance) <= *distance
                        {
                            out_push(out, *f, cand.clone());
                        }
                    }
                }
            }
            Query::Site(_) => {}
            Query::And(children) | Query::Or(children) => {
                for c in children {
                    walk(vocab, c, out);
                }
            }
            Query::Not(_) => {}
            Query::MatchAll | Query::MatchNothing => {}
        }
    }
    fn out_push(out: &mut Vec<(Field, String)>, f: Field, t: String) {
        let key = (f, t);
        if !out.contains(&key) {
            out.push(key);
        }
    }
    walk(vocab, query, &mut out);
    out
}

/// Collection statistics for scoring. Local search fills these from its
/// own view; the distributed coordinator (§63) fills them globally so
/// every shard scores comparably.
#[derive(Debug, Clone)]
pub struct CollectionStats {
    pub n: usize,
    pub df: BTreeMap<(Field, String), usize>,
    pub avg: [f64; 4],
    /// Newest stored-document timestamp across the collection (0 = unknown).
    /// Freshness decays every document against this (§56).
    pub max_timestamp: u64,
}

impl CollectionStats {
    /// Gather from one view (single-node path).
    pub fn collect<V: IndexView>(
        view: &V,
        analysis: &AnalysisConfig,
        scoring_terms: &[(Field, String)],
    ) -> sealion_core::error::Result<Self> {
        let mut df = BTreeMap::new();
        for (f, t) in scoring_terms {
            df.insert((*f, t.clone()), view.postings(*f, t)?.len());
        }
        let n = view.len().max(1);
        let mut avg = [0f64; 4];
        for f in Field::ALL {
            avg[f.index()] = view.field_token_total(f, analysis) as f64 / n as f64;
        }
        let mut max_timestamp = 0u64;
        for id in view.all_doc_ids() {
            if let Some(d) = view.get(id) {
                max_timestamp = max_timestamp.max(d.timestamp);
            }
        }
        Ok(Self {
            n,
            df,
            avg,
            max_timestamp,
        })
    }
}

/// Per-query memo of field lengths: lengths are fixed for the query, so
/// computing each (doc, field) once beats re-analyzing stored documents
/// per term (milestone 16 profiling win; always safe, dropped per query).
pub type LengthMemo = std::collections::HashMap<(DocId, Field), usize>;

pub(crate) fn memo_field_length<V: IndexView>(
    view: &V,
    doc: DocId,
    field: Field,
    analysis: &AnalysisConfig,
    memo: &mut LengthMemo,
) -> usize {
    if let Some(&n) = memo.get(&(doc, field)) {
        return n;
    }
    let n = view.field_length(doc, field, analysis);
    memo.insert((doc, field), n);
    n
}

/// Shared scoring context: one struct instead of an 8-argument call.
pub(crate) struct ScoreCtx<'a, V: IndexView> {
    pub view: &'a V,
    pub analysis: &'a AnalysisConfig,
    pub ranking: &'a RankingConfig,
}

/// Score one document against expanded scoring terms under `stats`
/// (local or global — the caller decides comparability).
///
/// `term_postings` holds each scoring term's list fetched ONCE per query
/// (aligned with `scoring_terms`); per-candidate work is then binary
/// search + memoized lengths instead of full-list clones and re-analysis.
pub(crate) fn score_doc<V: IndexView>(
    ctx: &ScoreCtx<'_, V>,
    scoring_terms: &[(Field, String)],
    term_postings: &[Vec<sealion_index::mem_index::Posting>],
    doc: DocId,
    stats: &CollectionStats,
    lengths: &mut LengthMemo,
) -> (f32, Vec<TermScore>, [(Field, f32); 4], ExtraScores) {
    let (view, analysis, ranking) = (ctx.view, ctx.analysis, ctx.ranking);
    let n = stats.n;
    let mut total = 0f32;
    let mut terms = Vec::new();
    let mut field_totals = [
        (Field::Title, 0f32),
        (Field::Heading, 0f32),
        (Field::Body, 0f32),
        (Field::Anchor, 0f32),
    ];
    for ((f, term), list) in scoring_terms.iter().zip(term_postings.iter()) {
        let df = stats.df.get(&(*f, term.clone())).copied().unwrap_or(0);
        // tf via binary search on the prefetched list (DocId order).
        let tf = match list.binary_search_by(|p| p.doc.cmp(&doc)) {
            Ok(i) => list[i].freq(),
            Err(_) => 0,
        };
        if tf == 0 {
            continue;
        }
        let doc_len = memo_field_length(view, doc, *f, analysis, lengths);
        let avg = stats.avg[f.index()];
        let (contrib, idf_v) =
            bm25_term_contrib(tf, df, n, doc_len, avg, ranking, field_weight(ranking, *f));
        if idf_v == 0.0 {
            continue;
        }
        total += contrib;
        field_totals[f.index()].1 += contrib;
        terms.push(TermScore {
            field: *f,
            term: term.clone(),
            tf,
            df,
            idf: idf_v,
            doc_len,
            avg_len: avg as f32,
            contrib,
        });
    }
    // Authority (§55) and freshness (§56) blends. Weights of 0 (defaults)
    // keep pure lexical ranking; eval must justify anything else.
    let mut authority = 0f32;
    let mut freshness = 0f32;
    if ranking.authority_weight != 0.0 || ranking.freshness_weight != 0.0 {
        if let Some(stored) = view.get(doc) {
            if ranking.authority_weight != 0.0 {
                authority = (ranking.authority_weight as f32) * doc_authority(stored);
                total += authority;
            }
            if ranking.freshness_weight != 0.0 {
                freshness = (ranking.freshness_weight as f32)
                    * doc_recency(
                        stored.timestamp,
                        stats.max_timestamp,
                        ranking.freshness_halflife_days,
                    );
                total += freshness;
            }
        }
    }
    (
        total,
        terms,
        field_totals,
        ExtraScores {
            authority,
            freshness,
        },
    )
}

/// Non-lexical score additions, surfaced in `Explain`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExtraScores {
    pub authority: f32,
    pub freshness: f32,
}

/// Exhaustive ranked search: Boolean candidates scored by BM25, sorted by
/// score desc (DocId asc tiebreak), truncated to `top_k`.
pub fn ranked_search<V: IndexView>(
    view: &V,
    analysis: &AnalysisConfig,
    ranking: &RankingConfig,
    query: &Query,
    top_k: usize,
) -> Result<Vec<RankedHit>> {
    let candidates = search_view(view, query)?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    ranked_search_with_stats(view, analysis, ranking, query, top_k, &candidates, None)
        .map(|(hits, _)| hits)
}

/// Exhaustive ranked search with caller-supplied statistics: pass `None`
/// for single-node local stats, or `Some(global)` for distributed shards
/// (§63) so every shard scores comparably. Returns hits plus the stats
/// actually used (useful for explain/diagnostics).
pub fn ranked_search_with_stats<V: IndexView>(
    view: &V,
    analysis: &AnalysisConfig,
    ranking: &RankingConfig,
    query: &Query,
    top_k: usize,
    candidates: &[DocId],
    global: Option<CollectionStats>,
) -> Result<(Vec<RankedHit>, CollectionStats)> {
    // Expanded scoring terms: prefix/fuzzy leaves expand against the live
    // vocabulary, unscoped leaves expand per field.
    let scoring_terms = expand_terms(view, query);
    let stats = match global {
        Some(g) => g,
        None => CollectionStats::collect(view, analysis, &scoring_terms)?,
    };
    // Fetch-once: each term's postings decoded a single time per query.
    let mut term_postings = Vec::with_capacity(scoring_terms.len());
    for (f, t) in &scoring_terms {
        term_postings.push(view.postings(*f, t)?);
    }
    let mut lengths: LengthMemo = Default::default();
    let ctx = ScoreCtx {
        view,
        analysis,
        ranking,
    };
    let mut hits = Vec::with_capacity(candidates.len());
    for doc in candidates.iter().copied() {
        let (total, terms, field_totals, extra) = score_doc(
            &ctx,
            &scoring_terms,
            &term_postings,
            doc,
            &stats,
            &mut lengths,
        );
        hits.push(RankedHit {
            doc,
            score: total,
            explain: Explain {
                doc,
                total,
                terms,
                field_totals,
                authority: extra.authority,
                freshness: extra.freshness,
            },
        });
    }
    // Score desc, DocId asc for deterministic ties.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc.cmp(&b.doc))
    });
    hits.truncate(top_k);
    Ok((hits, stats))
}

/// Escape `&<>"` for safe HTML snippet rendering (§42).
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Build a snippet around the first query-term match (body preferred,
/// title fallback). Highlights whole words whose analyzed form equals a
/// query term (so stemmed `compil` highlights `compiler`), wraps them in
/// `<mark>`, escapes the rest, caps at `max_len` chars.
pub fn make_snippet(
    doc: &Document,
    query_terms: &[(Option<Field>, &str)],
    analysis: &AnalysisConfig,
    max_len: usize,
) -> String {
    use sealion_index::stemmer::stem;
    use std::collections::BTreeSet;
    let term_set: BTreeSet<&str> = query_terms.iter().map(|(_, t)| *t).collect();
    let stemmer = analysis.stemmer_for(Field::Body);
    // A word matches if its analyzed form is a query term.
    let word_matches = |word: &str| -> bool {
        let lower = word.to_lowercase();
        if lower.is_empty() {
            return false;
        }
        let stemmed = stem(&lower, stemmer);
        term_set.contains(stemmed.as_str())
    };
    let body = if doc.body.is_empty() {
        doc.title.clone()
    } else {
        doc.body.clone()
    };
    let chars: Vec<char> = body.chars().collect();
    // Word spans (char offsets) over alphanumeric runs.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_alphanumeric() {
            let s = i;
            while i < chars.len() && chars[i].is_alphanumeric() {
                i += 1;
            }
            spans.push((s, i));
        } else {
            i += 1;
        }
    }
    // First matching word centers the window.
    let first_match = spans.iter().find(|(s, e)| {
        let w: String = chars[*s..*e].iter().collect();
        word_matches(&w)
    });
    let center = first_match.map(|(s, _)| *s).unwrap_or(0);
    let cs = center.saturating_sub(60).min(chars.len());
    let ce = (cs + max_len).min(chars.len());
    // Highlight spans intersecting the window.
    let mut out = String::new();
    if cs > 0 {
        out.push('…');
    }
    let mut pos = cs;
    for (s, e) in &spans {
        if *e <= cs || *s >= ce {
            continue;
        }
        let s = (*s).max(cs);
        let e = (*e).min(ce);
        if s > pos {
            out.push_str(&escape_html(&chars[pos..s].iter().collect::<String>()));
        }
        let w: String = chars[s..e].iter().collect();
        if word_matches(&w) {
            out.push_str("<mark>");
            out.push_str(&escape_html(&w));
            out.push_str("</mark>");
        } else {
            out.push_str(&escape_html(&w));
        }
        pos = e;
    }
    if pos < ce {
        out.push_str(&escape_html(&chars[pos..ce].iter().collect::<String>()));
    }
    if ce < chars.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sealion_core::config::StemmerKind;
    use sealion_core::document::Source;
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
            source: Source::Synthetic { name: "r".into() },
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
    fn idf_is_positive_and_zero_when_absent() {
        assert_eq!(idf(10, 0), 0.0);
        assert!(idf(10, 1) > idf(10, 5));
        assert!(idf(10, 5) > 0.0);
    }

    #[test]
    fn repeated_term_scores_higher_ceteris_paribus() {
        let (a, r) = cfg();
        let mut idx = MemIndex::new(a.clone());
        idx.add_document(doc(1, "", "compiler compiler compiler"));
        idx.add_document(doc(2, "", "compiler"));
        let q = Query::term(Some(Field::Body), "compiler");
        let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc, DocId(1));
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn title_weight_beats_body() {
        let (a, r) = cfg();
        let mut idx = MemIndex::new(a.clone());
        idx.add_document(doc(1, "compiler", "nothing relevant here"));
        idx.add_document(doc(2, "nothing relevant", "compiler here"));
        let q = Query::term(None, "compiler");
        let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
        assert_eq!(hits.len(), 2);
        // title_weight 4.0 > body_weight 1.0.
        assert_eq!(hits[0].doc, DocId(1));
    }

    #[test]
    fn snippet_highlights_and_escapes() {
        let (a, _) = cfg();
        let d = doc(1, "t", "a <b>compiler</b> optimization guide");
        let terms = vec![(Some(Field::Body), "compiler")];
        let s = make_snippet(&d, &terms, &a, 200);
        assert!(s.contains("<mark>compiler</mark>"), "{s}");
        assert!(s.contains("&lt;b&gt;"), "{s}");
    }

    #[test]
    fn tfidf_baseline_orders_like_bm25_here() {
        // TF-IDF (§27) must also prefer repetition ceteris paribus.
        let (a, _) = cfg();
        let r = RankingConfig {
            scoring: ScoringKind::TfIdf,
            ..RankingConfig::default()
        };
        let mut idx = MemIndex::new(a.clone());
        idx.add_document(doc(1, "", "compiler compiler compiler"));
        idx.add_document(doc(2, "", "compiler"));
        let q = Query::term(Some(Field::Body), "compiler");
        let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc, DocId(1));
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn authority_breaks_lexical_ties() {
        let (a, _) = cfg();
        let mut r = RankingConfig::default();
        let mut idx = MemIndex::new(a.clone());
        // Identical bodies: lexical scores tie.
        let mut d1 = doc(1, "", "compiler");
        let mut d2 = doc(2, "", "compiler");
        d1.metadata.insert("authority".to_string(), "0.2".into());
        d2.metadata.insert("authority".to_string(), "0.9".into());
        idx.add_document(d1);
        idx.add_document(d2);
        let q = Query::term(Some(Field::Body), "compiler");
        // Weight 0 (default): tie → DocId order.
        let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
        assert_eq!(hits[0].doc, DocId(1));
        assert_eq!(hits[0].explain.authority, 0.0);
        // Weight > 0: authority decides, explain shows it.
        r.authority_weight = 5.0;
        let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
        assert_eq!(hits[0].doc, DocId(2));
        assert!((hits[0].explain.authority - 4.5).abs() < 1e-4);
    }

    #[test]
    fn freshness_prefers_newer_docs() {
        let (a, _) = cfg();
        let r = RankingConfig {
            freshness_weight: 5.0,
            freshness_halflife_days: 30.0,
            ..RankingConfig::default()
        };
        let mut idx = MemIndex::new(a.clone());
        let mut old = doc(1, "", "compiler");
        let mut new = doc(2, "", "compiler");
        old.timestamp = 1_000_000;
        new.timestamp = 1_000_000 + 30 * 86_400; // one half-life newer.
        idx.add_document(old);
        idx.add_document(new);
        let q = Query::term(Some(Field::Body), "compiler");
        // Weight 0: tie → DocId order.
        let off = RankingConfig::default();
        let hits = ranked_search(&idx, &a, &off, &q, 10).unwrap();
        assert_eq!(hits[0].doc, DocId(1));
        // Weight > 0: newer wins by ~half the weight (one half-life).
        let hits = ranked_search(&idx, &a, &r, &q, 10).unwrap();
        assert_eq!(hits[0].doc, DocId(2));
        assert!((hits[0].explain.freshness - 5.0).abs() < 1e-4);
        assert!((hits[1].explain.freshness - 2.5).abs() < 1e-4);
    }

    #[test]
    fn recency_math() {
        assert_eq!(doc_recency(0, 100, 30.0), 0.0);
        assert_eq!(doc_recency(100, 0, 30.0), 0.0);
        assert_eq!(doc_recency(100, 100, 30.0), 1.0);
        // One half-life → 0.5.
        assert!((doc_recency(1, 1 + 30 * 86_400, 30.0) - 0.5).abs() < 1e-6);
    }
}
