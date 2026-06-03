# mc-channels

Channel abstraction layer for mavis-claw.

**Status:** Stub — not yet implemented.

## Planned Contents

- `Channel` trait (start, send, stop)
- `MessageHandler` trait (on_message callback)
- `IncomingMessage`, `OutgoingMessage`, `MessageContent`
- Implementations: Feishu (webhook + OpenAPI), WeChat (enterprise), QQ (OneBot)
