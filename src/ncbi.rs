//! Mapping GTDB-Tk reference genome accessions to NCBI **species** taxon ids.
//!
//! GTDB-Tk records, for every bin, the accession of the reference genome it matched
//! (`GTDBtk fastani Ref`, e.g. `GCA_900553985.1`). That accession is the reliable key into NCBI
//! taxonomy: matching GTDB's *names* against ENA fails for the ~19% of a real sheet whose names
//! exist only in GTDB (`CAG-269`, `UBA9414`) or that ENA has not adopted (`Ruminococcus gnavus`,
//! now *Mediterraneibacter*). See ADR 0008.
//!
//! Two things this module must get right:
//!
//! - **Batching.** Both endpoints take many keys per request, so a whole sheet costs a handful of
//!   POSTs rather than one per row. This is also what keeps us well inside NCBI's unauthenticated
//!   rate limit.
//! - **Climbing to species rank.** A genome's own taxon is often a *strain* — RefSeq isolate
//!   genomes (`GCF_`) are over half of a typical sheet, and 54 of one sheet's 172 taxa were
//!   `STRAIN` or `SUBSPECIES`. A MAG is not the type strain it happens to resemble, so the strain
//!   taxon is wrong to submit under; we walk its lineage up to the `SPECIES` rank.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Base URL of the NCBI Datasets v2 REST service.
const NCBI_DATASETS_BASE: &str = "https://api.ncbi.nlm.nih.gov/datasets/v2alpha";

/// Give up on a connection that will not open, rather than hanging the whole command.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on waiting for one response. Generous: these are batched requests over many accessions.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Accessions per genome-report request. NCBI's own page size cap.
const GENOME_CHUNK: usize = 100;

/// Taxon ids per taxonomy request.
const TAXON_CHUNK: usize = 200;

/// The rank we resolve every genome to. NCBI spells its ranks in upper case.
const SPECIES: &str = "SPECIES";

/// Resolves reference genome accessions to NCBI species taxon ids.
///
/// Abstracted behind a trait so the fill logic can be exercised offline; the only production
/// implementation is [`NcbiDatasets`].
pub trait TaxonSource {
    /// Map each accession that NCBI knows to its species taxon id.
    ///
    /// Accessions NCBI does not know — suppressed or replaced assemblies — are simply absent from
    /// the returned map rather than an error, so the caller can fall back for those rows.
    fn species_taxids(&self, accessions: &[String]) -> Result<HashMap<String, String>>;
}

/// Live source backed by the NCBI Datasets v2 REST service.
pub struct NcbiDatasets {
    base: String,
    /// Pooled connections, for the same reason the ENA client pools them: several requests
    /// back-to-back against one host should not each pay a TCP + TLS handshake.
    agent: ureq::Agent,
}

impl NcbiDatasets {
    pub fn new() -> Self {
        Self {
            base: NCBI_DATASETS_BASE.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout_read(READ_TIMEOUT)
                .build(),
        }
    }

    /// POST `body` to `path` and decode the response as `T`.
    ///
    /// A 429 is retried after a short pause: NCBI rate-limits unauthenticated callers, and a whole
    /// sheet is only a handful of requests, so one backoff is cheaper than failing the run.
    fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base);
        let mut attempt = 0;
        loop {
            let network = |message: String| Error::Network {
                url: url.clone(),
                message,
            };
            match self.agent.post(&url).send_json(body.clone()) {
                Ok(resp) => {
                    return resp
                        .into_json::<T>()
                        .map_err(|e| network(format!("could not decode NCBI response: {e}")));
                }
                Err(ureq::Error::Status(429, _)) if attempt < 3 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_secs(attempt));
                }
                Err(ureq::Error::Status(code, _)) => return Err(network(format!("HTTP {code}"))),
                Err(e) => return Err(network(e.to_string())),
            }
        }
    }

    /// Look up the rank and lineage of each taxon id.
    fn taxonomy(&self, taxons: &[String]) -> Result<HashMap<String, TaxonomyNode>> {
        let mut out = HashMap::new();
        for chunk in taxons.chunks(TAXON_CHUNK) {
            let body = serde_json::json!({ "taxons": chunk });
            let page: TaxonomyResponse = self.post("/taxonomy", &body)?;
            for node in page.taxonomy_nodes {
                out.insert(node.taxonomy.tax_id.to_string(), node.taxonomy);
            }
        }
        Ok(out)
    }
}

impl TaxonSource for NcbiDatasets {
    fn species_taxids(&self, accessions: &[String]) -> Result<HashMap<String, String>> {
        // 1. Accession -> the taxon of the genome itself, whatever its rank.
        let mut genome_taxids = HashMap::new();
        for chunk in accessions.chunks(GENOME_CHUNK) {
            let body = serde_json::json!({
                "accessions": chunk,
                "page_size": GENOME_CHUNK,
            });
            let page: GenomeReportResponse = self.post("/genome/dataset_report", &body)?;
            for report in page.reports {
                if let Some(tax_id) = report.organism.and_then(|o| o.tax_id) {
                    genome_taxids.insert(report.accession, tax_id.to_string());
                }
            }
        }

        // 2. Those taxa's ranks and lineages.
        let mut ids: Vec<String> = genome_taxids.values().cloned().collect();
        ids.sort_unstable();
        ids.dedup();
        let nodes = self.taxonomy(&ids)?;

        // 3. Ranks of every ancestor of the taxa that are not already species, so each can be
        //    climbed. One extra request for the whole sheet, and none at all when every genome
        //    already sits at species rank.
        let mut ancestors: Vec<String> = nodes
            .values()
            .filter(|n| !n.is_species())
            .flat_map(|n| n.lineage.iter().map(u64::to_string))
            .collect();
        ancestors.sort_unstable();
        ancestors.dedup();
        let ancestor_nodes = if ancestors.is_empty() {
            HashMap::new()
        } else {
            self.taxonomy(&ancestors)?
        };

        Ok(resolve_species(&genome_taxids, &nodes, &ancestor_nodes))
    }
}

impl Default for NcbiDatasets {
    fn default() -> Self {
        Self::new()
    }
}

/// Reduce each accession to a species taxon id, dropping the ones that cannot be climbed.
///
/// Split out from the request plumbing so the climb is testable without a network.
fn resolve_species(
    genome_taxids: &HashMap<String, String>,
    nodes: &HashMap<String, TaxonomyNode>,
    ancestors: &HashMap<String, TaxonomyNode>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (accession, tax_id) in genome_taxids {
        let Some(node) = nodes.get(tax_id) else {
            continue;
        };
        let species = if node.is_species() {
            Some(tax_id.clone())
        } else {
            // The lineage runs root-first, so the nearest species ancestor is the last one.
            node.lineage
                .iter()
                .rev()
                .map(u64::to_string)
                .find(|id| ancestors.get(id).is_some_and(TaxonomyNode::is_species))
        };
        if let Some(species) = species {
            out.insert(accession.clone(), species);
        }
    }
    out
}

/// `POST /genome/dataset_report` — the fields we consume; all others are ignored.
#[derive(Debug, Deserialize)]
struct GenomeReportResponse {
    #[serde(default)]
    reports: Vec<GenomeReport>,
}

#[derive(Debug, Deserialize)]
struct GenomeReport {
    accession: String,
    #[serde(default)]
    organism: Option<Organism>,
}

#[derive(Debug, Deserialize)]
struct Organism {
    #[serde(default)]
    tax_id: Option<u64>,
}

/// `POST /taxonomy` — the fields we consume; all others are ignored.
#[derive(Debug, Deserialize)]
struct TaxonomyResponse {
    #[serde(default)]
    taxonomy_nodes: Vec<TaxonomyNodeWrapper>,
}

#[derive(Debug, Deserialize)]
struct TaxonomyNodeWrapper {
    taxonomy: TaxonomyNode,
}

#[derive(Debug, Deserialize)]
struct TaxonomyNode {
    tax_id: u64,
    #[serde(default)]
    rank: Option<String>,
    /// Ancestor taxon ids, root first, excluding the node itself.
    #[serde(default)]
    lineage: Vec<u64>,
}

impl TaxonomyNode {
    fn is_species(&self) -> bool {
        self.rank.as_deref() == Some(SPECIES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tax_id: u64, rank: &str, lineage: &[u64]) -> TaxonomyNode {
        TaxonomyNode {
            tax_id,
            rank: Some(rank.to_string()),
            lineage: lineage.to_vec(),
        }
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn nodes(list: Vec<TaxonomyNode>) -> HashMap<String, TaxonomyNode> {
        list.into_iter()
            .map(|n| (n.tax_id.to_string(), n))
            .collect()
    }

    #[test]
    fn a_species_rank_genome_is_used_as_is() {
        // `GCA_900553985.1` is a MAG-derived assembly: its own taxon is already a species.
        let got = resolve_species(
            &map(&[("GCA_900553985.1", "59620")]),
            &nodes(vec![node(59620, "SPECIES", &[1485, 59619])]),
            &HashMap::new(),
        );
        assert_eq!(got, map(&[("GCA_900553985.1", "59620")]));
    }

    #[test]
    fn a_strain_genome_climbs_to_its_species() {
        // `GCF_000012825.1` is *Phocaeicola vulgatus* ATCC 8482 (taxon 435590); a MAG must be
        // submitted as the species (821), not as the type strain it resembles.
        let got = resolve_species(
            &map(&[("GCF_000012825.1", "435590")]),
            &nodes(vec![node(435590, "STRAIN", &[815, 909656, 821])]),
            &nodes(vec![
                node(815, "FAMILY", &[]),
                node(909656, "GENUS", &[]),
                node(821, "SPECIES", &[]),
            ]),
        );
        assert_eq!(got, map(&[("GCF_000012825.1", "821")]));
    }

    #[test]
    fn a_subspecies_genome_climbs_to_its_species() {
        let got = resolve_species(
            &map(&[("GCF_000000001.1", "100")]),
            &nodes(vec![node(100, "SUBSPECIES", &[10, 50])]),
            &nodes(vec![node(10, "GENUS", &[]), node(50, "SPECIES", &[])]),
        );
        assert_eq!(got, map(&[("GCF_000000001.1", "50")]));
    }

    #[test]
    fn the_climb_takes_the_nearest_species_ancestor() {
        // Two species ranks in one lineage: the deepest (last) one is the genome's own species.
        let got = resolve_species(
            &map(&[("GCF_000000002.1", "300")]),
            &nodes(vec![node(300, "STRAIN", &[7, 200, 250])]),
            &nodes(vec![
                node(7, "SPECIES", &[]),
                node(200, "GENUS", &[]),
                node(250, "SPECIES", &[]),
            ]),
        );
        assert_eq!(got, map(&[("GCF_000000002.1", "250")]));
    }

    #[test]
    fn a_taxon_with_no_species_ancestor_is_dropped_not_guessed() {
        let got = resolve_species(
            &map(&[("GCA_000000003.1", "400")]),
            &nodes(vec![node(400, "GENUS", &[2, 10])]),
            &nodes(vec![node(2, "SUPERKINGDOM", &[]), node(10, "FAMILY", &[])]),
        );
        assert!(got.is_empty(), "got: {got:?}");
    }

    #[test]
    fn an_accession_ncbi_does_not_know_is_absent_rather_than_an_error() {
        // Suppressed and replaced assemblies come back with no report at all; the caller falls
        // back to the lineage for those rows.
        let got = resolve_species(&map(&[]), &HashMap::new(), &HashMap::new());
        assert!(got.is_empty());
    }

    #[test]
    fn genome_reports_without_an_organism_are_skipped() {
        let parsed: GenomeReportResponse = serde_json::from_str(
            r#"{"reports":[
                 {"accession":"GCA_1.1","organism":{"tax_id":59620}},
                 {"accession":"GCA_2.1"},
                 {"accession":"GCA_3.1","organism":{}}
               ]}"#,
        )
        .unwrap();
        let ids: Vec<_> = parsed
            .reports
            .iter()
            .filter_map(|r| r.organism.as_ref().and_then(|o| o.tax_id))
            .collect();
        assert_eq!(ids, [59620]);
    }

    #[test]
    fn taxonomy_responses_decode_rank_and_lineage() {
        let parsed: TaxonomyResponse = serde_json::from_str(
            r#"{"taxonomy_nodes":[{"taxonomy":{
                 "tax_id":435590,"organism_name":"Phocaeicola vulgatus ATCC 8482",
                 "lineage":[1,131567,2,815,909656,821],"rank":"STRAIN"}}]}"#,
        )
        .unwrap();
        let node = &parsed.taxonomy_nodes[0].taxonomy;
        assert_eq!(node.tax_id, 435590);
        assert!(!node.is_species());
        assert_eq!(node.lineage.last(), Some(&821));
    }

    #[test]
    fn a_node_without_a_rank_is_not_treated_as_a_species() {
        let parsed: TaxonomyResponse =
            serde_json::from_str(r#"{"taxonomy_nodes":[{"taxonomy":{"tax_id":1}}]}"#).unwrap();
        assert!(!parsed.taxonomy_nodes[0].taxonomy.is_species());
    }

    #[test]
    fn chunk_sizes_match_what_the_endpoints_accept() {
        // A regression guard: NCBI caps the genome report page size at 100.
        let accessions: Vec<String> = (0..250).map(|i| format!("GCA_{i}.1")).collect();
        let chunks: Vec<usize> = accessions.chunks(GENOME_CHUNK).map(<[_]>::len).collect();
        assert_eq!(chunks, [100, 100, 50]);
    }
}
