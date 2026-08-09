# Native MCP surfaces

This package owns the shared Rust stdio executable for MCP surfaces. It is
invoked with `--surface-id <id>` and keeps the MCP protocol boundary in Rust;
surface implementations are added as explicit modules rather than launching a
JavaScript runtime.
