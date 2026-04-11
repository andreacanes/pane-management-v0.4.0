---
description: Rust companion DTOs and Kotlin @Serializable DTOs must stay wire-compatible
paths: ["workspace-resume/src-tauri/src/companion/models.rs", "pane-management-mobile/app/src/main/java/com/andreacanes/panemgmt/data/models/Dtos.kt"]
---

Every change to `workspace-resume/src-tauri/src/companion/models.rs` needs a matching change in `pane-management-mobile/app/src/main/java/com/andreacanes/panemgmt/data/models/Dtos.kt`.

- Adding a field is safe (Kotlin JSON has `ignoreUnknownKeys = true`; make Rust field `Option` with `#[serde(skip_serializing_if = "Option::is_none")]`)
- Renaming a field silently breaks decoding on the Kotlin side — no error surfaces
- Tagged enums on both sides must share the discriminator: Rust `#[serde(tag = "type", rename_all = "snake_case")]` matches Kotlin `Json { classDiscriminator = "type" }` with `@SerialName("snake_case")` per variant
- Snake-case wire names in Kotlin use `@SerialName("snake_case")` on camelCase Kotlin properties

Full DTO mirror table + examples: see `pane-management-mobile/CLAUDE.md`.
