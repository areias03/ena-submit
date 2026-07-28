//! `mag prepare`: complete a MAG sample sheet by filling its `tax_id` column.
//!
//! The sheet is read with the generic [`Table`] reader so every checklist column is preserved in
//! order. Each row is resolved to a taxon ENA will accept, and both its `tax_id` and its
//! `scientific_name` cell are written, since ENA checks the two against each other. All other
//! cells pass through untouched, and rows that already carry a `tax_id` are left as-is (idempotent
//! re-runs).
//!
//! Resolution takes the row's **reference genome accession** (`GTDBtk fastani Ref`) as its key:
//! [`crate::ncbi`] maps it to an NCBI species taxon id, and ENA confirms that id by an exact
//! by-id lookup. Matching GTDB's *names* against ENA instead fails on ~19% of a real sheet, since
//! many exist only in GTDB or have been renamed in ENA — see ADR 0008.
//!
//! Rows with no usable accession — GTDB-Tk writes `0` when it assigned no species — fall back to
//! looking names up in ENA, walking the GTDB lineage from the species down to the phylum until one
//! resolves (see [`fallback_chain`]). Rows whose classification is malformed, or that no candidate
//! resolves, are collected and reported together as one error.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::gtdb::{self, Lineage};
use crate::input::Table;
use crate::ncbi::{NcbiDatasets, TaxonSource};

/// Base URL of the ENA taxonomy REST service.
const ENA_TAXONOMY_BASE: &str = "https://www.ebi.ac.uk/ena/taxonomy/rest";

/// Give up on a connection that will not open, rather than hanging the whole command.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on waiting for one taxonomy response. Generous: a whole sheet is hundreds of these, but a
/// single stalled read should not block a run indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Header of the column holding the accession of the reference genome GTDB-Tk matched each bin to.
const REFERENCE_COLUMN: &str = "GTDBtk fastani Ref";

/// Values of [`REFERENCE_COLUMN`] that mean "GTDB-Tk matched no reference". `0` is what GTDB-Tk
/// itself writes; the others are what people write by hand for the same thing.
const NO_REFERENCE: [&str; 3] = ["0", "n/a", "na"];

/// Complete `input`'s `tax_id` column and write the result to `output`.
pub fn prepare(input: &Path, output: &Path) -> Result<()> {
    let table = Table::read(input)?;
    let resolver = Memoized::new(EnaTaxonomy::new());
    let filled = fill_taxids(&table, &resolver, &NcbiDatasets::new());
    // Logged on both paths: on a failing run this is what the time was spent on.
    tracing::info!(lookups = resolver.lookups(), "ENA taxonomy lookups issued");
    write_table(output, &filled?)?;
    Ok(())
}

/// Outcome of resolving one row to a taxon.
#[derive(Clone)]
enum Resolution {
    /// Resolved to this taxon id, whose ENA scientific name is `name`.
    Found { tax_id: String, name: String },
    /// A soft problem to aggregate and report (unknown / ambiguous / not submittable).
    Problem(String),
}

/// Resolves taxa against ENA. Abstracted behind a trait so the fill logic can be exercised
/// offline; the only production implementation is [`EnaTaxonomy`].
trait TaxonomyResolver {
    /// Resolve one scientific name. `Err` signals a hard failure (network/HTTP) that should abort
    /// the run; a soft [`Resolution::Problem`] is returned in `Ok` so it can be aggregated per row.
    fn resolve(&self, scientific_name: &str) -> Result<Resolution>;

    /// Resolve one taxon id, yielding ENA's own name for it. This is the primary path: the id
    /// comes from NCBI via the row's reference accession, and ENA only has to confirm it.
    fn resolve_id(&self, tax_id: &str) -> Result<Resolution>;
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

impl<R: TaxonomyResolver> Memoized<R> {
    /// Run `lookup` unless `key` is already cached. Only `Ok` outcomes are cached: a hard failure
    /// aborts the run anyway, and caching one would turn a transient network blip into a permanent
    /// answer for that key.
    fn cached(
        &self,
        key: String,
        lookup: impl FnOnce() -> Result<Resolution>,
    ) -> Result<Resolution> {
        if let Some(hit) = self.cache.borrow().get(&key) {
            return Ok(hit.clone());
        }
        let resolved = lookup()?;
        self.cache.borrow_mut().insert(key, resolved.clone());
        Ok(resolved)
    }
}

impl<R: TaxonomyResolver> TaxonomyResolver for Memoized<R> {
    fn resolve(&self, scientific_name: &str) -> Result<Resolution> {
        // Namespaced so a name can never collide with a taxon id in the same cache.
        self.cached(format!("name:{scientific_name}"), || {
            self.inner.resolve(scientific_name)
        })
    }

    fn resolve_id(&self, tax_id: &str) -> Result<Resolution> {
        self.cached(format!("id:{tax_id}"), || self.inner.resolve_id(tax_id))
    }
}

/// Fill the `tax_id` column of `table`, resolving each row via `taxa` (accession → NCBI species
/// id) and confirming the result against ENA with `resolver`.
///
/// Returns a new [`Table`] with the same headers/order. Row-level problems (empty cells, malformed
/// classifications, rows nothing resolves) are collected and returned as a single [`Error::Input`];
/// a hard network failure aborts immediately. Both the `tax_id` and the `scientific_name` cell are
/// written, so the sheet's name always matches the id beside it.
fn fill_taxids(
    table: &Table,
    resolver: &dyn TaxonomyResolver,
    taxa: &dyn TaxonSource,
) -> Result<Table> {
    let sci_col = require_column(table, "scientific_name")?;
    let tax_col = require_column(table, "tax_id")?;
    let ref_col = require_column(table, REFERENCE_COLUMN)?;

    let mut rows = table.rows.clone();
    let pending: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| row[tax_col].trim().is_empty())
        .collect();
    // One batched pass over every accession the sheet still needs, before any row is resolved.
    let mut accessions: Vec<String> = pending
        .iter()
        .filter_map(|row| reference_accession(&row[ref_col]))
        .map(str::to_string)
        .collect();
    accessions.sort_unstable();
    accessions.dedup();
    let species = taxa.species_taxids(&accessions)?;
    tracing::info!(
        accessions = accessions.len(),
        resolved = species.len(),
        "reference accessions mapped to NCBI species taxa"
    );
    // A cell holding a real accession NCBI cannot resolve is a stale reference — the assembly was
    // suppressed or replaced — not the "GTDB-Tk matched nothing" case that `0` marks. Both fall
    // back to the classification, but only this one points at a sheet that needs refreshing.
    for (accession, rows) in unresolved_accessions(&pending, ref_col, &species) {
        tracing::warn!(
            accession,
            rows,
            "reference accession not found in NCBI; falling back to the classification"
        );
    }

    let mut problems = Vec::new();
    let mut filled = 0usize;
    let mut kept = 0usize;
    let mut rewritten = 0usize;
    let mut fallbacks = 0usize;

    for (i, row) in rows.iter_mut().enumerate() {
        let number = i + 1;
        // Respect a tax_id the user (or a prior run) already provided.
        if !row[tax_col].trim().is_empty() {
            kept += 1;
            continue;
        }
        let cell = row[sci_col].trim().to_string();
        if cell.is_empty() {
            problems.push(format!("row {number}: scientific_name is empty"));
            continue;
        }
        let lineage = match gtdb::parse(&cell) {
            Ok(lineage) => lineage,
            Err(reason) => {
                problems.push(format!("row {number}: {reason}"));
                continue;
            }
        };
        let taxid = reference_accession(&row[ref_col]).and_then(|acc| species.get(acc));
        let outcome = match taxid {
            Some(taxid) => resolve_taxid(taxid, resolver)?,
            None => resolve_by_name(&lineage, resolver)?,
        };
        match outcome {
            RowOutcome::Filled {
                tax_id,
                name,
                via_accession,
            } => {
                row[tax_col] = tax_id;
                filled += 1;
                if !via_accession {
                    fallbacks += 1;
                    tracing::info!(row = number, from = %cell, to = %name, "resolved by name");
                }
                if name != cell {
                    // ENA checks scientific_name against tax_id, so the cell must follow.
                    row[sci_col] = name;
                    rewritten += 1;
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
    tracing::info!(
        filled,
        kept,
        rewritten,
        fallbacks,
        "tax_id resolution complete"
    );
    if rewritten > 0 {
        println!(
            "Rewrote {rewritten} scientific_name cell(s) from their GTDB classification \
             ({fallbacks} resolved from the lineage, the rest from the reference accession)"
        );
    }
    Ok(Table {
        path: table.path.clone(),
        headers: table.headers.clone(),
        rows,
    })
}

/// The accessions `pending` asks for that NCBI could not resolve, mapped to how many rows each
/// affects.
///
/// Reported per accession rather than per row: the accession is the thing to fix, and one stale
/// reference typically covers many bins. `BTreeMap` keeps the warnings in a stable order.
fn unresolved_accessions<'a>(
    pending: &[&'a Vec<String>],
    ref_col: usize,
    species: &HashMap<String, String>,
) -> BTreeMap<&'a str, usize> {
    let mut missing = BTreeMap::new();
    for row in pending {
        if let Some(accession) = reference_accession(&row[ref_col])
            && !species.contains_key(accession)
        {
            *missing.entry(accession).or_default() += 1;
        }
    }
    missing
}

/// The usable reference accession in a [`REFERENCE_COLUMN`] cell, or `None` when GTDB-Tk recorded
/// no match (it writes `0`) or the cell is blank.
fn reference_accession(cell: &str) -> Option<&str> {
    let cell = cell.trim();
    let usable = !cell.is_empty()
        && !NO_REFERENCE
            .iter()
            .any(|placeholder| cell.eq_ignore_ascii_case(placeholder));
    usable.then_some(cell)
}

/// What one row resolved to.
enum RowOutcome {
    /// Use this taxon id, and write `name` — ENA's own name for it — into the sheet.
    /// `via_accession` distinguishes the primary path from the lineage fallback, for reporting.
    Filled {
        tax_id: String,
        name: String,
        via_accession: bool,
    },
    /// A soft problem to aggregate and report.
    Problem(String),
}

/// Confirm an NCBI taxon id against ENA and take ENA's name for it.
fn resolve_taxid(tax_id: &str, resolver: &dyn TaxonomyResolver) -> Result<RowOutcome> {
    Ok(match resolver.resolve_id(tax_id)? {
        Resolution::Found { tax_id, name } => RowOutcome::Filled {
            tax_id,
            name,
            via_accession: true,
        },
        Resolution::Problem(reason) => RowOutcome::Problem(format!(
            "reference genome resolves to NCBI taxon {tax_id}, which {reason}"
        )),
    })
}

/// Resolve a row that has no usable reference accession by looking names from its GTDB lineage up
/// in ENA, taking the first that resolves.
fn resolve_by_name(lineage: &Lineage, resolver: &dyn TaxonomyResolver) -> Result<RowOutcome> {
    let candidates = fallback_chain(lineage);
    if candidates.is_empty() {
        // Nothing between the domain and the species is populated, so there is no name to try.
        let deepest = match lineage.deepest() {
            Some((rank, name)) => format!("its deepest rank is {rank}__{name}"),
            None => "it is empty at every rank".to_string(),
        };
        return Ok(RowOutcome::Problem(format!(
            "no reference accession, and the classification names no usable rank ({deepest})"
        )));
    }
    let mut last = None;
    for candidate in &candidates {
        match resolver.resolve(candidate)? {
            Resolution::Found { tax_id, name } => {
                return Ok(RowOutcome::Filled {
                    tax_id,
                    name,
                    via_accession: false,
                });
            }
            Resolution::Problem(reason) => last = Some(reason),
        }
    }
    Ok(RowOutcome::Problem(format!(
        "no reference accession, and no ENA taxon for any of {} (last: {})",
        candidates
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(", "),
        last.unwrap_or_default()
    )))
}

/// The ENA names to try for a row with no usable reference accession, deepest rank first.
///
/// The species and `"{genus} sp."` steps are the shapes ENA accepts for a named organism; below
/// them the chain climbs the lineage as `"{rank} bacterium"`, which is how NCBI names taxa for
/// bins identified no further than a family or order. The domain is never tried — `"Bacteria
/// bacterium"` names nothing.
///
/// A GTDB placeholder species is skipped rather than tried: a `sp<digits>` epithet is a GTDB
/// accession that matches nothing in ENA, so looking it up spends a request that cannot succeed.
fn fallback_chain(lineage: &Lineage) -> Vec<String> {
    let mut chain = Vec::new();
    let genus = lineage.rank('g');
    match lineage.rank('s') {
        Some(species) => {
            if !is_gtdb_placeholder(species) {
                chain.push(species.to_string());
            }
            if let Some(genus) = genus {
                chain.push(format!("{genus} sp."));
            }
        }
        None => {
            if let Some(genus) = genus {
                // `uncultured {genus} sp.` is the form NCBI uses where a plain `{genus} sp.` has
                // no record — the two are never both present, so trying both costs one lookup.
                chain.push(format!("{genus} sp."));
                chain.push(format!("uncultured {genus} sp."));
            }
        }
    }
    for rank in ['f', 'o', 'c', 'p'] {
        if let Some(name) = lineage.rank(rank) {
            chain.push(format!("{name} bacterium"));
        }
    }
    chain
}

/// Whether `name` is a GTDB placeholder binomial: a genus followed by a `sp<digits>` epithet.
fn is_gtdb_placeholder(name: &str) -> bool {
    let mut tokens = name.split_whitespace();
    tokens.next().is_some_and(is_genus_token)
        && tokens.next_back().is_some_and(is_gtdb_placeholder_epithet)
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

    /// GET `url` and decode it as `T`. A 404 is reported as `Ok(None)` — ENA uses it for "nothing
    /// matched" — while other non-2xx statuses and transport/decoding failures become
    /// [`Error::Network`].
    fn fetch<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<Option<T>> {
        match self.agent.get(url).call() {
            Ok(resp) => resp.into_json::<T>().map(Some).map_err(|e| Error::Network {
                url: url.to_string(),
                message: format!("could not decode taxonomy response: {e}"),
            }),
            // ENA answers with 404 and a plain-text body when nothing matches.
            Err(ureq::Error::Status(404, _)) => Ok(None),
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
    #[serde(rename = "scientificName", default)]
    scientific_name: Option<String>,
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
        // The by-name endpoint answers with an array: a name can match several taxa.
        let entries = self.fetch::<Vec<TaxonEntry>>(&url)?.unwrap_or_default();
        Ok(classify(scientific_name, entries))
    }

    fn resolve_id(&self, tax_id: &str) -> Result<Resolution> {
        let url = format!("{}/tax-id/{}", self.base, encode_path_segment(tax_id));
        // The by-id endpoint answers with a single object: an id matches one taxon or none.
        let entry = self.fetch::<TaxonEntry>(&url)?;
        Ok(classify_id(tax_id, entry))
    }
}

/// Turn a by-name lookup result into a [`Resolution`]: exactly one submittable match is `Found`;
/// zero matches, multiple matches, and non-submittable matches are reportable problems.
fn classify(name: &str, mut entries: Vec<TaxonEntry>) -> Resolution {
    match entries.len() {
        0 => Resolution::Problem(format!("no ENA taxon matches scientific_name '{name}'")),
        1 => {
            let taxon = entries.remove(0);
            if is_submittable(&taxon) {
                Resolution::Found {
                    // Prefer ENA's own spelling, falling back to the name we asked for.
                    name: taxon.scientific_name.unwrap_or_else(|| name.to_string()),
                    tax_id: taxon.tax_id,
                }
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

/// Turn a by-id lookup result into a [`Resolution`]. Unlike a name, an id cannot be ambiguous; it
/// can still be unknown to ENA or not submittable, and it must carry a name to write into the row.
fn classify_id(tax_id: &str, entry: Option<TaxonEntry>) -> Resolution {
    let Some(taxon) = entry else {
        return Resolution::Problem(format!("is unknown to ENA (taxId {tax_id})"));
    };
    if !is_submittable(&taxon) {
        return Resolution::Problem(format!("is not submittable to ENA (taxId {tax_id})"));
    }
    match taxon.scientific_name {
        Some(name) => Resolution::Found {
            tax_id: taxon.tax_id,
            name,
        },
        None => Resolution::Problem(format!("has no scientific name in ENA (taxId {tax_id})")),
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

    /// Resolver driven by fixed name -> taxId and taxId -> name maps; anything unmapped reports a
    /// problem. Records every lookup, in order, so tests can assert which were actually issued.
    struct FakeResolver {
        by_name: HashMap<&'static str, &'static str>,
        by_id: HashMap<&'static str, &'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeResolver {
        /// The lookups issued so far, in call order. Ids are prefixed to tell them from names.
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl TaxonomyResolver for FakeResolver {
        fn resolve(&self, name: &str) -> Result<Resolution> {
            self.calls.borrow_mut().push(name.to_string());
            Ok(match self.by_name.get(name) {
                Some(id) => Resolution::Found {
                    tax_id: (*id).to_string(),
                    name: name.to_string(),
                },
                None => {
                    Resolution::Problem(format!("no ENA taxon matches scientific_name '{name}'"))
                }
            })
        }

        fn resolve_id(&self, tax_id: &str) -> Result<Resolution> {
            self.calls.borrow_mut().push(format!("id:{tax_id}"));
            Ok(match self.by_id.get(tax_id) {
                Some(name) => Resolution::Found {
                    tax_id: tax_id.to_string(),
                    name: (*name).to_string(),
                },
                None => Resolution::Problem(format!("is unknown to ENA (taxId {tax_id})")),
            })
        }
    }

    /// Taxon source driven by a fixed accession -> species taxId map, recording what it was asked.
    struct FakeTaxa {
        found: HashMap<&'static str, &'static str>,
        asked: RefCell<Vec<String>>,
    }

    impl TaxonSource for FakeTaxa {
        fn species_taxids(&self, accessions: &[String]) -> Result<HashMap<String, String>> {
            *self.asked.borrow_mut() = accessions.to_vec();
            Ok(accessions
                .iter()
                .filter_map(|a| {
                    self.found
                        .get(a.as_str())
                        .map(|t| (a.clone(), (*t).to_string()))
                })
                .collect())
        }
    }

    fn taxa(pairs: &[(&'static str, &'static str)]) -> FakeTaxa {
        FakeTaxa {
            found: pairs.iter().copied().collect(),
            asked: RefCell::new(Vec::new()),
        }
    }

    /// A resolver knowing `names` by name and `ids` by taxon id.
    fn resolver(
        names: &[(&'static str, &'static str)],
        ids: &[(&'static str, &'static str)],
    ) -> FakeResolver {
        FakeResolver {
            by_name: names.iter().copied().collect(),
            by_id: ids.iter().copied().collect(),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn table(text: &str) -> Table {
        Table::parse(PathBuf::from("mag_samples.tsv"), text).unwrap()
    }

    /// A GTDB-Tk classification cell naming `genus` and `species` (either may be empty).
    fn cell(genus: &str, species: &str) -> String {
        format!("d__Bacteria;p__Somephylum;o__Someorder;f__Somefamily;g__{genus};s__{species}")
    }

    /// A one-row sheet: `tax_id`, `scientific_name`, `GTDBtk fastani Ref`.
    fn sheet(rows: &[(&str, &str, &str)]) -> Table {
        let mut text = format!("tax_id\tscientific_name\t{REFERENCE_COLUMN}\n");
        for (tax, sci, acc) in rows {
            text.push_str(&format!("{tax}\t{sci}\t{acc}\n"));
        }
        table(&text)
    }

    #[test]
    fn the_reference_accession_resolves_the_row_and_ena_names_it() {
        let t = sheet(&[("", &cell("Phocaeicola", "Phocaeicola vulgatus"), "GCF_1.1")]);
        let r = resolver(&[], &[("821", "Phocaeicola vulgatus")]);
        let filled = fill_taxids(&t, &r, &taxa(&[("GCF_1.1", "821")])).unwrap();

        assert_eq!(
            filled.rows[0],
            ["821", "Phocaeicola vulgatus", "GCF_1.1"],
            "both cells are written from ENA's answer"
        );
        // Only the by-id lookup was issued: the name in the sheet is never matched against ENA.
        assert_eq!(r.calls(), ["id:821"]);
    }

    #[test]
    fn ena_names_the_taxon_even_when_gtdb_calls_it_something_else() {
        // The case that motivated this design: GTDB says `Prevotella copri`, ENA says `Segatella`.
        let t = sheet(&[("", &cell("Prevotella", "Prevotella copri"), "GCF_2.1")]);
        let r = resolver(&[], &[("165179", "Segatella copri")]);
        let filled = fill_taxids(&t, &r, &taxa(&[("GCF_2.1", "165179")])).unwrap();
        assert_eq!(filled.rows[0], ["165179", "Segatella copri", "GCF_2.1"]);
    }

    #[test]
    fn other_columns_are_preserved_in_order() {
        let t = table(&format!(
            "sample_alias\ttax_id\tscientific_name\t{REFERENCE_COLUMN}\tbiome\n\
             bin.1\t\t{}\tGCF_1.1\thuman gut\n",
            cell("Homo", "Homo sapiens")
        ));
        let r = resolver(&[], &[("9606", "Homo sapiens")]);
        let filled = fill_taxids(&t, &r, &taxa(&[("GCF_1.1", "9606")])).unwrap();

        assert_eq!(
            filled.headers,
            [
                "sample_alias",
                "tax_id",
                "scientific_name",
                REFERENCE_COLUMN,
                "biome"
            ]
        );
        assert_eq!(
            filled.rows[0],
            ["bin.1", "9606", "Homo sapiens", "GCF_1.1", "human gut"]
        );
    }

    #[test]
    fn keeps_already_filled_tax_id_and_issues_no_lookups() {
        // The mapped taxId differs from the pre-filled one; the pre-filled value must win, and the
        // cells beside it are left exactly as the user wrote them.
        let c = cell("Homo", "Homo sapiens");
        let t = sheet(&[("42", &c, "GCF_1.1")]);
        let r = resolver(&[], &[("9606", "Homo sapiens")]);
        let source = taxa(&[("GCF_1.1", "9606")]);
        let filled = fill_taxids(&t, &r, &source).unwrap();

        assert_eq!(filled.rows[0], ["42", &c, "GCF_1.1"]);
        assert!(r.calls().is_empty(), "no ENA lookup for a row already done");
        assert!(
            source.asked.borrow().is_empty(),
            "a filled row's accession is not even collected"
        );
    }

    #[test]
    fn accessions_are_collected_once_deduplicated_and_sorted() {
        let c = cell("Phocaeicola", "Phocaeicola vulgatus");
        let t = sheet(&[
            ("", &c, "GCF_2.1"),
            ("", &c, "GCF_1.1"),
            ("", &c, "GCF_2.1"),
        ]);
        let source = taxa(&[("GCF_1.1", "821"), ("GCF_2.1", "821")]);
        let r = resolver(&[], &[("821", "Phocaeicola vulgatus")]);
        fill_taxids(&t, &r, &source).unwrap();

        assert_eq!(*source.asked.borrow(), ["GCF_1.1", "GCF_2.1"]);
        // Three rows, two accessions, one taxon: the memo keeps ENA to a single lookup.
        let memo = Memoized::new(resolver(&[], &[("821", "Phocaeicola vulgatus")]));
        fill_taxids(&t, &memo, &source).unwrap();
        assert_eq!(memo.inner.calls(), ["id:821"]);
    }

    #[test]
    fn a_row_gtdb_tk_matched_no_reference_for_falls_back_to_the_lineage() {
        // GTDB-Tk writes `0` when it assigned no species; those rows have no accession to resolve.
        let t = sheet(&[("", &cell("Rothia", ""), "0")]);
        let r = resolver(&[("uncultured Rothia sp.", "316088")], &[]);
        let filled = fill_taxids(&t, &r, &taxa(&[])).unwrap();

        assert_eq!(filled.rows[0], ["316088", "uncultured Rothia sp.", "0"]);
        // `{genus} sp.` is tried first; `uncultured {genus} sp.` is the second shape ENA uses.
        assert_eq!(r.calls(), ["Rothia sp.", "uncultured Rothia sp."]);
    }

    #[test]
    fn an_accession_ncbi_does_not_know_falls_back_rather_than_failing() {
        // Suppressed and replaced assemblies resolve to nothing; the row is not lost.
        let t = sheet(&[(
            "",
            &cell("Phocaeicola", "Phocaeicola vulgatus"),
            "GCA_gone.1",
        )]);
        let r = resolver(&[("Phocaeicola vulgatus", "821")], &[]);
        let filled = fill_taxids(&t, &r, &taxa(&[])).unwrap();
        assert_eq!(
            filled.rows[0],
            ["821", "Phocaeicola vulgatus", "GCA_gone.1"]
        );
    }

    #[test]
    fn a_blank_reference_cell_falls_back_too() {
        let t = sheet(&[("", &cell("Phocaeicola", "Phocaeicola vulgatus"), "  ")]);
        let r = resolver(&[("Phocaeicola vulgatus", "821")], &[]);
        let filled = fill_taxids(&t, &r, &taxa(&[])).unwrap();
        assert_eq!(filled.rows[0][0], "821");
    }

    #[test]
    fn the_fallback_chain_climbs_species_then_genus_then_family() {
        // Only the family-level name is known, so every earlier candidate must be tried and fail.
        let t = sheet(&[("", &cell("Scybalocola", "Scybalocola sp900001"), "0")]);
        let r = resolver(&[("Somefamily bacterium", "2485925")], &[]);
        let filled = fill_taxids(&t, &r, &taxa(&[])).unwrap();

        assert_eq!(filled.rows[0][1], "Somefamily bacterium");
        // The GTDB placeholder species is skipped: `sp<digits>` matches nothing in ENA.
        assert_eq!(
            r.calls(),
            ["Scybalocola sp.", "Somefamily bacterium"],
            "placeholder species skipped, then genus, then family"
        );
    }

    #[test]
    fn the_fallback_chain_stops_at_the_first_hit() {
        let t = sheet(&[("", &cell("Phocaeicola", "Phocaeicola vulgatus"), "0")]);
        let r = resolver(
            &[
                ("Phocaeicola vulgatus", "821"),
                ("Phocaeicola sp.", "310298"),
            ],
            &[],
        );
        let filled = fill_taxids(&t, &r, &taxa(&[])).unwrap();
        assert_eq!(filled.rows[0][0], "821");
        assert_eq!(r.calls(), ["Phocaeicola vulgatus"], "no candidate wasted");
    }

    #[test]
    fn a_lineage_with_no_genus_or_species_still_climbs_from_the_family() {
        let t = sheet(&[("", "d__Bacteria;f__Eggerthellaceae;g__;s__", "0")]);
        let r = resolver(&[("Eggerthellaceae bacterium", "1972561")], &[]);
        let filled = fill_taxids(&t, &r, &taxa(&[])).unwrap();
        assert_eq!(filled.rows[0][1], "Eggerthellaceae bacterium");
    }

    #[test]
    fn a_lineage_with_nothing_below_the_domain_is_reported_with_its_deepest_rank() {
        let t = sheet(&[("", "d__Bacteria;f__;g__;s__", "0")]);
        let err = fill_taxids(&t, &resolver(&[], &[]), &taxa(&[]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("names no usable rank"), "got: {err}");
        assert!(err.contains("d__Bacteria"), "got: {err}");
    }

    #[test]
    fn an_exhausted_fallback_chain_reports_every_candidate_tried() {
        let t = sheet(&[("", &cell("Notagenus", "Notagenus notaspecies"), "0")]);
        let err = fill_taxids(&t, &resolver(&[], &[]), &taxa(&[]))
            .unwrap_err()
            .to_string();
        for candidate in [
            "Notagenus notaspecies",
            "Notagenus sp.",
            "Somefamily bacterium",
            "Someorder bacterium",
            "Somephylum bacterium",
        ] {
            assert!(err.contains(candidate), "missing {candidate} in: {err}");
        }
    }

    #[test]
    fn a_taxon_ena_rejects_is_reported_against_the_row() {
        let t = sheet(&[("", &cell("Homo", "Homo sapiens"), "GCF_1.1")]);
        let err = fill_taxids(&t, &resolver(&[], &[]), &taxa(&[("GCF_1.1", "9606")]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("row 1"), "got: {err}");
        assert!(err.contains("reference genome resolves to"), "got: {err}");
        assert!(err.contains("9606"), "got: {err}");
    }

    #[test]
    fn aggregates_row_problems_with_row_numbers() {
        let good = cell("Homo", "Homo sapiens");
        let t = table(&format!(
            "alias\ttax_id\tscientific_name\t{REFERENCE_COLUMN}\n\
             bin.1\t\t{good}\tGCF_1.1\n\
             bin.2\t\t{}\t0\n\
             bin.3\t\tNot A Classification\t0\n\
             bin.4\t\t\t0\n",
            cell("Notagenus", "Notagenus notaspecies"),
        ));
        let r = resolver(&[], &[("9606", "Homo sapiens")]);
        let err = fill_taxids(&t, &r, &taxa(&[("GCF_1.1", "9606")]))
            .unwrap_err()
            .to_string();

        assert!(err.contains("row 2"), "got: {err}");
        assert!(err.contains("row 3"), "got: {err}");
        assert!(err.contains("not a GTDB classification"), "got: {err}");
        assert!(err.contains("row 4"), "got: {err}");
        assert!(err.contains("scientific_name is empty"), "got: {err}");
        // Row 1 resolved fine, so it must not appear.
        assert!(!err.contains("row 1"), "got: {err}");
    }

    #[test]
    fn a_cached_problem_is_reported_for_every_affected_row() {
        // Caching must not swallow rows: the second row's problem is still aggregated.
        let c = cell("Notagenus", "Notagenus notaspecies");
        let t = sheet(&[("", &c, "GCF_1.1"), ("", &c, "GCF_1.1")]);
        let r = Memoized::new(resolver(&[], &[]));
        let err = fill_taxids(&t, &r, &taxa(&[("GCF_1.1", "404")]))
            .unwrap_err()
            .to_string();

        assert_eq!(r.inner.calls(), ["id:404"], "the failure is looked up once");
        assert!(err.contains("row 1"), "got: {err}");
        assert!(err.contains("row 2"), "got: {err}");
    }

    #[test]
    fn a_name_and_a_taxon_id_cannot_collide_in_the_memo_cache() {
        // Both lookups use the same cache; a name that happens to read like an id must not take
        // the id's cached answer, or vice versa.
        let r = Memoized::new(resolver(
            &[("821", "1")],
            &[("821", "Phocaeicola vulgatus")],
        ));
        let by_name = r.resolve("821").unwrap();
        let by_id = r.resolve_id("821").unwrap();

        match (by_name, by_id) {
            (Resolution::Found { tax_id: a, .. }, Resolution::Found { name: b, .. }) => {
                assert_eq!(a, "1");
                assert_eq!(b, "Phocaeicola vulgatus");
            }
            _ => panic!("expected both lookups to resolve independently"),
        }
        assert_eq!(r.inner.calls(), ["821", "id:821"]);
    }

    #[test]
    fn fallback_chain_shapes() {
        let chain = |cell: &str| fallback_chain(&gtdb::parse(cell).unwrap());

        assert_eq!(
            chain("f__Bacteroidaceae;g__Phocaeicola;s__Phocaeicola vulgatus"),
            [
                "Phocaeicola vulgatus",
                "Phocaeicola sp.",
                "Bacteroidaceae bacterium"
            ]
        );
        // A GTDB placeholder species is never tried as written.
        assert_eq!(
            chain("f__Bacteroidaceae;g__Phocaeicola;s__Phocaeicola sp900556845"),
            ["Phocaeicola sp.", "Bacteroidaceae bacterium"]
        );
        // Genus only: both `sp.` shapes, then the climb.
        assert_eq!(
            chain("o__Micrococcales;f__Micrococcaceae;g__Rothia;s__"),
            [
                "Rothia sp.",
                "uncultured Rothia sp.",
                "Micrococcaceae bacterium",
                "Micrococcales bacterium",
            ]
        );
        // Neither: climb from the family, deepest rank first.
        assert_eq!(
            chain("p__Actinomycetota;c__Coriobacteriia;f__Eggerthellaceae;g__;s__"),
            [
                "Eggerthellaceae bacterium",
                "Coriobacteriia bacterium",
                "Actinomycetota bacterium",
            ]
        );
        // The domain is never tried: "Bacteria bacterium" names nothing.
        assert!(chain("d__Bacteria;g__;s__").is_empty());
    }

    #[test]
    fn stale_accessions_are_counted_per_accession_for_reporting() {
        // Rows 1-2 share one suppressed assembly, row 3 has another, row 4 resolves fine, and
        // row 5 was never matched to a reference at all.
        let c = cell("Phocaeicola", "Phocaeicola vulgatus");
        let t = sheet(&[
            ("", &c, "GCA_gone.1"),
            ("", &c, "GCA_gone.1"),
            ("", &c, "GCA_also_gone.1"),
            ("", &c, "GCF_here.1"),
            ("", &c, "0"),
        ]);
        let species: HashMap<String, String> =
            [("GCF_here.1".to_string(), "821".to_string())].into();
        let pending: Vec<&Vec<String>> = t.rows.iter().collect();

        let missing = unresolved_accessions(&pending, 2, &species);
        assert_eq!(
            missing,
            [("GCA_also_gone.1", 1), ("GCA_gone.1", 2)].into(),
            "only accessions NCBI could not resolve, counted by row"
        );
    }

    #[test]
    fn rows_that_already_have_a_tax_id_are_not_warned_about() {
        // `pending` excludes them, so a stale accession beside a filled tax_id is nobody's problem.
        let c = cell("Phocaeicola", "Phocaeicola vulgatus");
        let t = sheet(&[("821", &c, "GCA_gone.1"), ("", &c, "GCA_gone.1")]);
        let pending: Vec<&Vec<String>> = t
            .rows
            .iter()
            .filter(|row| row[0].trim().is_empty())
            .collect();

        let missing = unresolved_accessions(&pending, 2, &HashMap::new());
        assert_eq!(missing, [("GCA_gone.1", 1)].into());
    }

    #[test]
    fn reference_accession_recognises_the_placeholders_gtdb_tk_writes() {
        assert_eq!(
            reference_accession("GCF_000012825.1"),
            Some("GCF_000012825.1")
        );
        assert_eq!(reference_accession(" GCA_1.1 "), Some("GCA_1.1"));
        for placeholder in ["0", "", "   ", "N/A", "n/a", "NA"] {
            assert_eq!(
                reference_accession(placeholder),
                None,
                "for {placeholder:?}"
            );
        }
    }

    #[test]
    fn missing_columns_are_errors() {
        for (text, missing) in [
            (
                format!("sample_alias\ttax_id\t{REFERENCE_COLUMN}\nbin.1\t\t0\n"),
                "scientific_name",
            ),
            (
                format!("scientific_name\t{REFERENCE_COLUMN}\nd__Bacteria\t0\n"),
                "tax_id",
            ),
            (
                "tax_id\tscientific_name\n\td__Bacteria\n".to_string(),
                REFERENCE_COLUMN,
            ),
        ] {
            let err = fill_taxids(&table(&text), &resolver(&[], &[]), &taxa(&[]))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!("missing required column: {missing}")),
                "got: {err}"
            );
        }
    }

    fn entry(tax_id: &str, name: Option<&str>, submittable: Option<&str>) -> TaxonEntry {
        TaxonEntry {
            tax_id: tax_id.to_string(),
            scientific_name: name.map(str::to_string),
            submittable: submittable.map(str::to_string),
        }
    }

    #[test]
    fn classify_single_submittable_is_found() {
        match classify("x", vec![entry("9606", Some("Homo sapiens"), Some("true"))]) {
            Resolution::Found { tax_id, name } => {
                assert_eq!(tax_id, "9606");
                assert_eq!(name, "Homo sapiens", "ENA's spelling wins over the query");
            }
            Resolution::Problem(p) => panic!("expected Found, got: {p}"),
        }
    }

    #[test]
    fn classify_falls_back_to_the_queried_name_when_ena_omits_one() {
        match classify("x", vec![entry("9606", None, Some("true"))]) {
            Resolution::Found { name, .. } => assert_eq!(name, "x"),
            Resolution::Problem(p) => panic!("expected Found, got: {p}"),
        }
    }

    #[test]
    fn classify_non_submittable_is_problem() {
        let p = match classify("x", vec![entry("1", Some("Bacteria"), Some("false"))]) {
            Resolution::Problem(p) => p,
            Resolution::Found { .. } => panic!("expected Problem"),
        };
        assert!(p.contains("not submittable"), "got: {p}");
    }

    #[test]
    fn classify_zero_matches_is_problem() {
        let p = match classify("x", vec![]) {
            Resolution::Problem(p) => p,
            Resolution::Found { .. } => panic!("expected Problem"),
        };
        assert!(p.contains("no ENA taxon"), "got: {p}");
    }

    #[test]
    fn classify_multiple_matches_is_ambiguous() {
        let p = match classify(
            "x",
            vec![
                entry("1", Some("A"), Some("true")),
                entry("2", Some("B"), Some("true")),
            ],
        ) {
            Resolution::Problem(p) => p,
            Resolution::Found { .. } => panic!("expected Problem"),
        };
        assert!(p.contains("ambiguous"), "got: {p}");
        assert!(p.contains('1') && p.contains('2'), "got: {p}");
    }

    #[test]
    fn classify_id_takes_enas_name_and_checks_submittability() {
        match classify_id(
            "821",
            Some(entry("821", Some("Phocaeicola vulgatus"), Some("true"))),
        ) {
            Resolution::Found { tax_id, name } => {
                assert_eq!(tax_id, "821");
                assert_eq!(name, "Phocaeicola vulgatus");
            }
            Resolution::Problem(p) => panic!("expected Found, got: {p}"),
        }
    }

    #[test]
    fn classify_id_rejects_unknown_unsubmittable_and_unnamed_taxa() {
        let cases = [
            (None, "unknown to ENA"),
            (
                Some(entry("1", Some("Bacteria"), Some("false"))),
                "not submittable",
            ),
            (Some(entry("1", None, Some("true"))), "no scientific name"),
        ];
        for (entry, expected) in cases {
            match classify_id("1", entry) {
                Resolution::Problem(p) => assert!(p.contains(expected), "got: {p}"),
                Resolution::Found { .. } => panic!("expected Problem for {expected}"),
            }
        }
    }

    #[test]
    fn genus_tokens_and_placeholder_epithets_are_recognised() {
        for name in [
            "Phocaeicola sp900556845",
            "Clostridium AQ sp000165065",
            "Phocaeicola sp1",
        ] {
            assert!(is_gtdb_placeholder(name), "expected placeholder: {name}");
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
        ] {
            assert!(!is_gtdb_placeholder(name), "not a placeholder: {name}");
        }
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
        let t = sheet(&[("9606", "Homo sapiens", "GCF_1.1")]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tsv");
        write_table(&path, &t).unwrap();

        let back = Table::read(&path).unwrap();
        assert_eq!(back.headers, t.headers);
        assert_eq!(back.rows, t.rows);
    }

    #[test]
    fn fill_then_write_produces_exact_tsv() {
        let t = sheet(&[("", &cell("Homo", "Homo sapiens"), "GCF_1.1")]);
        let r = resolver(&[], &[("9606", "Homo sapiens")]);
        let filled = fill_taxids(&t, &r, &taxa(&[("GCF_1.1", "9606")])).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("filled.tsv");
        write_table(&path, &filled).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content,
            format!("tax_id\tscientific_name\t{REFERENCE_COLUMN}\n9606\tHomo sapiens\tGCF_1.1\n")
        );
    }
}
