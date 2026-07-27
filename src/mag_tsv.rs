//! `mag prepare`: complete a MAG sample sheet by filling its `tax_id` column.
//!
//! The sheet is read with the generic [`Table`] reader so every checklist column is preserved in
//! order. For each row we resolve the `scientific_name` cell to an NCBI/ENA taxon id via the ENA
//! taxonomy REST API and write it into the `tax_id` cell; all other cells pass through untouched.
//! Cells that already carry a `tax_id` are left as-is (idempotent re-runs). A name ENA cannot
//! resolve but that identifies a genus — a bare `Bacteroides`, or a GTDB placeholder such as
//! `Phocaeicola sp900556845` — is retried as `{genus} sp.`, the form ENA accepts for submission,
//! and the `scientific_name` cell is rewritten to match. Rows whose name is unknown, ambiguous, or
//! not submittable are collected and reported together as one error.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::input::Table;

/// Base URL of the ENA taxonomy REST service.
const ENA_TAXONOMY_BASE: &str = "https://www.ebi.ac.uk/ena/taxonomy/rest";

/// Give up on a connection that will not open, rather than hanging the whole command.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on waiting for one taxonomy response. Generous: a whole sheet is hundreds of these, but a
/// single stalled read should not block a run indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Complete `input`'s `tax_id` column and write the result to `output`.
pub fn prepare(input: &Path, output: &Path) -> Result<()> {
    let table = Table::read(input)?;
    let resolver = Memoized::new(EnaTaxonomy::new());
    let filled = fill_taxids(&table, &resolver);
    // Logged on both paths: on a failing run this is what the time was spent on.
    tracing::info!(lookups = resolver.lookups(), "taxonomy lookups issued");
    write_table(output, &filled?)?;
    Ok(())
}

/// Outcome of resolving one scientific name.
#[derive(Clone)]
enum Resolution {
    /// Resolved to this taxon id.
    Found(String),
    /// A soft problem to aggregate and report (unknown / ambiguous / not submittable).
    Problem(String),
}

/// Resolves scientific names to taxon ids. Abstracted behind a trait so the fill logic can be
/// exercised offline; the only production implementation is [`EnaTaxonomy`].
trait TaxonomyResolver {
    /// Resolve one name. `Err` signals a hard failure (network/HTTP) that should abort the run;
    /// a soft [`Resolution::Problem`] is returned in `Ok` so it can be aggregated per row.
    fn resolve(&self, scientific_name: &str) -> Result<Resolution>;
}

/// Caches another resolver's answers by name, so each distinct name costs one request per run.
///
/// Sample sheets repeat names heavily — the reference sheet is 2676 rows but only 282 distinct
/// names — and the `"{genus} sp."` fallback collapses them further still, since many placeholder
/// names share one genus. Without this, a full sheet issues thousands of duplicate requests.
struct Memoized<R> {
    inner: R,
    cache: RefCell<HashMap<String, Resolution>>,
}

impl<R: TaxonomyResolver> Memoized<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// How many distinct names were resolved — i.e. how many requests actually reached ENA.
    fn lookups(&self) -> usize {
        self.cache.borrow().len()
    }
}

impl<R: TaxonomyResolver> TaxonomyResolver for Memoized<R> {
    fn resolve(&self, scientific_name: &str) -> Result<Resolution> {
        if let Some(hit) = self.cache.borrow().get(scientific_name) {
            return Ok(hit.clone());
        }
        // Only `Ok` outcomes are cached: a hard failure aborts the run anyway, and caching one
        // would turn a transient network blip into a permanent answer for that name.
        let resolved = self.inner.resolve(scientific_name)?;
        self.cache
            .borrow_mut()
            .insert(scientific_name.to_string(), resolved.clone());
        Ok(resolved)
    }
}

/// Fill the `tax_id` column of `table`, resolving each row's `scientific_name` via `resolver`.
///
/// Returns a new [`Table`] with the same headers/order. Row-level problems (empty or unresolvable
/// names) are collected and returned as a single [`Error::Input`]; a hard resolver failure aborts
/// immediately. A name that does not resolve but identifies a genus is retried as `"{genus} sp."`
/// (see [`genus_fallback`]); on success both `tax_id` and the `scientific_name` cell are written.
fn fill_taxids(table: &Table, resolver: &dyn TaxonomyResolver) -> Result<Table> {
    let sci_col = require_column(table, "scientific_name")?;
    let tax_col = require_column(table, "tax_id")?;

    let mut rows = table.rows.clone();
    let mut problems = Vec::new();
    let mut filled = 0usize;
    let mut kept = 0usize;
    let mut fallbacks = 0usize;

    for (i, row) in rows.iter_mut().enumerate() {
        let number = i + 1;
        // Respect a tax_id the user (or a prior run) already provided.
        if !row[tax_col].trim().is_empty() {
            kept += 1;
            continue;
        }
        let name = row[sci_col].trim().to_string();
        if name.is_empty() {
            problems.push(format!("row {number}: scientific_name is empty"));
            continue;
        }
        match resolve_name(&name, resolver)? {
            RowOutcome::Filled { tax_id, rename } => {
                row[tax_col] = tax_id;
                filled += 1;
                if let Some(resolved_name) = rename {
                    // ENA checks scientific_name against tax_id, so the cell must follow.
                    row[sci_col] = resolved_name;
                    fallbacks += 1;
                    tracing::info!(row = number, from = %name, "genus fallback applied");
                }
            }
            RowOutcome::Problem(reason) => problems.push(format!("row {number}: {reason}")),
        }
    }

    if !problems.is_empty() {
        return Err(Error::Input {
            path: table.path.clone(),
            message: problems.join("\n"),
        });
    }
    tracing::info!(filled, kept, fallbacks, "tax_id resolution complete");
    if fallbacks > 0 {
        println!("Rewrote {fallbacks} scientific_name cell(s) to the \"<genus> sp.\" form");
    }
    Ok(Table {
        path: table.path.clone(),
        headers: table.headers.clone(),
        rows,
    })
}

/// What one row's `scientific_name` resolved to.
enum RowOutcome {
    /// Use this taxon id. `rename` carries the `"{genus} sp."` name to write back into the sheet
    /// when the fallback — not the name as written — is what resolved.
    Filled {
        tax_id: String,
        rename: Option<String>,
    },
    /// A soft problem to aggregate and report.
    Problem(String),
}

/// Resolve one `scientific_name`, applying the `"{genus} sp."` fallback where it applies.
///
/// A GTDB placeholder goes straight to the fallback: a `sp<digits>` epithet is a GTDB accession
/// that matches nothing in ENA (the endpoint answers with an empty list for these), so looking the
/// name up as written spends a request that cannot succeed. Every other name — including a bare
/// genus, which does resolve, to a non-submittable taxon — is tried as written first, so a name
/// ENA accepts is never second-guessed.
fn resolve_name(name: &str, resolver: &dyn TaxonomyResolver) -> Result<RowOutcome> {
    match genus_fallback(name) {
        Some(candidate) if is_gtdb_placeholder(name) => {
            Ok(match resolver.resolve(&candidate)? {
                Resolution::Found(tax_id) => RowOutcome::Filled {
                    tax_id,
                    rename: Some(candidate),
                },
                // Say plainly that the name as written was never looked up.
                Resolution::Problem(reason) => RowOutcome::Problem(format!(
                    "'{name}' is a GTDB placeholder, not an ENA name; \
                     genus fallback '{candidate}' failed: {reason}"
                )),
            })
        }
        candidate => Ok(match resolver.resolve(name)? {
            Resolution::Found(tax_id) => RowOutcome::Filled {
                tax_id,
                rename: None,
            },
            Resolution::Problem(reason) => match candidate {
                // The cell identifies a genus: retry the name ENA actually accepts.
                Some(candidate) => match resolver.resolve(&candidate)? {
                    Resolution::Found(tax_id) => RowOutcome::Filled {
                        tax_id,
                        rename: Some(candidate),
                    },
                    Resolution::Problem(second) => RowOutcome::Problem(format!(
                        "{reason}; genus fallback '{candidate}' also failed: {second}"
                    )),
                },
                None => RowOutcome::Problem(reason),
            },
        }),
    }
}

/// Whether `name` is a GTDB placeholder binomial: a genus followed by a `sp<digits>` epithet.
fn is_gtdb_placeholder(name: &str) -> bool {
    let mut tokens = name.split_whitespace();
    tokens.next().is_some_and(is_genus_token)
        && tokens.next_back().is_some_and(is_gtdb_placeholder_epithet)
}

/// The submittable `"{genus} sp."` name to try when `name` itself names no submittable taxon.
///
/// Two shapes qualify, both resolving to the genus in the first token:
/// - a bare genus — `Bacteroides` → `Bacteroides sp.`;
/// - a GTDB placeholder binomial, whose last token is `sp` followed by digits — `Phocaeicola
///   sp900556845` → `Phocaeicola sp.`, `Clostridium AQ sp000165065` → `Clostridium sp.`. These
///   accessioned epithets exist only in GTDB, so ENA can never match them as written.
///
/// Anything else — a real binomial (`Homo sapiens`), an already-suffixed name (`Bacteroides sp.`) —
/// yields `None` and is reported as before.
fn genus_fallback(name: &str) -> Option<String> {
    let mut tokens = name.split_whitespace();
    let genus = tokens.next().filter(|t| is_genus_token(t))?;
    match tokens.next_back() {
        // A bare genus.
        None => Some(format!("{genus} sp.")),
        // A GTDB placeholder: strip the accessioned epithet (and any GTDB genus suffix between).
        Some(last) if is_gtdb_placeholder_epithet(last) => Some(format!("{genus} sp.")),
        Some(_) => None,
    }
}

/// Whether `token` looks like a genus name: at least two ASCII letters, initial capital.
fn is_genus_token(token: &str) -> bool {
    token.len() >= 2
        && token.starts_with(|c: char| c.is_ascii_uppercase())
        && token.chars().all(|c| c.is_ascii_alphabetic())
}

/// Whether `token` is a GTDB accessioned epithet: `sp` followed by digits (`sp900556845`).
fn is_gtdb_placeholder_epithet(token: &str) -> bool {
    token
        .strip_prefix("sp")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// Locate a required column index, or an [`Error::Input`] naming the missing column.
fn require_column(table: &Table, name: &str) -> Result<usize> {
    table.column(name).ok_or_else(|| Error::Input {
        path: table.path.clone(),
        message: format!("missing required column: {name}"),
    })
}

/// Write a [`Table`] back out as TSV (header row + data rows), overwriting `path`.
fn write_table(path: &Path, table: &Table) -> Result<()> {
    let mut out = String::new();
    out.push_str(&table.headers.join("\t"));
    out.push('\n');
    for row in &table.rows {
        out.push_str(&row.join("\t"));
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| Error::io(path, e))?;
    println!("Wrote completed sample sheet: {}", path.display());
    Ok(())
}

/// Live resolver backed by the ENA taxonomy REST service.
struct EnaTaxonomy {
    base: String,
    /// Pooled connections. A sheet issues hundreds of lookups back-to-back against one host, and a
    /// fresh agent per call would pay a TCP + TLS handshake for each (measured: ~0.7s of the ~1.1s
    /// per request). The agent also carries the timeouts, which `ureq::get` alone does not set.
    agent: ureq::Agent,
}

impl EnaTaxonomy {
    fn new() -> Self {
        Self {
            base: ENA_TAXONOMY_BASE.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout_read(READ_TIMEOUT)
                .build(),
        }
    }

    /// GET `url` and decode the JSON array of taxa. A 404 (nothing matched) yields an empty list;
    /// other non-2xx statuses and transport/decoding failures become [`Error::Network`].
    fn fetch_taxa(&self, url: &str) -> Result<Vec<TaxonEntry>> {
        match self.agent.get(url).call() {
            Ok(resp) => resp
                .into_json::<Vec<TaxonEntry>>()
                .map_err(|e| Error::Network {
                    url: url.to_string(),
                    message: format!("could not decode taxonomy response: {e}"),
                }),
            // ENA answers with 404 and a plain-text body when a name matches nothing.
            Err(ureq::Error::Status(404, _)) => Ok(Vec::new()),
            Err(ureq::Error::Status(code, _)) => Err(Error::Network {
                url: url.to_string(),
                message: format!("HTTP {code}"),
            }),
            Err(e) => Err(Error::Network {
                url: url.to_string(),
                message: e.to_string(),
            }),
        }
    }
}

/// The subset of an ENA taxonomy entry we consume; all other fields are ignored.
#[derive(Debug, Deserialize)]
struct TaxonEntry {
    #[serde(rename = "taxId")]
    tax_id: String,
    /// ENA's `"true"`/`"false"` flag for whether the taxon may be used in a submission.
    #[serde(default)]
    submittable: Option<String>,
}

impl TaxonomyResolver for EnaTaxonomy {
    fn resolve(&self, scientific_name: &str) -> Result<Resolution> {
        let url = format!(
            "{}/scientific-name/{}",
            self.base,
            encode_path_segment(scientific_name)
        );
        let entries = self.fetch_taxa(&url)?;
        Ok(classify(scientific_name, entries))
    }
}

/// Turn a taxonomy lookup result into a [`Resolution`]: exactly one submittable match is `Found`;
/// zero matches, multiple matches, and non-submittable matches are reportable problems.
fn classify(name: &str, mut entries: Vec<TaxonEntry>) -> Resolution {
    match entries.len() {
        0 => Resolution::Problem(format!("no ENA taxon matches scientific_name '{name}'")),
        1 => {
            let taxon = entries.remove(0);
            if is_submittable(&taxon) {
                Resolution::Found(taxon.tax_id)
            } else {
                Resolution::Problem(format!(
                    "scientific_name '{name}' (taxId {}) is not submittable to ENA",
                    taxon.tax_id
                ))
            }
        }
        _ => {
            let ids: Vec<&str> = entries.iter().map(|t| t.tax_id.as_str()).collect();
            Resolution::Problem(format!(
                "scientific_name '{name}' is ambiguous: matches taxIds {}",
                ids.join(", ")
            ))
        }
    }
}

/// ENA reports submittability as the string `"true"`.
fn is_submittable(taxon: &TaxonEntry) -> bool {
    taxon
        .submittable
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("true"))
}

/// Percent-encode a single URL path segment (RFC 3986 unreserved bytes pass through). Scientific
/// names carry spaces and other characters that must be escaped for the taxonomy endpoint.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Resolver driven by a fixed name -> taxId map; unmapped names report a problem. Records every
    /// name it is asked for, in order, so tests can assert which lookups were actually issued.
    struct FakeResolver {
        found: HashMap<&'static str, &'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeResolver {
        /// The names looked up so far, in call order.
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl TaxonomyResolver for FakeResolver {
        fn resolve(&self, name: &str) -> Result<Resolution> {
            self.calls.borrow_mut().push(name.to_string());
            Ok(match self.found.get(name) {
                Some(id) => Resolution::Found((*id).to_string()),
                None => {
                    Resolution::Problem(format!("no ENA taxon matches scientific_name '{name}'"))
                }
            })
        }
    }

    fn table(text: &str) -> Table {
        Table::parse(PathBuf::from("mag_samples.tsv"), text).unwrap()
    }

    fn resolver(pairs: &[(&'static str, &'static str)]) -> FakeResolver {
        FakeResolver {
            found: pairs.iter().copied().collect(),
            calls: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn fills_empty_tax_id_and_preserves_other_columns() {
        let t = table(
            "sample_alias\ttax_id\tscientific_name\tbiome\nbin.1\t\tHomo sapiens\thuman gut\n",
        );
        let r = resolver(&[("Homo sapiens", "9606")]);
        let filled = fill_taxids(&t, &r).unwrap();

        assert_eq!(
            filled.headers,
            ["sample_alias", "tax_id", "scientific_name", "biome"]
        );
        assert_eq!(
            filled.rows[0],
            ["bin.1", "9606", "Homo sapiens", "human gut"]
        );
    }

    #[test]
    fn keeps_already_filled_tax_id() {
        // The mapped taxId differs from the pre-filled one; the pre-filled value must win.
        let t = table("tax_id\tscientific_name\n42\tHomo sapiens\n");
        let r = resolver(&[("Homo sapiens", "9606")]);
        let filled = fill_taxids(&t, &r).unwrap();
        assert_eq!(filled.rows[0], ["42", "Homo sapiens"]);
    }

    #[test]
    fn aggregates_unresolved_rows_with_row_numbers() {
        // The `alias` cell keeps the third row from being an all-whitespace line, which the generic
        // Table reader would otherwise skip as blank.
        let t = table(
            "alias\ttax_id\tscientific_name\nbin.1\t\tHomo sapiens\nbin.2\t\tNot A Species\nbin.3\t\t\n",
        );
        let r = resolver(&[("Homo sapiens", "9606")]);
        let err = fill_taxids(&t, &r).unwrap_err().to_string();
        assert!(err.contains("row 2"), "got: {err}");
        assert!(err.contains("Not A Species"), "got: {err}");
        assert!(err.contains("row 3"), "got: {err}");
        assert!(err.contains("scientific_name is empty"), "got: {err}");
        // Row 1 resolved fine, so it must not appear.
        assert!(!err.contains("row 1"), "got: {err}");
    }

    #[test]
    fn genus_only_name_falls_back_to_sp() {
        // Only the `sp.` form is known, mirroring ENA: the bare genus is not submittable.
        let t = table("tax_id\tscientific_name\n\tBacteroides\n");
        let r = resolver(&[("Bacteroides sp.", "820")]);
        let filled = fill_taxids(&t, &r).unwrap();
        assert_eq!(filled.rows[0], ["820", "Bacteroides sp."]);
    }

    #[test]
    fn direct_hit_wins_over_fallback() {
        let t = table("tax_id\tscientific_name\n\tBacteroides\n");
        let r = resolver(&[("Bacteroides", "816"), ("Bacteroides sp.", "820")]);
        let filled = fill_taxids(&t, &r).unwrap();
        assert_eq!(filled.rows[0], ["816", "Bacteroides"]);
    }

    #[test]
    fn gtdb_placeholder_falls_back_to_genus_sp() {
        // `sp900556845` is a GTDB accession, unknown to ENA; the genus is the usable part.
        let t = table("tax_id\tscientific_name\n\tPhocaeicola sp900556845\n");
        let r = resolver(&[("Phocaeicola sp.", "310298")]);
        let filled = fill_taxids(&t, &r).unwrap();
        assert_eq!(filled.rows[0], ["310298", "Phocaeicola sp."]);
    }

    #[test]
    fn gtdb_placeholder_with_genus_suffix_falls_back_to_the_first_token() {
        // GTDB's `Clostridium AQ sp000165065`: the suffix goes with the epithet.
        let t = table("tax_id\tscientific_name\n\tClostridium AQ sp000165065\n");
        let r = resolver(&[("Clostridium sp.", "1506")]);
        let filled = fill_taxids(&t, &r).unwrap();
        assert_eq!(filled.rows[0], ["1506", "Clostridium sp."]);
    }

    #[test]
    fn genus_fallback_not_tried_for_real_binomials() {
        let t = table("tax_id\tscientific_name\n\tPhocaeicola vulgatus\n");
        let err = fill_taxids(&t, &resolver(&[])).unwrap_err().to_string();
        assert!(!err.contains("genus fallback"), "got: {err}");
        assert!(err.contains("Phocaeicola vulgatus"), "got: {err}");
    }

    #[test]
    fn gtdb_placeholder_skips_the_doomed_direct_lookup() {
        // `sp<digits>` is a GTDB accession that ENA never matches, so only the fallback is issued.
        let t = table("tax_id\tscientific_name\n\tPhocaeicola sp900556845\n");
        let r = resolver(&[("Phocaeicola sp.", "310298")]);
        fill_taxids(&t, &r).unwrap();
        assert_eq!(r.calls(), ["Phocaeicola sp."]);
    }

    #[test]
    fn bare_genus_still_tries_the_name_as_written_first() {
        // A bare genus does resolve (to a non-submittable taxon), so the direct lookup is kept.
        let t = table("tax_id\tscientific_name\n\tBacteroides\n");
        let r = resolver(&[("Bacteroides sp.", "820")]);
        fill_taxids(&t, &r).unwrap();
        assert_eq!(r.calls(), ["Bacteroides", "Bacteroides sp."]);
    }

    #[test]
    fn unresolvable_placeholder_says_it_was_never_looked_up_directly() {
        let t = table("tax_id\tscientific_name\n\tNotagenus sp900556845\n");
        let err = fill_taxids(&t, &resolver(&[])).unwrap_err().to_string();
        assert!(err.contains("is a GTDB placeholder"), "got: {err}");
        assert!(err.contains("'Notagenus sp.'"), "got: {err}");
    }

    #[test]
    fn repeated_names_are_resolved_once() {
        let t = table("tax_id\tscientific_name\n\tHomo sapiens\n\tHomo sapiens\n\tHomo sapiens\n");
        let r = Memoized::new(resolver(&[("Homo sapiens", "9606")]));
        let filled = fill_taxids(&t, &r).unwrap();

        assert_eq!(r.inner.calls(), ["Homo sapiens"]);
        // Every row is still filled, not just the one that missed the cache.
        for row in &filled.rows {
            assert_eq!(row[0], "9606");
        }
    }

    #[test]
    fn names_sharing_a_fallback_candidate_resolve_it_once() {
        // Two distinct placeholders of the same genus collapse onto one `sp.` lookup.
        let t = table(
            "tax_id\tscientific_name\n\tPhocaeicola sp900556845\n\tPhocaeicola sp000432335\n",
        );
        let r = Memoized::new(resolver(&[("Phocaeicola sp.", "310298")]));
        let filled = fill_taxids(&t, &r).unwrap();

        assert_eq!(r.inner.calls(), ["Phocaeicola sp."]);
        assert_eq!(filled.rows[0], ["310298", "Phocaeicola sp."]);
        assert_eq!(filled.rows[1], ["310298", "Phocaeicola sp."]);
    }

    #[test]
    fn a_cached_problem_is_reported_for_every_affected_row() {
        // Caching must not swallow rows: the second row's problem is still aggregated.
        let t = table(
            "alias\ttax_id\tscientific_name\nbin.1\t\tNot A Species\nbin.2\t\tNot A Species\n",
        );
        let r = Memoized::new(resolver(&[]));
        let err = fill_taxids(&t, &r).unwrap_err().to_string();

        assert_eq!(r.inner.calls(), ["Not A Species"]);
        assert!(err.contains("row 1"), "got: {err}");
        assert!(err.contains("row 2"), "got: {err}");
    }

    #[test]
    fn genus_fallback_failure_reports_both_attempts() {
        let t = table("tax_id\tscientific_name\n\tNotagenus\n");
        let err = fill_taxids(&t, &resolver(&[])).unwrap_err().to_string();
        assert!(err.contains("'Notagenus'"), "got: {err}");
        assert!(err.contains("genus fallback 'Notagenus sp.'"), "got: {err}");
    }

    #[test]
    fn genus_fallback_recognises_bare_genera_and_gtdb_placeholders() {
        for (name, want) in [
            ("Bacteroides", "Bacteroides sp."),
            ("Phocaeicola sp900556845", "Phocaeicola sp."),
            ("Clostridium AQ sp000165065", "Clostridium sp."),
            ("Phocaeicola sp1", "Phocaeicola sp."),
        ] {
            assert_eq!(genus_fallback(name).as_deref(), Some(want), "for {name:?}");
        }
        for name in [
            "Homo sapiens",
            // `sp` with no digits is a real epithet fragment, not a GTDB accession.
            "Phocaeicola sp",
            "Clostridium AQ innocuum",
            "sp900556845",
            "bacteroides",
            "B",
            "",
            "Bacteroides sp.",
        ] {
            assert_eq!(
                genus_fallback(name),
                None,
                "expected no fallback for {name:?}"
            );
        }
    }

    #[test]
    fn missing_scientific_name_column_is_error() {
        let t = table("sample_alias\ttax_id\nbin.1\t\n");
        let r = resolver(&[]);
        let err = fill_taxids(&t, &r).unwrap_err().to_string();
        assert!(
            err.contains("missing required column: scientific_name"),
            "got: {err}"
        );
    }

    #[test]
    fn missing_tax_id_column_is_error() {
        let t = table("scientific_name\nHomo sapiens\n");
        let r = resolver(&[]);
        let err = fill_taxids(&t, &r).unwrap_err().to_string();
        assert!(
            err.contains("missing required column: tax_id"),
            "got: {err}"
        );
    }

    fn entry(tax_id: &str, submittable: Option<&str>) -> TaxonEntry {
        TaxonEntry {
            tax_id: tax_id.to_string(),
            submittable: submittable.map(str::to_string),
        }
    }

    #[test]
    fn classify_single_submittable_is_found() {
        match classify("x", vec![entry("9606", Some("true"))]) {
            Resolution::Found(id) => assert_eq!(id, "9606"),
            Resolution::Problem(p) => panic!("expected Found, got: {p}"),
        }
    }

    #[test]
    fn classify_non_submittable_is_problem() {
        let p = match classify("x", vec![entry("1", Some("false"))]) {
            Resolution::Problem(p) => p,
            Resolution::Found(_) => panic!("expected Problem"),
        };
        assert!(p.contains("not submittable"), "got: {p}");
    }

    #[test]
    fn classify_zero_matches_is_problem() {
        let p = match classify("x", vec![]) {
            Resolution::Problem(p) => p,
            Resolution::Found(_) => panic!("expected Problem"),
        };
        assert!(p.contains("no ENA taxon"), "got: {p}");
    }

    #[test]
    fn classify_multiple_matches_is_ambiguous() {
        let p = match classify(
            "x",
            vec![entry("1", Some("true")), entry("2", Some("true"))],
        ) {
            Resolution::Problem(p) => p,
            Resolution::Found(_) => panic!("expected Problem"),
        };
        assert!(p.contains("ambiguous"), "got: {p}");
        assert!(p.contains('1') && p.contains('2'), "got: {p}");
    }

    #[test]
    fn encodes_spaces_but_not_unreserved() {
        assert_eq!(
            encode_path_segment("uncultured Bacteroides sp."),
            "uncultured%20Bacteroides%20sp."
        );
    }

    #[test]
    fn write_table_round_trips_through_reader() {
        let t = table("sample_alias\ttax_id\tscientific_name\nbin.1\t9606\tHomo sapiens\n");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tsv");
        write_table(&path, &t).unwrap();

        let back = Table::read(&path).unwrap();
        assert_eq!(back.headers, t.headers);
        assert_eq!(back.rows, t.rows);
    }

    #[test]
    fn fill_then_write_produces_exact_tsv() {
        let t = table("tax_id\tscientific_name\n\tHomo sapiens\n");
        let filled = fill_taxids(&t, &resolver(&[("Homo sapiens", "9606")])).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("filled.tsv");
        write_table(&path, &filled).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "tax_id\tscientific_name\n9606\tHomo sapiens\n");
    }
}
