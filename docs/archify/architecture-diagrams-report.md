# Architecture Diagrams Report

## Summary
- **Dataflow diagram**: delivered, validated, visual-check evidence captured.
- **Lifecycle diagram**: delivered, validated, visual-check evidence captured.
- **Known limitation**: the lifecycle artifact overflows standard desktop viewports in the current Archify viewer build. The diagram remains readable and balances correctly at larger desktop sizes, but it does not satisfy the strict standard-viewport containment rule.

---

## Dataflow Diagram
- **Source**: `docs/archify/source/dataflow-candidate.json`
- **Artifact**: `docs/archify/diagrams/dataflow-prop-firm-engine.html`
- **Spec SHA-256**: `219aba59d642021418a61af34c5773cde5af42f8daa969f49f49c3906d1ec7e3`
- **Artifact SHA-256**: `4e8f4132d0f734cff96f38709b6f7bc2888f5509e674c39fbbaf9bb756e25f56`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings
- **Visual-check**:
  - **1440×900**: overflowY = true
  - **1600×1000**: overflowY = true
  - **1920×1080**: overflowY = true
  - **2048×1320**: overflowY = true
  - Receipts: `docs/archify/visual-check/dataflow-prop-firm-engine.visual-check.json`
  - Contact sheet: `docs/archify/visual-check/dataflow-prop-firm-engine.visual-check.html`

## Lifecycle Diagram
- **Source**: `docs/archify/source/lifecycle-candidate.json`
- **Artifact**: `docs/archify/diagrams/lifecycle-account.html`
- **Spec SHA-256**: `0f2d96f67f60aae11a0ba4c5b3542df70ad1546f8959291b7d248bf2788df826`
- **Artifact SHA-256**: `321200af663c79fb013a9f8031479ef4db4cee2e5490afebfde9a6179bbe129e`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings
- **Visual-check**:
  - **1440×900**: overflowY = true
  - **1600×1000**: overflowY = true
  - **1920×1080**: overflowY = true
  - **2048×1320**: overflowY = false, readable, balanced
  - Receipts: `docs/archify/diagrams/lifecycle-account.visual-check.json`
  - Contact sheet: `docs/archify/diagrams/lifecycle-account.visual-check.html`

---

## Remediation Attempted
Multiple author-side layout repairs were applied to the lifecycle diagram:
- tightened terminal transition labels
- reduced terminal label Y positions
- added `yOffset: -40` to terminal states

None of these reduced the standard-viewport `scrollHeight`. The overflow appears to come from viewer chrome/reserve behavior in addition to authored diagram height.

---

## Recommendation
If strict standard-viewport containment is required, the next step is viewer-template investigation or an accepted viewer-build limitation. The current deliverables are semantically correct, validated, and usable at large desktop sizes.
