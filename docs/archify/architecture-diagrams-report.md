# Architecture Diagrams Report

## Summary
- **Architecture diagram**: delivered, validated, visual-check evidence captured.
- **Workflow diagram**: delivered, validated, visual-check evidence captured.
- **Sequence diagram**: delivered, validated, visual-check evidence captured.
- **Dataflow diagram**: delivered, validated, visual-check evidence captured.
- **Lifecycle diagram**: delivered, validated, visual-check evidence captured.

---

## Architecture Diagram
- **Source**: `docs/archify/source/architecture.json`
- **Artifact**: `docs/archify/diagrams/architecture-prop-firm-engine.html`
- **Spec SHA-256**: `41f0539b69df6299225c7a88252577351d1b16eb5c9bf9ddca6632b1a865a1e9`
- **Artifact SHA-256**: `1dcaf13d00dc49acbda9e6140c016c03842f1d511a0b83b93bd98c0213356c94`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings

---

## Workflow Diagram
- **Source**: `docs/archify/source/workflow-candidate.json`
- **Artifact**: `docs/archify/diagrams/workflow-prop-firm-engine.html`
- **Spec SHA-256**: `b12109ea9712e3bcf62d28e0174b8a995ce6c466752e0286cc8d0bc1cbdeebc1`
- **Artifact SHA-256**: `26dba848aae45a9347fa68e4001d49f4e7e43b250f1b7c6b2c18c4833911dbbd`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings

---

## Sequence Diagram
- **Source**: `docs/archify/source/sequence-candidate.json`
- **Artifact**: `docs/archify/diagrams/sequence-evaluate-order.html`
- **Spec SHA-256**: `38043b445eac18dfd0cae349faaa376773127eda5ec3dc00135402f6cdab321d`
- **Artifact SHA-256**: `2be8ac41dcedcf3546dc99d150fd7584d573afdc90ede6a06611f13b3bd3c67e`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings

---

## Dataflow Diagram
- **Source**: `docs/archify/source/dataflow-candidate.json`
- **Artifact**: `docs/archify/diagrams/dataflow-prop-firm-engine.html`
- **Spec SHA-256**: `63a93359acb3e90954cdcfc63e6e05630d440fec6b1f1bc769a9e2ead8b9433b`
- **Artifact SHA-256**: `af1e8430826a0f9a82d7084672db8378939cccd6faa9f03f2f2b7e3cc3913f6b`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings

---

## Lifecycle Diagram
- **Source**: `docs/archify/source/lifecycle-candidate.json`
- **Artifact**: `docs/archify/diagrams/lifecycle-account.html`
- **Spec SHA-256**: `6d5765f423aab0301effb1dfacb61f303c67dbf668a4aca53362217d4524ef0b`
- **Artifact SHA-256**: `c7fdc96d960dc74d9713a5af12f510ccf515dd206153ab7e39183953b735c8b6`
- **Validation**: 9/9 showcase checks passed, 0 errors, 0 warnings

## Visual Check Evidence
Visual check evidence has been captured for all diagrams at standard desktop viewport sizes (1440×900, 1600×1000, 1920×1080, 2048×1320) in both light and dark themes.

## Notes
All diagrams reflect the D81 stateless compute service architecture where:
- No server-side account persistence exists (ADR-11)
- Account state flows in/out via request/response
- Input hash enables byte-for-byte replay
- Broker-reported equity is required for breach decisions (P1-5)
- Estimated equity can only trigger warnings at most
- Engine emits events to Event Store; platform AUD owns audit trail (D81/I-25)
