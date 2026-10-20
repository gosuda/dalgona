#!/bin/sh
# Canned stream-json transcript for the gemini_cli driver tests: consumes
# stdin, emits two text deltas and a successful result, then exits 0.
# Speaks the pinned stream-json shapes (init/message/result); argv ignored.
cat > /dev/null
printf '%s\n' \
'{"type":"init","timestamp":"2026-10-16T00:00:00Z","session_id":"s1","model":"gemini-3-pro"}' \
'{"type":"message","timestamp":"2026-10-16T00:00:00Z","role":"assistant","content":"Hello","delta":true}' \
'{"type":"message","timestamp":"2026-10-16T00:00:00Z","role":"assistant","content":" world","delta":true}' \
'{"type":"result","timestamp":"2026-10-16T00:00:00Z","status":"success","stats":{"total_tokens":17,"input_tokens":10,"output_tokens":7,"cached":0,"input":10,"duration_ms":5,"tool_calls":0,"models":{}}}'
