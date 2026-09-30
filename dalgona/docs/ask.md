# Ask

The `ask` battery registers a rich `ask` tool and an ask prompt section through the Rust extension API. It uses the shared question service and preserves typed choice and text answers. If no controller can answer, the request fails closed rather than inventing an answer.
