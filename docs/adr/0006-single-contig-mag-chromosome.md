# 6. Single-contig MAG bins submitted as chromosomes

- Status: accepted
- Date: 2026-07-23
- Relates to [ADR 0004](0004-mag-sample-taxid-fill.md) (MAG submission flow) and
  [ADR 0002](0002-wrap-webin-cli-hybrid.md) (Webin-CLI as the submission engine).

## Context

The MAG submission path (ADR 0003/0004) submits each bin under the Webin-CLI `genome` context with a
single FASTA, i.e. as anonymous **contigs**. Long-read assemblies increasingly produce a bin that is
a **single, often circular, contig** — a *complete* genome rather than a set of fragments.

ENA represents a complete replicon with a **chromosome list file**: a gzipped, tab-separated file
that names each sequence as a chromosome/plasmid and records its topology (linear/circular). Submitted
alongside the FASTA (manifest key `CHROMOSOME_LIST`), it produces proper `/chromosome` qualifiers and
a `circular` topology in the resulting records. Without it, a closed genome is filed as an
undifferentiated contig, losing that structure.

## Decision

Add a **fallback** to `mag submit`: for each bin, scan its FASTA and count sequences.

- **Exactly one sequence** → submit it as a **chromosome**. Generate a gzipped chromosome list file
  mapping the contig's object name (the FASTA `>` header's first token) to a chromosome name, with a
  chromosome **type** (default `chromosome`) and **topology** (default `linear`; `circular` when the
  user marks the contig closed). Topology is written as a modifier on the type, e.g.
  `Circular-Chromosome`, per ENA's chromosome-list format. The genome manifest then carries both
  `FASTA` and `CHROMOSOME_LIST`.
- **Zero or multiple sequences** → unchanged; submit as contigs (FASTA only).

Chromosome `topology` and `chromosome_name` come from optional columns on the MAG-assembly TSV (the
chromosome type defaults to `Chromosome`, and `chromosome_name` defaults to the bin name). Detection
is **structural** (sequence count), read transparently whether the FASTA is plain or gzipped
(magic-byte sniffing via the `flate2` crate) — "long reads" is the motivating cause, not a condition
the tool checks.

The FASTA scan and chromosome-list rendering shipped first as a standalone, unit-tested `chromosome`
module; `mag submit` (milestone 7) now wires it in: for a single-contig bin it writes the gzipped
chromosome list beside the FASTA and adds `CHROMOSOME_LIST` to the genome manifest.

## Consequences

- New dependency: **`flate2`** (pure-Rust miniz backend) to read gzipped FASTA and write gzipped
  chromosome lists. ENA requires all data files gzipped, so the tool emits the list gzipped.
- The tool now **reads FASTA content**; previously it only passed file paths through to Webin-CLI.
  The scan is a cheap `>`-line count, not a full parse — enough to decide contig vs chromosome.
- Closed single-contig genomes get correct chromosome-level records, including circular topology.
- Scope limits: one bin = one FASTA. A bin whose closed genome plus plasmids arrive as several
  contigs is treated as multi-contig (contigs submission); per-replicon chromosome lists for such
  bins are out of scope for now. Non-`genome`/non-MAG paths are unaffected.
