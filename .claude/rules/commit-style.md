---
description: Conventional-commit subject + why-focused body + mandatory Claude co-author trailer
---

Commit messages use `feat:` / `fix:` / `refactor:` / `chore:` / `test:` / `docs:` with a single-sentence subject and a body that explains *why* (not what — the diff shows what). Bundle related changes into one commit; do not split for split's sake.

Every commit must end with the co-author trailer:

```
Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

Never `git push --force` or `git commit --no-verify`. Always create new commits rather than amending published ones.
