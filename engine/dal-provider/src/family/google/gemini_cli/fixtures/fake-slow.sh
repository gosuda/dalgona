#!/bin/sh
# Slow fixture for the gemini_cli cancellation test: sleeps 30s so the test
# can prove that dropping the stream kills the child instead of waiting out
# its natural exit. Argv ignored.
sleep 30
