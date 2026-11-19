#### Web step 36. Notifications

- Session events. When a request arrives, or a turn ends with `completed`, `failed` or `interrupted`, call `notifyAttention(kind)`.
- Desktop notification. Shown when `document.visibilityState === "hidden"` and `Notification.permission === "granted"`: `new Notification(text, { body: threadTitle, tag: threadId })`. Texts: `Waiting for your reply`, the state words `Done` and `Failed` (Step 10).
- Settings row.
  - Button `Allow desktop notifications` calls `Notification.requestPermission()`.
  - State text: `Desktop notifications are on`, `Desktop notifications are blocked in this browser`, or `Desktop notifications are off`.
- Test. `session.test.ts` with stubbed `Notification` and `visibilityState`:
  - `notifies on a waiting approval only when hidden and granted`
  - `never asks for permission without a click`
  - Fails before: there are no notifications.
- Spec (`docs/spec/web.md`). New `### Notifications`.
- Lands after: Steps 4, 10 and 23.

Anchor: Kenya Hara.

