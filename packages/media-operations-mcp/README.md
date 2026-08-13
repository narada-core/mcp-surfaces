# Media Operations MCP

Rust-native CLI and MCP stdio client for the private media API. Set `NARADA_MEDIA_API_URL` and `NARADA_MEDIA_API_TOKEN`.

Examples:

```text
narada-media youtube inspect https://youtu.be/VIDEO
narada-media youtube transcript https://youtu.be/VIDEO --wait
narada-media youtube clip https://youtu.be/VIDEO --start 30 --end 45 --wait --output .
narada-media x download https://x.com/user/status/ID --wait --output .
narada-media mcp
```
