# Tutorial — `ena-submit` command by command

A walk-through of every command, with the input files each one expects and the output each one
produces. Read it top to bottom the first time; after that, jump to the command you need.

Throughout, `$` marks a shell prompt and the indented blocks below a command are its actual output.

**Contents**

- [Before you start](#before-you-start)
- [`init` — scaffold a project](#init--scaffold-a-project)
- [Two flags every submission command shares](#two-flags-every-submission-command-shares)
- [`reads` — submit sequencing runs](#reads--submit-sequencing-runs)
- [`assembly` — submit genome assemblies](#assembly--submit-genome-assemblies)
- [`mag prepare` — fill the MAG sample sheet](#mag-prepare--fill-the-mag-sample-sheet)
- [`mag submit` — submit MAG assemblies](#mag-submit--submit-mag-assemblies)
- [`status` — read the submission history](#status--read-the-submission-history)
- [Troubleshooting](#troubleshooting)

---

## Before you start

**Build the binary.**

```sh
$ cargo build --release
# binary at target/release/ena-submit
```

Put it on your `PATH`, or call it by path. The examples below assume `ena-submit` resolves.

**What each command needs at runtime:**

| command | needs Java + Webin-CLI jar | needs network | needs credentials |
| --- | --- | --- | --- |
| `init` | no | no | no |
| `mag prepare` | no | yes (NCBI + ENA) | no |
| `reads`, `assembly`, `mag submit` | yes | yes (Webin) | yes |
| `status` | no | no | no |

Webin-CLI needs **Java 17+**, and the jar must be **1.8.12 or newer** (the password is passed via
`-passwordEnv`). Download it from
<https://github.com/enasequence/webin-cli/releases/latest>.

**Credentials** come from the environment (preferred) or `ena-submit.toml`:

```sh
$ export WEBIN_USERNAME=Webin-12345
$ export WEBIN_PASSWORD='…'
```

In fish:

```fish
$ set -x WEBIN_USERNAME Webin-12345
$ set -x WEBIN_PASSWORD '…'
```

Both halves are required — a username without a password is reported as missing credentials.

**Logging** goes to stderr at `info` by default, each line prefixed with a UTC timestamp (trimmed
from the sample output below for readability). Turn it up or down with `RUST_LOG`:

```sh
$ RUST_LOG=debug ena-submit assembly assemblies.tsv
$ RUST_LOG=warn   ena-submit assembly assemblies.tsv
```

---

## `init` — scaffold a project

Creates `ena-submit.toml` and a `templates/` directory in the **current** directory. Run it once,
in the directory you intend to work from.

```sh
$ mkdir my-submission && cd my-submission
$ ena-submit init
```

```
  wrote: /home/you/my-submission/ena-submit.toml
  wrote: /home/you/my-submission/templates/reads.tsv
  wrote: /home/you/my-submission/templates/assemblies.tsv
  wrote: /home/you/my-submission/templates/mag_samples.tsv
  wrote: /home/you/my-submission/templates/mag_assemblies.tsv
  wrote: /home/you/my-submission/templates/registered_mags.tsv
Initialised ena-submit project.
- ena-submit.toml: edit paths / set credentials via WEBIN_USERNAME & WEBIN_PASSWORD
- templates/: copy a template, fill it in, then run `ena-submit <reads|assembly|mag>`
```

`init` never overwrites. Re-running it in a directory that already has some of these files reports
what it skipped and writes only what is missing:

```sh
$ ena-submit init
```

```
  exists, skipped: /home/you/my-submission/ena-submit.toml
  exists, skipped: /home/you/my-submission/templates/reads.tsv
  …
```

### The config file

`ena-submit.toml` is loaded from the current working directory. Every key is optional, and the
environment always wins over the file:

```toml
# Which Webin service to target by default: "test" or "production".
default_environment = "test"

# Path to the Webin-CLI jar.
webin_cli_jar = "webin-cli.jar"

# Java executable used to run the jar.
java_bin = "java"

# Where Webin-CLI writes manifests, validation reports, and receipts.
output_dir = ".ena-submit/webin"
```

Precedence is: built-in defaults < `ena-submit.toml` < environment variables.

| config key | environment variable | default |
| --- | --- | --- |
| `webin_username` | `WEBIN_USERNAME` | — |
| `webin_password` | `WEBIN_PASSWORD` | — |
| `webin_cli_jar` | `WEBIN_CLI_JAR` | `webin-cli.jar` |
| `java_bin` | `JAVA_BIN` | `java` |
| `output_dir` | `ENA_SUBMIT_OUTPUT_DIR` | `.ena-submit/webin` |
| `default_environment` | *(none — CLI flags override)* | `test` |

You can keep credentials in the TOML file, but then keep the file out of version control; the
provided `.gitignore` already excludes it.

---

## Two flags every submission command shares

`reads`, `assembly`, and `mag submit` all take the same two pairs of mutually exclusive flags.

**What to do — defaults to validate-only:**

| flag | effect |
| --- | --- |
| `--validate` | validate the objects, upload nothing (**default**) |
| `--submit` | validate *and* really submit |

**Where to do it — defaults to the config's `default_environment`:**

| flag | effect |
| --- | --- |
| `--test` | the Webin **test** service; accessions are throwaway |
| `--production` | the **production** service; mints permanent accessions |

Passing both halves of a pair is a parse error:

```sh
$ ena-submit assembly assemblies.tsv --validate --submit
```

```
error: the argument '--validate' cannot be used with '--submit'
```

So the safe rehearsal is the bare command, and the real thing is explicit:

```sh
$ ena-submit assembly assemblies.tsv                       # validate, on whatever env config says
$ ena-submit assembly assemblies.tsv --test                # validate against test
$ ena-submit assembly assemblies.tsv --submit --test       # real submission, test service
$ ena-submit assembly assemblies.tsv --submit --production # the irreversible one
```

**`--input-dir DIR`** is the third shared flag. File paths inside a TSV are interpreted relative to
it (it becomes Webin-CLI's `-inputDir`), which lets you keep the sheet and the data apart:

```sh
$ ena-submit reads runs.tsv --input-dir /data/fastq
```

Without it, paths are relative to the current directory.

### Every input TSV must have data rows

A file with only a header is rejected, so an unfilled template fails loudly instead of reporting a
successful run that did nothing:

```
Error: ena-submit failed

Caused by:
    templates/reads.tsv: no data rows: the file has a header but nothing to process
```

Parsing is plain tab-splitting with no quote handling, blank lines are skipped, cells are trimmed,
and **header matching is case-insensitive** (`Study` and `study` are the same column).

---

## `reads` — submit sequencing runs

```
ena-submit reads <input.tsv> [--validate|--submit] [--test|--production] [--input-dir DIR]
```

One row per run. Start from `templates/reads.tsv`.

### Input columns

**Required:** `name`, `study`, `sample`, `platform`, `instrument`, `library_source`,
`library_selection`, `library_strategy`.

**Optional:** `library_name`, `insert_size` (non-negative integer), `description`.

**Data files — at least one column, and only one kind per row:** `fastq1`, `fastq2`, `bam`, `cram`.
`fastq2` may only appear alongside `fastq1`, and mixing FASTQ with BAM/CRAM in one row is rejected.

### Example — paired-end Illumina

`runs.tsv`:

```tsv
name	study	sample	platform	instrument	library_source	library_selection	library_strategy	library_name	insert_size	description	fastq1	fastq2
run_A	PRJEB00000	ERS0000000	ILLUMINA	Illumina NovaSeq 6000	GENOMIC	RANDOM	WGS	libA	350	Dog gut metagenome, replicate A	fastq/A_1.fastq.gz	fastq/A_2.fastq.gz
run_B	PRJEB00000	ERS0000001	ILLUMINA	Illumina NovaSeq 6000	GENOMIC	RANDOM	WGS	libB	350	Dog gut metagenome, replicate B	fastq/B_1.fastq.gz	fastq/B_2.fastq.gz
```

Rehearse first:

```sh
$ ena-submit reads runs.tsv --test
```

```
INFO reads submit input=runs.tsv mode="validate" env=test
INFO invoking webin-cli name=run_A context=reads mode="validate"
INFO invoking webin-cli name=run_B context=reads mode="validate"
validated: 2 ok, 0 failed
```

Then submit for real:

```sh
$ ena-submit reads runs.tsv --submit --production
```

```
INFO reads submit input=runs.tsv mode="submit" env=production
INFO invoking webin-cli name=run_A context=reads mode="submit"
INFO invoking webin-cli name=run_B context=reads mode="submit"
submitted: 2 ok, 0 failed
```

Each row becomes one Webin-CLI manifest under `output_dir`, e.g.:

```
STUDY	PRJEB00000
SAMPLE	ERS0000000
NAME	run_A
PLATFORM	ILLUMINA
INSTRUMENT	Illumina NovaSeq 6000
LIBRARY_SOURCE	GENOMIC
LIBRARY_SELECTION	RANDOM
LIBRARY_STRATEGY	WGS
LIBRARY_NAME	libA
INSERT_SIZE	350
DESCRIPTION	Dog gut metagenome, replicate A
FASTQ	fastq/A_1.fastq.gz
FASTQ	fastq/A_2.fastq.gz
```

### A single-file BAM row

```tsv
name	study	sample	platform	instrument	library_source	library_selection	library_strategy	bam
run_C	PRJEB00000	ERS0000002	ILLUMINA	Illumina NovaSeq 6000	GENOMIC	RANDOM	WGS	bam/C.bam
```

### When rows fail

Row-level problems are collected across the **whole file** and reported together — you fix them in
one pass rather than one per run:

```sh
$ ena-submit reads broken.tsv
```

```
Error: ena-submit failed

Caused by:
    broken.tsv: row 1: missing required 'name'
    row 2: 'insert_size' = 'about 350' is not a non-negative integer
    row 4: fastq2 given without fastq1
```

A missing *column* (as opposed to a missing value) is reported before any row is looked at:

```
Error: ena-submit failed

Caused by:
    broken.tsv: missing required column(s): study, platform
```

Nothing is sent to ENA when input fails to parse — the whole file is read and validated before the
first object is handed to Webin-CLI. Credentials and the toolchain are checked earlier still, so a
missing password is reported before the input is even opened.

---

## `assembly` — submit genome assemblies

```
ena-submit assembly <input.tsv> [--validate|--submit] [--test|--production] [--input-dir DIR]
```

One row per assembly, for isolates and anything else that is not a MAG. (MAGs have their own path;
see below.) Start from `templates/assemblies.tsv`.

### Input columns

**Required:** `assemblyname`, `study`, `sample`, `assembly_type`, `coverage`, `program`, `platform`.

**Optional:** `moleculetype`, `mingaplength` (integer), `description`, `run_ref`.

**Sequence files — at least one of `fasta` or `flatfile` is required:** `fasta`, `flatfile`, `agp`,
`chromosome_list`, `unlocalised_list`.

`assemblyname` is checked against ENA's rule before anything is submitted: at most 50 characters,
starting with a letter or digit, and containing only letters, digits, space, `_`, `#`, `-`, `.`.

### Example

`assemblies.tsv`:

```tsv
assemblyname	study	sample	assembly_type	coverage	program	platform	moleculetype	mingaplength	description	run_ref	fasta
asm_isolate1	PRJEB00000	ERS0000000	clone or isolate	30	SPAdes	ILLUMINA	genomic DNA	100	E. coli isolate 1	ERR0000000	asm/isolate1.fasta.gz
```

```sh
$ ena-submit assembly assemblies.tsv --test
```

```
INFO assembly submit input=assemblies.tsv mode="validate" env=test
INFO invoking webin-cli name=asm_isolate1 context=genome mode="validate"
validated: 1 ok, 0 failed
```

With the FASTA living somewhere else:

```sh
$ ena-submit assembly assemblies.tsv --submit --production --input-dir /scratch/assemblies
```

```
submitted: 1 ok, 0 failed
```

### An assembly with a chromosome list

```tsv
assemblyname	study	sample	assembly_type	coverage	program	platform	fasta	chromosome_list
asm_isolate2	PRJEB00000	ERS0000003	clone or isolate	120	Flye	OXFORD_NANOPORE	asm/iso2.fasta.gz	asm/iso2.chromosome_list.txt.gz
```

### Rejected names

```
Error: ena-submit failed

Caused by:
    assemblies.tsv: row 1: assemblyname 'bad/name' has invalid characters (allowed: letters, digits, space, _ # - .)
    row 2: assemblyname 'aaaaaa…' exceeds 50 characters
```

---

## `mag prepare` — fill the MAG sample sheet

```
ena-submit mag prepare <mags.tsv> [-o|--output <out.tsv>]
```

Default output: `mag_samples.filled.tsv`.

This command does **not** talk to Webin at all — no credentials, no Java, no jar. It needs outbound
HTTPS to `api.ncbi.nlm.nih.gov` and `www.ebi.ac.uk` (no API key).

### The problem it solves

ENA validates `scientific_name` against `tax_id`, so the two have to agree, and GTDB's names often
have no ENA equivalent — they exist only in GTDB (`CAG-269`, `UBA9414`) or have been renamed
(`Prevotella copri` → *Segatella copri*). Matching GTDB names against ENA fails on roughly a fifth
of a real sheet.

So `mag prepare` uses the **reference genome accession** as its key instead: it maps
`GTDBtk fastani Ref` to an NCBI species taxon id, confirms that id against ENA, and writes back both
the `tax_id` and ENA's own name for it.

### Input

Your sheet, with every checklist column you intend to submit already filled — plus three columns
that matter here:

| column | comes from GTDB-Tk's | example |
| --- | --- | --- |
| `tax_id` | *(left empty — this is what gets filled)* | |
| `scientific_name` | `classification` | `d__Bacteria;…;g__Phocaeicola;s__Phocaeicola vulgatus` |
| `GTDBtk fastani Ref` | `fastani_reference` | `GCF_000012825.1` |

Every other column is passed through byte-for-byte, so the sheet's shape is yours to decide. Start
from `templates/mag_samples.tsv`.

`mags.tsv` (abridged — a real sheet has many more checklist columns):

```tsv
sample_alias	tax_id	scientific_name	GTDBtk fastani Ref	sample derived from	environment (biome)	completeness score	contamination score
bin.1		d__Bacteria;p__Bacteroidota;c__Bacteroidia;o__Bacteroidales;f__Bacteroidaceae;g__Phocaeicola;s__Phocaeicola vulgatus	GCF_000012825.1	ERS1111111	human gut	95.5	2.1
bin.2		d__Bacteria;p__Bacteroidota;c__Bacteroidia;o__Bacteroidales;f__Bacteroidaceae;g__Prevotella;s__Prevotella copri_A	GCF_002224675.1	ERS1111112	human gut	91.0	3.4
bin.3		d__Bacteria;p__Actinomycetota;c__Actinomycetia;o__Actinomycetales;f__Micrococcaceae;g__Rothia;s__	0	ERS1111113	human gut	78.2	1.0
```

### Running it

```sh
$ ena-submit mag prepare mags.tsv -o mag_samples.filled.tsv
```

```
INFO mag prepare input=mags.tsv output=mag_samples.filled.tsv
INFO ENA taxonomy lookups issued lookups=3
INFO tax_id resolution complete filled=3 kept=0 rewritten=3 fallbacks=1
Rewrote 3 scientific_name cell(s) from their GTDB classification (1 resolved from the lineage, the rest from the reference accession)
Wrote completed sample sheet: mag_samples.filled.tsv
```

The output sheet has the same columns in the same order, with `tax_id` filled and
`scientific_name` replaced by ENA's name:

```tsv
sample_alias	tax_id	scientific_name	GTDBtk fastani Ref	sample derived from	environment (biome)	completeness score	contamination score
bin.1	821	Phocaeicola vulgatus	GCF_000012825.1	ERS1111111	human gut	95.5	2.1
bin.2	165179	Segatella copri	GCF_002224675.1	ERS1111112	human gut	91.0	3.4
bin.3	1936029	uncultured Rothia sp.	0	ERS1111113	human gut	78.2	1.0
```

### How each row is resolved

1. **Reference accession present** — mapped via the NCBI Datasets API to a species taxon id, then
   confirmed against ENA. A reference that is itself a *strain* (common for `GCF_` records) is
   climbed to its species first: a MAG is not the type strain it happens to resemble.

   | `GTDBtk fastani Ref` | GTDB calls it | ENA name written |
   | --- | --- | --- |
   | `GCF_000012825.1` | `Phocaeicola vulgatus` | `Phocaeicola vulgatus` (821) |
   | `GCF_002224675.1` | `Prevotella copri_A` | `Segatella copri` (165179) |
   | `GCA_900553985.1` | `CAG-269 sp900553985` | `uncultured Clostridium sp.` (59620) |
   | `GCA_018365895.1` | `UBA9414 sp018365895` | `Lachnospiraceae bacterium` (1898203) |

2. **No usable accession** — GTDB-Tk writes `0` when it made no species assignment (`n/a` and `na`
   are accepted too). These fall back to ENA *name* lookups, walking up the lineage until one
   resolves: the species, then `"<genus> sp."`, then `"uncultured <genus> sp."`, then
   `"<family> bacterium"`, and onward through order, class, and phylum. GTDB's polyphyly suffixes
   (`_A`, `_AQ`) are stripped for these lookups, since they exist nowhere in ENA.

   | classification | resolved by name to |
   | --- | --- |
   | `g__Rothia;s__` | `uncultured Rothia sp.` |
   | `g__Merdisoma;s__` (GTDB-only genus) | `Lachnospiraceae bacterium` |
   | `f__Eggerthellaceae;g__;s__` | `Eggerthellaceae bacterium` |

3. **Row already has a `tax_id`** — left untouched. Re-running `mag prepare` on its own output is a
   no-op, so it is safe to fix a handful of rows by hand and run it again.

### Stale accessions are flagged

An accession that is real but unknown to NCBI — suppressed or replaced since your GTDB-Tk run —
takes the same lineage fallback, but is warned about once per accession, because it means the sheet
is stale rather than merely unclassified:

```
WARN reference accession not found in NCBI; falling back to the classification accession="GCA_902363515.1" rows=44
```

### When rows cannot be resolved

Every unresolvable row is collected and reported together, with its row number:

```
Error: ena-submit failed

Caused by:
    mags.tsv: row 12: not a GTDB classification: 'Escherichia coli' (at 'Escherichia coli')
    row 41: reference genome resolves to NCBI taxon 562, which is not submittable to ENA (taxId 562)
    row 88: no reference accession, and no ENA taxon for any of 'Merdisoma sp.', 'uncultured Merdisoma sp.' (last: no ENA taxon matches scientific_name 'uncultured Merdisoma sp.')
```

Fix those rows (usually by supplying a `tax_id` yourself) and re-run.

### Then: register the samples

`mag prepare` produces a *sheet*, not a submission. Upload `mag_samples.filled.tsv` through the
Webin spreadsheet UI to obtain the `ERS…` sample accessions, then record the mapping in a small TSV
for the next step (start from `templates/registered_mags.tsv`):

`registered_mags.tsv`:

```tsv
bin_name	sample_accession
bin.1	ERS2222222
bin.2	ERS2222223
bin.3	ERS2222224
```

---

## `mag submit` — submit MAG assemblies

```
ena-submit mag submit <mags.tsv> --samples <registered_mags.tsv> \
    [--validate|--submit] [--test|--production] [--input-dir DIR]
```

`--samples` is required. Note that the `<mags.tsv>` here is the **assembly** sheet
(`templates/mag_assemblies.tsv`), not the sample sheet you fed to `mag prepare`.

### Input columns

**Required:** `bin_name`, `assemblyname`, `study`, `coverage`, `program`, `platform`, `fasta`.

**Optional:** `run_ref`, `description`, `topology` (`linear` — the default — or `circular`),
`chromosome_name`.

`sample` and `assembly_type` are *not* your columns: the sample comes from the `--samples` mapping,
and the assembly type is set to ENA's MAG value automatically.

`mag_assemblies.tsv`:

```tsv
bin_name	assemblyname	study	coverage	program	platform	fasta	run_ref	description	topology	chromosome_name
bin.1	MAG_bin.1	PRJEB00000	25	metaSPAdes	ILLUMINA	bins/bin.1.fasta.gz	ERR0000000	MAG derived from ERS1111111		
bin.2	MAG_bin.2	PRJEB00000	31	metaSPAdes	ILLUMINA	bins/bin.2.fasta.gz	ERR0000000	MAG derived from ERS1111112		
bin.3	MAG_bin.3	PRJEB00000	48	metaSPAdes	ILLUMINA	bins/bin.3.fasta.gz	ERR0000000	Closed single-contig MAG	circular	bin.3_chr
```

### Running it

```sh
$ ena-submit mag submit mag_assemblies.tsv --samples registered_mags.tsv --test
```

```
INFO mag submit input=mag_assemblies.tsv samples=registered_mags.tsv mode="validate" env=test
INFO invoking webin-cli name=MAG_bin.1 context=genome mode="validate"
INFO invoking webin-cli name=MAG_bin.2 context=genome mode="validate"
INFO single contig: submitting as chromosome bin=bin.3
INFO invoking webin-cli name=MAG_bin.3 context=genome mode="validate"
validated: 3 ok, 0 failed
```

For real, with the bins on another filesystem:

```sh
$ ena-submit mag submit mag_assemblies.tsv --samples registered_mags.tsv \
    --submit --production --input-dir /scratch/mags
```

```
submitted: 3 ok, 0 failed
```

### Single-contig bins become chromosomes

Before submitting each bin, `ena-submit` scans its FASTA (gzip-aware). If the bin is a **single
contig**, ENA wants it submitted as a chromosome, so a `CHROMOSOME_LIST` file is generated
automatically and added to the manifest. It is written next to the FASTA, named from the assembly:

```
bins/MAG_bin.3.chromosome_list.txt.gz
```

`topology` and `chromosome_name` only matter for these bins: `topology` records `linear` (default)
or `circular`, and `chromosome_name` names the chromosome, defaulting to the `bin_name`. For
multi-contig bins, leave both blank — they are ignored.

### Missing samples fail fast

Every bin must have a registered sample before *any* bin is sent. This is checked up front, so you
never end up with half a run submitted:

```sh
$ ena-submit mag submit mag_assemblies.tsv --samples registered_mags.tsv --submit
```

```
Error: ena-submit failed

Caused by:
    registered_mags.tsv: no registered sample_accession for bin(s): bin.7, bin.9
```

A duplicated `bin_name` in the mapping is likewise an error:

```
Error: ena-submit failed

Caused by:
    registered_mags.tsv: row 5: duplicate bin_name 'bin.2'
```

---

## `status` — read the submission history

```
ena-submit status
```

Every attempt — validate or submit, success or failure — is appended as one JSON line to
`.ena-submit/history.jsonl`, in the directory you ran from. `status` renders it in order:

```sh
$ ena-submit status
```

```
2026-07-28T09:12:44Z  test       validate  genome ok     asm_isolate1
2026-07-28T09:31:02Z  production submit    genome ok     asm_isolate1  [ANALYSIS=ERZ1234567]
2026-07-29T10:04:18Z  production submit    genome FAILED MAG_bin.2  (validation failed; see .ena-submit/webin/genome/MAG_bin.2/validate)
2026-07-29T10:04:51Z  production submit    genome ok     MAG_bin.3  [ANALYSIS=ERZ1234570]
```

The columns are: timestamp (UTC, RFC 3339), environment, mode, context, outcome, object name, then
any accessions in `[…]` and any failure reason in `(…)`.

With nothing recorded yet:

```sh
$ ena-submit status
```

```
No submissions recorded yet.
```

The file is append-only and never rewritten, so it is a durable record of what you sent. It is
plain JSONL, one object per line, if you want to query it yourself:

```sh
$ jq -r 'select(.outcome=="success" and .mode=="submit") | .accessions[].accession' \
    .ena-submit/history.jsonl
```

---

## Troubleshooting

**`missing Webin credentials`** — set `WEBIN_USERNAME` *and* `WEBIN_PASSWORD`. Both are needed;
either one alone counts as missing.

**`Webin-CLI jar not found at webin-cli.jar`** — download the jar and either drop it in the working
directory, set `webin_cli_jar` in `ena-submit.toml`, or export `WEBIN_CLI_JAR`.

**`Java 11 is too old; Webin-CLI needs Java 17+`** — install a newer JDK, or point `java_bin` /
`JAVA_BIN` at one you already have.

**`could not run Java ('java': …)`** — `java` is not on your `PATH`; set `java_bin`.

**Invalid credentials mid-run** — a rejected submission account aborts the entire run rather than
recording the same failure once per object. If objects were already processed, the run says so
before it stops:

```
aborted — submitted: 4 ok, 0 failed before stopping
Error: ena-submit failed

Caused by:
    Webin rejected the submission account: check WEBIN_USERNAME and WEBIN_PASSWORD (…)
```

That line matters after a `--submit` run: those four objects really did reach ENA and are in the
history.

**A run reports failures** — the exit status is non-zero and the count tells you how many:

```
validated: 5 ok, 2 failed
```

Per-object detail is in Webin-CLI's own reports under `output_dir` (`.ena-submit/webin` by
default), and the summary line is in the history via `ena-submit status`.

**Everything failed identically** — check the shared inputs first: study accession, sample
accessions, and whether you are pointed at test while referencing production accessions (or vice
versa). Test and production are separate worlds; accessions do not carry between them.
