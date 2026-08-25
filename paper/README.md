# Orbit-Transfer · Paper (IEEEtran)

LaTeX source for the Orbit-Transfer conference paper
(`paper.tex` + `references.bib`).

## Building

Requires a LaTeX distribution with the **IEEEtran** document class
(TeX Live, MiKTeX, or Overleaf all ship it).

```sh
# TeX Live / MiKTeX (three passes resolve references and citations):
pdflatex paper.tex
bibtex paper
pdflatex paper.tex
pdflatex paper.tex
```

Or import `paper.tex` and `references.bib` into
[Overleaf](https://overleaf.com) and set the compiler to **pdfLaTeX**.

## Notes

- The table numbers come from the aggregation bench
  (`crates/orbit-relay/tests/agg_bench.rs`, run in release) and the throttle
  calibration (`crates/orbit-relay/tests/throttle_cal.rs`), five runs each on
  a Windows 11 loopback testbed.
- The citations use BibTeX keys; both `\cite`-in-text and the handwritten
  `thebibliography` list are kept in sync. If you prefer auto-generated
  references, remove the `thebibliography` environment and uncomment the
  `\bibliography{references}` lines.