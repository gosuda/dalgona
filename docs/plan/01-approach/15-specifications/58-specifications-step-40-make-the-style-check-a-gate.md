#### Specifications step 40. Make the style check a gate
Anchor: Edsger W. Dijkstra

- `justfile:121`: change the line to `verify: fmt-check vet lint test vuln hashline-model spec-style web-check cf-check`.
- `.github/workflows/ci.yml` needs no change: it already installs `uv` (`ci.yml:43-46`) and runs `just verify`.
- Proof: `just spec-style` prints `style findings: 0`, and `just verify` passes.
- Lands after: Specifications steps 2 to 35, and tests-and-docs Steps 12 to 18.
- From this step on, every step of this plan that adds a sentence to `docs/spec` keeps `just spec-style` at zero findings.

