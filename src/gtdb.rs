//! Parsing of GTDB-Tk classification strings.
//!
//! MAG sample sheets carry GTDB-Tk's `classification` string verbatim in `scientific_name`:
//! seven `rank__value` fields joined by `;`, from domain down to species. ENA knows nothing of
//! this format, so [`parse`] turns one into a [`Lineage`] whose ranks can be read individually.
//!
//! The taxon id itself comes from the row's reference genome accession (see [`crate::ncbi`]); the
//! lineage is what the ENA-name fallback chain is built from when a row has no usable accession,
//! which is why every rank — not just species and genus — is kept.
//!
//! Values are stripped of GTDB's polyphyly suffixes (`Clostridium_AQ` → `Clostridium`,
//! `Bacteroides fragilis_A` → `Bacteroides fragilis`). Those suffixes mark GTDB's splits of a
//! name NCBI keeps whole; they exist nowhere in ENA, so a name carrying one can never match.

/// A parsed GTDB-Tk classification: its populated ranks, deepest last, suffixes stripped.
///
/// Ranks that GTDB left empty (`s__` with no species assignment) are simply absent.
#[derive(Debug, PartialEq, Eq)]
pub struct Lineage {
    /// `(rank letter, value)` in lineage order, holding only the ranks that carry a value.
    ranks: Vec<(char, String)>,
}

impl Lineage {
    /// The value at `rank` (`'d'`, `'p'`, `'c'`, `'o'`, `'f'`, `'g'`, `'s'`), if GTDB assigned one.
    ///
    /// `'d'` also answers for a `k__` kingdom field, since the two are the same rank spelled
    /// differently by different pipelines.
    pub fn rank(&self, rank: char) -> Option<&str> {
        self.ranks
            .iter()
            .find(|(r, _)| *r == rank)
            .map(|(_, v)| v.as_str())
    }

    /// The deepest populated rank, as `(rank letter, value)`. `None` for a lineage empty
    /// throughout. Used to make a "names nothing usable" failure actionable.
    pub fn deepest(&self) -> Option<(char, &str)> {
        self.ranks.last().map(|(r, v)| (*r, v.as_str()))
    }
}

/// The rank prefixes GTDB-Tk emits, in lineage order. `k` covers kingdom-style output, which
/// some pipelines produce in place of `d`; it is normalised to `d` so callers ask for one rank.
const RANKS: [char; 8] = ['d', 'k', 'p', 'c', 'o', 'f', 'g', 's'];

/// Parse a GTDB-Tk classification string into its populated ranks.
///
/// `Err` carries a row-level reason to aggregate and report; it never aborts a run.
pub fn parse(cell: &str) -> Result<Lineage, String> {
    let mut ranks = Vec::new();

    for field in cell.split(';') {
        let field = field.trim();
        let (rank, value) = split_field(field)
            .ok_or_else(|| format!("not a GTDB classification: '{cell}' (at '{field}')"))?;
        if value.is_empty() {
            continue;
        }
        ranks.push((if rank == 'k' { 'd' } else { rank }, strip_suffixes(value)));
    }

    Ok(Lineage { ranks })
}

/// Split a `rank__value` field into its rank letter and value, or `None` if it is not one.
fn split_field(field: &str) -> Option<(char, &str)> {
    let (rank, value) = field.split_once("__")?;
    let mut chars = rank.chars();
    let rank = chars.next().filter(|c| RANKS.contains(c))?;
    chars.next().is_none().then_some((rank, value.trim()))
}

/// Strip GTDB's polyphyly suffix — `_` followed by uppercase ASCII letters — from every
/// whitespace-separated token of `value`.
///
/// The suffix can sit on either token of a binomial: GTDB writes both `Anaerobiospirillum_A
/// thomasii` (the genus was split) and `Bacteroides fragilis_A` (the species was).
fn strip_suffixes(value: &str) -> String {
    value
        .split_whitespace()
        .map(strip_suffix)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip one trailing `_[A-Z]+` from `token`, leaving anything else — including the accessioned
/// epithets (`sp900556845`) the taxonomy layer recognises — untouched.
fn strip_suffix(token: &str) -> &str {
    match token.rsplit_once('_') {
        Some((stem, suffix))
            if !stem.is_empty()
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_uppercase()) =>
        {
            stem
        }
        _ => token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full seven-rank lineage ending in `s__{species}`.
    fn lineage(genus: &str, species: &str) -> String {
        format!(
            "d__Bacteria;p__Bacteroidota;c__Bacteroidia;o__Bacteroidales;\
             f__Bacteroidaceae;g__{genus};s__{species}"
        )
    }

    #[test]
    fn every_rank_is_readable_by_letter() {
        let l = parse(&lineage("Phocaeicola", "Phocaeicola vulgatus")).unwrap();
        assert_eq!(l.rank('d'), Some("Bacteria"));
        assert_eq!(l.rank('p'), Some("Bacteroidota"));
        assert_eq!(l.rank('c'), Some("Bacteroidia"));
        assert_eq!(l.rank('o'), Some("Bacteroidales"));
        assert_eq!(l.rank('f'), Some("Bacteroidaceae"));
        assert_eq!(l.rank('g'), Some("Phocaeicola"));
        assert_eq!(l.rank('s'), Some("Phocaeicola vulgatus"));
        assert_eq!(l.deepest(), Some(('s', "Phocaeicola vulgatus")));
    }

    #[test]
    fn ranks_gtdb_left_empty_are_absent() {
        let l = parse(&lineage("Rothia", "")).unwrap();
        assert_eq!(l.rank('s'), None);
        assert_eq!(l.rank('g'), Some("Rothia"));
        // The deepest rank is what the row can still be reported against.
        assert_eq!(l.deepest(), Some(('g', "Rothia")));
    }

    #[test]
    fn a_kingdom_field_answers_as_the_domain_rank() {
        // Some pipelines emit `k__` where GTDB-Tk emits `d__`; callers should not have to care.
        let l = parse("k__Bacteria;g__Rothia").unwrap();
        assert_eq!(l.rank('d'), Some("Bacteria"));
    }

    #[test]
    fn accessioned_epithets_pass_through_untouched() {
        // `sp900556845` is a GTDB accession; recognising it is the taxonomy layer's job.
        let l = parse(&lineage("Phocaeicola", "Phocaeicola sp900556845")).unwrap();
        assert_eq!(l.rank('s'), Some("Phocaeicola sp900556845"));
    }

    #[test]
    fn polyphyly_suffixes_are_stripped_from_either_token() {
        let cases = [
            // The suffix sits on the genus, in both ranks.
            (
                lineage("Clostridium_AQ", "Clostridium_AQ sp000165065"),
                Some("Clostridium sp000165065"),
                Some("Clostridium"),
            ),
            (
                lineage("Anaerobiospirillum_A", "Anaerobiospirillum_A thomasii"),
                Some("Anaerobiospirillum thomasii"),
                Some("Anaerobiospirillum"),
            ),
            // The suffix sits on the epithet.
            (
                lineage("Bacteroides", "Bacteroides fragilis_A"),
                Some("Bacteroides fragilis"),
                Some("Bacteroides"),
            ),
            // A genus-only lineage strips too.
            (lineage("Clostridium_Q", ""), None, Some("Clostridium")),
        ];
        for (cell, species, genus) in cases {
            let l = parse(&cell).unwrap();
            assert_eq!(l.rank('s'), species, "species of {cell}");
            assert_eq!(l.rank('g'), genus, "genus of {cell}");
        }
    }

    #[test]
    fn suffixes_are_stripped_at_every_rank_not_just_the_last_two() {
        // GTDB splits high ranks too: `p__Bacillota_A`.
        let l = parse("d__Bacteria;p__Bacillota_A;c__Clostridia;f__Lachnospiraceae").unwrap();
        assert_eq!(l.rank('p'), Some("Bacillota"));
    }

    #[test]
    fn a_lineage_without_genus_or_species_keeps_its_deepest_rank() {
        // Not an error here: a row with a usable reference accession needs no rank at all, so
        // whether this lineage is usable is the caller's question, not the parser's.
        let cell = "d__Bacteria;p__Actinomycetota;c__Coriobacteriia;o__Coriobacteriales;\
                    f__Eggerthellaceae;g__;s__";
        let l = parse(cell).unwrap();
        assert_eq!(l.rank('g'), None);
        assert_eq!(l.deepest(), Some(('f', "Eggerthellaceae")));
    }

    #[test]
    fn an_entirely_empty_lineage_has_no_ranks() {
        let l = parse("d__;p__;c__;o__;f__;g__;s__").unwrap();
        assert_eq!(l.deepest(), None);
        assert_eq!(l.rank('s'), None);
    }

    #[test]
    fn non_classification_cells_are_rejected() {
        let cases = [
            // A bare scientific name: the shape the sheet used to carry.
            "Phocaeicola vulgatus",
            // Missing the `__` separator.
            "d__Bacteria;p_Bacteroidota",
            // A rank letter GTDB does not emit.
            "d__Bacteria;x__Something",
            // A multi-letter rank prefix.
            "d__Bacteria;phylum__Bacteroidota",
            "",
        ];
        for cell in cases {
            let err = parse(cell).unwrap_err();
            assert!(err.contains("not a GTDB classification"), "for '{cell}'");
        }
    }

    #[test]
    fn whitespace_around_fields_and_values_is_ignored() {
        let l = parse(" d__Bacteria ; g__Rothia ; s__ Rothia mucilaginosa ").unwrap();
        assert_eq!(l.rank('s'), Some("Rothia mucilaginosa"));
        assert_eq!(l.rank('g'), Some("Rothia"));
    }

    #[test]
    fn strip_suffix_leaves_non_polyphyly_underscores_alone() {
        // Only an all-uppercase suffix is GTDB's polyphyly marker.
        assert_eq!(strip_suffix("Clostridium_AQ"), "Clostridium");
        assert_eq!(strip_suffix("Clostridium_aq"), "Clostridium_aq");
        assert_eq!(strip_suffix("Clostridium_A1"), "Clostridium_A1");
        assert_eq!(strip_suffix("_A"), "_A");
        assert_eq!(strip_suffix("Clostridium"), "Clostridium");
    }
}
