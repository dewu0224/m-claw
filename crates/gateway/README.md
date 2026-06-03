# mc-gateway

HTTP/WebSocket gateway for mavis-claw.

**Status:** Stub — not yet implemented.

## Planned Contents

- axum-based HTTP server
- Webhook routes: `/webhook/feishu`, `/webhook/wechat`, `/webhook/qq`
- OpenAI-compatible API: `POST /v1/chat/completions`
- Health/status endpoints: `GET /health`, `GET /api/status`
- Manual chat endpoint: `POST /api/chat`
