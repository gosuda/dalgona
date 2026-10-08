# dalgona settings that differ from dal

| key | dal default | dalgona default |
|---|---|---|
| `search_symbols` | `false` | `true` |
| `[tui].diagrams` | `false` | `false` |
| `edit_style` | `anchor` | `hashline` |
| `[guard].enabled` | `false` | `true` |
| `disabled_batteries` | none | `[]` |
| `experimental_batteries` | none | `[]` |
| `rule_sets.enabled` | none | all shipped sets |
| `[plugin.history].share` | none | `0.4` |
| `[plugin.plan].enabled` | none | `true` |
| `[plugin.web].provider` | none | `brave` |
| `[plugin.web].timeout_secs` | none | `30` |

Set `[tui].diagrams` to `true` to render supported fenced diagrams in transcript rows and ask previews. It defaults to `false`.

Battery tables are decoded strictly. Unknown keys, invalid types, and values outside documented ranges fail during product construction. `disabled_batteries` is the product-level switch for every battery; supported per-battery `enabled` keys are documented with their battery.
