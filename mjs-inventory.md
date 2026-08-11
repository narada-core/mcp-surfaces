# MJS file inventory

This register was reviewed on 2026-08-08 against revision
eeb464249eda123f844997481aaaf26d3b7a5880. It describes the current
`mcp-surfaces` repository, not external Narada or Site repositories.

## Current repository state

No `.mjs` files are present in the repository scan, and Git reports no
deleted tracked `.mjs` paths.

Commands:

```powershell
rg --files -uu -g '*.mjs'
git ls-files --deleted -- '*.mjs'
```

Both commands are expected to produce no paths for this repository. If a
future migration introduces an `.mjs` file, update this register in the same
change and record whether the file is authored source, generated output, or an
external-repository reference.
